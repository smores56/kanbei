/**
 * S3 WASM-arm agent: one worker hosts exactly one WebAssembly instance.
 *
 * The worker is created with no Deno runtime permissions, so the only reachable
 * authority from inside the instance is the single `ping` host import. The
 * supervisor talks to it over a dedicated `MessagePort` so this file needs no
 * worker-global typings.
 */
import { Schema } from "effect"
import { buildAgentModule } from "./wasm-module.ts"
import { WASM_MAX_PAGES } from "./protocol.ts"
import { WorkerRequest } from "./wasm-protocol.ts"

const bytes = buildAgentModule(WASM_MAX_PAGES)
const compileStarted = performance.now()
const module = new WebAssembly.Module(bytes)
const compileMs = performance.now() - compileStarted

let hostCalls = 0
const hostPing = (value: number): number => {
  hostCalls += 1
  return value + 1
}

const instantiateStarted = performance.now()
const instance = new WebAssembly.Instance(module, { kanbei: { ping: hostPing } })
const instantiateMs = performance.now() - instantiateStarted

const exportedFunction = (name: string): (argument: number) => number => {
  const value: unknown = instance.exports[name]
  if (typeof value !== "function") throw new Error(`missing wasm export ${name}`)
  return (argument: number) => Number(Reflect.apply(value, undefined, [argument]))
}

const spin = exportedFunction("spin")
const callHost = exportedFunction("callHost")
const alloc = exportedFunction("alloc")
const boom = exportedFunction("boom")
const hang = exportedFunction("hang")

const memoryPages = (): number => {
  const memory: unknown = instance.exports.memory
  if (!(memory instanceof WebAssembly.Memory)) throw new Error("missing wasm memory")
  return memory.buffer.byteLength / 65536
}

const handle = (port: MessagePort, request: WorkerRequest): void => {
  switch (request.kind) {
    case "ping": {
      port.postMessage({ kind: "pong" })
      return
    }
    case "spin": {
      const startedAt = performance.now()
      const checksum = spin(request.iterations)
      port.postMessage({
        kind: "computed",
        checksum,
        wallMs: performance.now() - startedAt,
        hostCalls,
      })
      return
    }
    case "hostCalls": {
      const startedAt = performance.now()
      const checksum = callHost(request.count)
      port.postMessage({
        kind: "computed",
        checksum,
        wallMs: performance.now() - startedAt,
        hostCalls,
      })
      return
    }
    case "boom": {
      try {
        boom(0)
        port.postMessage({ kind: "trapped", message: "no trap" })
      } catch (error) {
        port.postMessage({
          kind: "trapped",
          message: error instanceof WebAssembly.RuntimeError
            ? `RuntimeError: ${error.message}`
            : String(error),
        })
      }
      return
    }
    case "alloc": {
      const previousPages = alloc(request.pages)
      port.postMessage({ kind: "allocated", previousPages, memoryPages: memoryPages() })
      return
    }
    case "denoProbe": {
      try {
        const child = new Deno.Command("true", { stdin: "null", stdout: "null" }).spawn()
        void child.status
        port.postMessage({ kind: "denoDenied", denied: false })
      } catch {
        port.postMessage({ kind: "denoDenied", denied: true })
      }
      return
    }
    case "hang": {
      hang(0)
      return
    }
    default: {
      port.postMessage({ kind: "failed", message: "unknown request" })
    }
  }
}

const serve = (port: MessagePort): void => {
  port.addEventListener("message", (event: MessageEvent) => {
    try {
      handle(port, Schema.decodeUnknownSync(WorkerRequest)(event.data))
    } catch (error) {
      port.postMessage({ kind: "failed", message: String(error) })
    }
  })
  port.start()
  port.postMessage({
    kind: "ready",
    compileMs,
    instantiateMs,
    memoryPages: memoryPages(),
    imports: WebAssembly.Module.imports(module).length,
  })
}

addEventListener("message", (event) => {
  if (!(event instanceof MessageEvent)) return
  const port: unknown = event.data
  if (port instanceof MessagePort) serve(port)
})
