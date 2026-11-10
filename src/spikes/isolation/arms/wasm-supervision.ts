import { Effect, Schema, Scope } from "effect"
import { nowMs, ProbeError, readTextFile, withTimeout } from "../exec.ts"
import { WorkerRequest, WorkerResponse } from "../wasm-protocol.ts"

/**
 * Minimal WASM-arm supervision surface: one permission-less worker per agent,
 * one WebAssembly instance per worker, a sequential request channel, and
 * termination. Measurement probes live in `wasm-instance.ts`.
 */
const workerUrl = new URL("../agent-wasm-worker.ts", import.meta.url)

export const WASM_SUPERVISION_FILES: ReadonlyArray<string> = [
  "src/spikes/isolation/arms/wasm-supervision.ts",
  "src/spikes/isolation/agent-wasm-worker.ts",
  "src/spikes/isolation/wasm-protocol.ts",
]

const decode = (data: unknown): Effect.Effect<WorkerResponse, ProbeError> =>
  Effect.try({
    try: () => Schema.decodeUnknownSync(WorkerResponse)(data),
    catch: (error) =>
      new ProbeError({ operation: "decode-worker-response", detail: String(error) }),
  })

/** One-shot listener: requests are strictly sequential per instance. */
const nextMessage = (port: MessagePort): Effect.Effect<WorkerResponse, ProbeError> =>
  Effect.callback<unknown, ProbeError>((resume) => {
    const onMessage = (event: MessageEvent) => {
      port.removeEventListener("message", onMessage)
      resume(Effect.succeed(event.data))
    }
    port.addEventListener("message", onMessage)
    return Effect.sync(() => port.removeEventListener("message", onMessage))
  }).pipe(Effect.flatMap(decode))

export interface WasmAgent {
  readonly compileMs: number
  readonly instantiateMs: number
  readonly readyMs: number
  readonly imports: number
  readonly memoryPages: number
  readonly post: (request: WorkerRequest) => Effect.Effect<void, ProbeError>
  readonly request: (request: WorkerRequest) => Effect.Effect<WorkerResponse, ProbeError>
  readonly terminate: Effect.Effect<void>
}

export const startAgent = (): Effect.Effect<WasmAgent, ProbeError, Scope.Scope> =>
  Effect.gen(function* () {
    const startedAt = nowMs()
    const channel = new MessageChannel()
    const worker = yield* Effect.acquireRelease(
      Effect.sync(() => {
        const handle = new Worker(workerUrl, { type: "module", deno: { permissions: "none" } })
        handle.postMessage(channel.port2, [channel.port2])
        return handle
      }),
      (handle) => Effect.sync(() => handle.terminate()),
    )
    const port = channel.port1
    port.start()
    const ready = yield* withTimeout(nextMessage(port), 8_000, "wasm ready")
    if (ready.kind !== "ready") {
      return yield* Effect.fail(
        new ProbeError({ operation: "wasm ready", detail: `unexpected ${ready.kind}` }),
      )
    }
    return {
      compileMs: ready.compileMs,
      instantiateMs: ready.instantiateMs,
      readyMs: nowMs() - startedAt,
      imports: ready.imports,
      memoryPages: ready.memoryPages,
      post: (request) => Effect.sync(() => port.postMessage(request)),
      request: (request) =>
        Effect.gen(function* () {
          yield* Effect.sync(() => port.postMessage(request))
          return yield* withTimeout(nextMessage(port), 15_000, `wasm ${request.kind}`)
        }),
      terminate: Effect.sync(() => {
        worker.terminate()
        port.close()
      }),
    }
  })

export const processRssBytes = (): Effect.Effect<number> =>
  readTextFile("/proc/self/status").pipe(
    Effect.map((text) => {
      const match = /^VmRSS:\s+(\d+)\s+kB$/mu.exec(text ?? "")
      return match?.[1] === undefined ? 0 : Number(match[1]) * 1024
    }),
    Effect.catch(() => Effect.succeed(0)),
  )
