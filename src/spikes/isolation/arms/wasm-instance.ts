import { Effect, Result } from "effect"
import { nowMs, ProbeError, readTextFile } from "../exec.ts"
import {
  COLD_START_SAMPLES,
  CPU_WORK_ITERATIONS,
  IPC_ROUND_TRIPS,
  TARGET_CONCURRENCY,
  WASM_DENSITY_STEPS,
  WASM_MAX_PAGES,
} from "../protocol.ts"
import { round, summarize } from "../stats.ts"
import { processRssBytes, startAgent, WASM_SUPERVISION_FILES } from "./wasm-supervision.ts"
import type {
  ArmMetrics,
  ArmResult,
  CheckId,
  ContainmentCheck,
  DensityPoint,
  LatencySummary,
  Operability,
} from "../types.ts"

export const WASM_ARM_FILES = WASM_SUPERVISION_FILES

const range = (count: number): ReadonlyArray<number> => Array.from({ length: count }, (_v, i) => i)

const mib = (bytes: number): string => (bytes / 1024 / 1024).toFixed(1)

/** On-CPU nanoseconds from /proc/self/schedstat, scaled to microseconds. */
const processCpuUsec = (): Effect.Effect<number> =>
  readTextFile("/proc/self/schedstat").pipe(
    Effect.map((text) => Math.round(Number((text ?? "0").split(" ")[0] ?? "0") / 1_000)),
    Effect.catch(() => Effect.succeed(0)),
  )

const coldStartProbe = () =>
  Effect.gen(function* () {
    yield* Effect.scoped(startAgent())
    const samples = yield* Effect.forEach(
      range(COLD_START_SAMPLES),
      () =>
        Effect.scoped(Effect.gen(function* () {
          const agent = yield* startAgent()
          return agent.readyMs
        })),
      { concurrency: 1 },
    )
    return summarize(samples)
  })

const cpuProbe = () =>
  Effect.scoped(
    Effect.gen(function* () {
      const agent = yield* startAgent()
      const before = yield* processCpuUsec()
      const response = yield* agent.request({ kind: "spin", iterations: CPU_WORK_ITERATIONS })
      const used = (yield* processCpuUsec()) - before
      if (response.kind !== "computed") {
        return yield* Effect.fail(
          new ProbeError({ operation: "wasm cpu", detail: `unexpected ${response.kind}` }),
        )
      }
      return {
        cpuMs: round(used / 1000, 2),
        wallMs: round(response.wallMs, 2),
        hostCalls: response.hostCalls,
      }
    }),
  )

const ipcProbe = () =>
  Effect.scoped(
    Effect.gen(function* () {
      const agent = yield* startAgent()
      const samples = yield* Effect.forEach(
        range(IPC_ROUND_TRIPS),
        () =>
          Effect.gen(function* () {
            const startedAt = nowMs()
            yield* agent.request({ kind: "ping" })
            return nowMs() - startedAt
          }),
        { concurrency: 1 },
      )
      return summarize(samples)
    }),
  )

const skippedPoint = (agents: number): DensityPoint => ({
  agents,
  outcome: "skipped",
  readyMs: 0,
  totalRssBytes: 0,
  perAgentRssBytes: 0,
  failures: 0,
})

const densityPoint = (agents: number) =>
  Effect.gen(function* () {
    const baseline = yield* processRssBytes()
    return yield* Effect.scoped(
      Effect.gen(function* () {
        const startedAt = nowMs()
        const results = yield* Effect.forEach(
          range(agents),
          () => Effect.result(startAgent()),
          { concurrency: agents },
        )
        const readyMs = nowMs() - startedAt
        const live = results.flatMap((result) => (Result.isSuccess(result) ? [result.success] : []))
        const after = yield* processRssBytes()
        const delta = Math.max(0, after - baseline)
        yield* Effect.forEach(live, (agent) => agent.terminate, {
          concurrency: agents,
          discard: true,
        })
        return {
          agents,
          outcome: live.length === agents ? "ok" : "partial",
          readyMs: round(readyMs, 1),
          totalRssBytes: delta,
          perAgentRssBytes: live.length === 0 ? 0 : Math.round(delta / live.length),
          failures: agents - live.length,
        } satisfies DensityPoint
      }),
    )
  })

const densityProbe = () =>
  Effect.gen(function* () {
    const points = yield* Effect.forEach(
      WASM_DENSITY_STEPS,
      (agents) =>
        densityPoint(agents).pipe(
          Effect.catch(() => Effect.succeed(skippedPoint(agents))),
        ),
      { concurrency: 1 },
    )
    yield* Effect.sleep("200 millis")
    return points
  })

const crashProbe = () =>
  Effect.scoped(
    Effect.gen(function* () {
      const agents = yield* Effect.forEach(range(2), () => startAgent(), { concurrency: 2 })
      const victim = agents[0]
      const peer = agents[1]
      if (victim === undefined || peer === undefined) {
        return yield* Effect.fail(
          new ProbeError({ operation: "wasm crash", detail: "missing agent" }),
        )
      }
      const trapped = yield* victim.request({ kind: "boom" })
      const peerReply = yield* peer.request({ kind: "ping" })
      const contained = trapped.kind === "trapped" && peerReply.kind === "pong"
      return {
        id: "crash-isolation" as const,
        status: contained ? ("pass" as const) : ("fail" as const),
        detail: `unreachable trap surfaced as ${
          trapped.kind === "trapped" ? trapped.message : trapped.kind
        }; peer instance answered; supervisor process stayed up`,
      }
    }),
  )

const hangProbe = () =>
  Effect.scoped(
    Effect.gen(function* () {
      const agents = yield* Effect.forEach(range(2), () => startAgent(), { concurrency: 2 })
      const victim = agents[0]
      const peer = agents[1]
      if (victim === undefined || peer === undefined) {
        return yield* Effect.fail(
          new ProbeError({ operation: "wasm hang", detail: "missing agent" }),
        )
      }
      yield* victim.post({ kind: "hang" })
      yield* Effect.sleep("300 millis")
      const killedAt = nowMs()
      yield* victim.terminate
      const killMs = nowMs() - killedAt
      const peerReply = yield* peer.request({ kind: "ping" })
      return {
        id: "hang-containment" as const,
        status: peerReply.kind === "pong" ? ("pass" as const) : ("fail" as const),
        detail: `infinite wasm loop blocked its worker until the supervisor terminated it in ${
          killMs.toFixed(1)
        } ms; peer instance answered (instances cannot be preempted individually)`,
      }
    }),
  )

const oomProbe = () =>
  Effect.scoped(
    Effect.gen(function* () {
      const agent = yield* startAgent()
      const over = yield* agent.request({ kind: "alloc", pages: WASM_MAX_PAGES * 4 })
      const stillAlive = yield* agent.request({ kind: "ping" })
      const capped = over.kind === "allocated" && over.memoryPages <= WASM_MAX_PAGES &&
        stillAlive.kind === "pong"
      return {
        id: "oom-containment" as const,
        status: capped ? ("pass" as const) : ("fail" as const),
        detail: over.kind === "allocated"
          ? `requested ${
            WASM_MAX_PAGES * 4
          } pages, memory.grow returned ${over.previousPages}, instance stayed at ${over.memoryPages} pages and kept serving requests`
          : `unexpected ${over.kind}`,
      }
    }),
  )

const forkBombProbe = () =>
  Effect.scoped(
    Effect.gen(function* () {
      const agent = yield* startAgent()
      const probed = yield* agent.request({ kind: "denoProbe" })
      const denied = probed.kind === "denoDenied" && probed.denied
      const noSyscalls = agent.imports === 1
      return {
        id: "fork-bomb-containment" as const,
        status: denied && noSyscalls ? ("not-applicable" as const) : ("fail" as const),
        detail:
          `module imports ${agent.imports} host function(s) and the worker runtime has no permissions (process spawn denied=${denied}); a fork bomb has no reachable syscall`,
      }
    }),
  )

const cleanupProbe = () =>
  Effect.gen(function* () {
    const baseline = yield* processRssBytes()
    const peak = yield* Effect.scoped(
      Effect.gen(function* () {
        const agents = yield* Effect.forEach(range(8), () => startAgent(), { concurrency: 8 })
        const during = yield* processRssBytes()
        yield* Effect.forEach(agents, (agent) => agent.terminate, { concurrency: 8, discard: true })
        return Math.max(0, during - baseline)
      }),
    )
    yield* Effect.sleep("500 millis")
    const residual = Math.max(0, (yield* processRssBytes()) - baseline)
    const fresh = yield* Effect.scoped(
      Effect.gen(function* () {
        const agent = yield* startAgent()
        const reply = yield* agent.request({ kind: "ping" })
        return reply.kind === "pong"
      }),
    )
    return {
      id: "cleanup-after-kill" as const,
      status: fresh ? ("pass" as const) : ("fail" as const),
      detail:
        `8 workers terminated and the supervisor created a fresh instance afterwards; RSS peak ${
          mib(peak)
        } MiB, residual ${mib(residual)} MiB not returned to the OS`,
    }
  })

const guard = <A, R>(
  id: CheckId,
  effect: Effect.Effect<A, ProbeError, R>,
  render: (value: A) => ContainmentCheck,
): Effect.Effect<ContainmentCheck, never, R> =>
  Effect.gen(function* () {
    yield* Effect.logInfo(`check ${id}: running`)
    const result = yield* Effect.result(effect)
    if (Result.isSuccess(result)) {
      const rendered = render(result.success)
      yield* Effect.logInfo(`check ${id}: ${rendered.status}`)
      return rendered
    }
    yield* Effect.logInfo(`check ${id}: failed`)
    return { id, status: "fail" as const, detail: result.failure.detail }
  })

const operability = (supervisorLoc: number): Operability => ({
  hostDependencies: ["deno (V8 core wasm)"],
  requiresRoot: false,
  supervisorLoc,
  packaging: "One worker and one WebAssembly instance per agent; module bytes are built in-process",
  debuggability:
    "No process or thread introspection; traps surface as RuntimeError, hangs need worker termination",
  limitations: [
    "No WASI runtime on the host, so filesystem, network, and process capability are unreachable from an instance",
    "Instances share the worker thread: no CPU preemption or fuel metering, so a hang is only bounded by terminating the worker",
    "Worker RSS is not attributable per instance; density RSS is a process-level delta",
    'Worker permission scoping (`deno: { permissions: "none" }`) needs the unstable `--unstable-worker-options` flag in Deno 2.9',
  ],
})

export const measureWasmArm = (supervisorLoc: number): Effect.Effect<ArmResult> =>
  Effect.gen(function* () {
    const coldStart = yield* Effect.result(coldStartProbe())
    const cpu = yield* Effect.result(cpuProbe())
    const ipc = yield* Effect.result(ipcProbe())
    const density = yield* Effect.result(densityProbe())
    yield* Effect.logInfo(`wasm-instance cold start: ${describe(coldStart, summarizeLatency)}`)
    yield* Effect.logInfo(`wasm-instance cpu: ${describe(cpu, (value) => `${value.cpuMs} ms`)}`)
    yield* Effect.logInfo(`wasm-instance ipc: ${describe(ipc, summarizeLatency)}`)
    yield* Effect.logInfo(`wasm-instance density: ${describe(density, summarizeDensity)}`)

    const checks = yield* Effect.all([
      guard("crash-isolation", crashProbe(), (check) => check),
      guard("hang-containment", hangProbe(), (check) => check),
      guard("oom-containment", oomProbe(), (check) => check),
      guard("fork-bomb-containment", forkBombProbe(), (check) => check),
      guard("cleanup-after-kill", cleanupProbe(), (check) => check),
      Effect.succeed<ContainmentCheck>({
        id: "orphan-reaping",
        status: "not-applicable",
        detail: "an instance cannot create processes; there is no orphan to reap",
      }),
      Effect.succeed<ContainmentCheck>(
        Result.isSuccess(ipc)
          ? {
            id: "ipc-round-trip",
            status: "pass",
            detail: `postMessage ping/pong completed ${IPC_ROUND_TRIPS} times, p95 ${
              round(ipc.success.p95Ms, 2)
            } ms`,
          }
          : { id: "ipc-round-trip", status: "fail", detail: ipc.failure.detail },
      ),
      Effect.succeed<ContainmentCheck>({
        id: "resource-ceilings",
        status: "fail",
        detail:
          `linear memory is capped at ${WASM_MAX_PAGES} pages and growth is refused, but the runtime exposes no CPU-time or wall-clock ceiling and no fuel metering`,
      }),
      Effect.succeed<ContainmentCheck>({
        id: "hosts-agent-workload",
        status: "fail",
        detail:
          "the reference agent needs filesystem, network, and process capability; this arm exposes one integer host import and no WASI runtime, so it cannot host the LANG-1 execution surface",
      }),
    ], { concurrency: 1 })

    const targetPoint = Result.isSuccess(density)
      ? density.success.find((point) => point.agents === TARGET_CONCURRENCY)
      : undefined

    if (
      Result.isFailure(coldStart) || Result.isFailure(cpu) || Result.isFailure(ipc) ||
      Result.isFailure(density)
    ) {
      return {
        arm: "wasm-instance",
        status: "inconclusive",
        scope:
          "A WebAssembly instance per permission-less worker; containment, cold start, CPU, IPC, and density were attempted, but the metric probe failed",
        checks: [...checks],
        operability: operability(supervisorLoc),
        notes: ["metric probe failed; the containment checks above are the recorded evidence"],
      }
    }

    const metrics: ArmMetrics = {
      coldStartMs: coldStart.success,
      cpuWorkMs: cpu.success.cpuMs,
      cpuWorkIterations: CPU_WORK_ITERATIONS,
      ipcRoundTripMs: ipc.success,
      rssPerAgentBytes: targetPoint?.perAgentRssBytes ?? 0,
      density: [...density.success],
    }

    return {
      arm: "wasm-instance",
      status: "measured",
      scope:
        "Crash, hang, OOM, cleanup, and IPC describe one WebAssembly instance inside one permission-less Deno worker; fork-bomb and orphan checks are not applicable because an instance cannot create processes, and filesystem, network, and process capability are absent",
      metrics,
      checks: [...checks],
      operability: operability(supervisorLoc),
      notes: [
        `cold start includes V8 worker boot, module compilation, and instantiation (${COLD_START_SAMPLES} samples after one warm-up)`,
        `density sweep at ${WASM_DENSITY_STEPS.join(", ")} instances`,
      ],
    }
  })

const describe = <A, E extends ProbeError>(
  result: Result.Result<A, E>,
  render: (value: A) => string,
): string => Result.isSuccess(result) ? render(result.success) : `failed: ${result.failure.detail}`

const summarizeLatency = (summary: LatencySummary): string =>
  `p50 ${summary.p50Ms.toFixed(1)} ms p95 ${summary.p95Ms.toFixed(1)} ms`

const summarizeDensity = (points: ReadonlyArray<DensityPoint>): string =>
  points.map((point) => `${point.agents}:${point.outcome}/${point.failures}`).join(" ")
