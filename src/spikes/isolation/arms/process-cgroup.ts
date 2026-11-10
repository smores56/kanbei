import { Effect, Result, Scope } from "effect"
import {
  cgroupCurrentBytes,
  cgroupExists,
  cgroupPidsCurrent,
  cgroupPidsMax,
  memoryAvailableBytes,
} from "../host.ts"
import { nowMs, ProbeError, readTextFile, runCommand, withTimeout } from "../exec.ts"
import {
  AGENT_MEMORY_MAX_MIB,
  AGENT_TASKS_MAX,
  CLEANUP_TIMEOUT_MS,
  COLD_START_SAMPLES,
  CPU_WORK_ITERATIONS,
  DENSITY_STEPS,
  FORK_BOMB_TASKS_MAX,
  HANG_PROBE_TIMEOUT_MS,
  IPC_ROUND_TRIPS,
  OOM_PROBE_MEMORY_MAX_MIB,
  TARGET_CONCURRENCY,
} from "../protocol.ts"
import { round, summarize } from "../stats.ts"
import type {
  ArmMetrics,
  ArmResult,
  CheckId,
  ContainmentCheck,
  DensityPoint,
  LatencySummary,
  Operability,
} from "../types.ts"

import {
  cgroupCpuUsec,
  defaultLimits,
  PROCESS_SUPERVISION_FILES,
  processMarkerPids,
  rlimitSoft,
  startAgent,
  unitResult,
} from "./process-supervision.ts"
import type { ProcessArmConfig } from "./process-supervision.ts"

const range = (count: number): ReadonlyArray<number> => Array.from({ length: count }, (_v, i) => i)

const leftDetail = <A, E extends ProbeError>(result: Result.Result<A, E>): string | undefined =>
  Result.isFailure(result) ? result.failure.detail : undefined

const describe = <A, E extends ProbeError>(
  result: Result.Result<A, E>,
  render: (value: A) => string,
): string => Result.isSuccess(result) ? render(result.success) : `failed: ${result.failure.detail}`

const summarizeLatency = (summary: LatencySummary): string =>
  `p50 ${summary.p50Ms.toFixed(1)} ms p95 ${summary.p95Ms.toFixed(1)} ms`

const summarizeDensity = (points: ReadonlyArray<DensityPoint>): string =>
  points.map((point) => `${point.agents}:${point.outcome}/${point.failures}`).join(" ")

const runToExit = (
  config: ProcessArmConfig,
  slot: number,
): Effect.Effect<number, ProbeError, Scope.Scope> =>
  Effect.gen(function* () {
    const agent = yield* startAgent(config, slot, defaultLimits)
    yield* agent.send("EXIT")
    yield* withTimeout(agent.status, 5_000, "normal exit")
    return agent.readyMs
  })

const coldStartProbe = (config: ProcessArmConfig) =>
  Effect.gen(function* () {
    yield* Effect.scoped(runToExit(config, 900))
    const samples = yield* Effect.forEach(
      range(COLD_START_SAMPLES),
      (index) => Effect.scoped(runToExit(config, 901 + index)),
      { concurrency: 1 },
    )
    return summarize(samples)
  })

const cpuProbe = (config: ProcessArmConfig) =>
  Effect.scoped(
    Effect.gen(function* () {
      const agent = yield* startAgent(config, 910, defaultLimits)
      const before = yield* cgroupCpuUsec(agent.cgroupPath)
      const { reply } = yield* agent.request(`CPU ${CPU_WORK_ITERATIONS}`, "CPU")
      const after = yield* cgroupCpuUsec(agent.cgroupPath)
      const columns = reply.split(" ")
      const wallMs = Number(columns[2] ?? "0")
      yield* agent.send("EXIT")
      yield* withTimeout(agent.status, 5_000, "cpu probe exit")
      return { cpuMs: round((after - before) / 1000, 2), wallMs: round(wallMs, 2) }
    }),
  )

const ipcProbe = (config: ProcessArmConfig) =>
  Effect.scoped(
    Effect.gen(function* () {
      const agent = yield* startAgent(config, 920, defaultLimits)
      const samples = yield* Effect.forEach(
        range(IPC_ROUND_TRIPS),
        () => agent.request("PING", "PONG").pipe(Effect.map((result) => result.ms)),
        { concurrency: 1 },
      )
      yield* agent.send("EXIT")
      yield* withTimeout(agent.status, 5_000, "ipc probe exit")
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

const densityPoint = (config: ProcessArmConfig, agents: number) =>
  Effect.gen(function* () {
    const available = yield* memoryAvailableBytes
    if (available < agents * 80 * 1024 * 1024 + 384 * 1024 * 1024) {
      return skippedPoint(agents)
    }
    return yield* Effect.scoped(
      Effect.gen(function* () {
        const results = yield* Effect.forEach(
          range(agents),
          (index) => Effect.result(startAgent(config, 100 + index, defaultLimits)),
          { concurrency: agents },
        )
        const live = results.flatMap((result) => (Result.isSuccess(result) ? [result.success] : []))
        const rss = yield* Effect.forEach(
          live,
          (agent) => cgroupCurrentBytes(agent.cgroupPath),
          { concurrency: agents },
        )
        const failures = agents - live.length
        const readyMs = Math.max(0, ...live.map((agent) => agent.readyMs))
        const total = rss.reduce((sum, value) => sum + value, 0)
        yield* Effect.forEach(live, (agent) =>
          agent.send("EXIT").pipe(Effect.catch(() => Effect.void)), {
          concurrency: agents,
          discard: true,
        })
        yield* Effect.forEach(live, (agent) =>
          withTimeout(agent.status, 5_000, "density exit").pipe(
            Effect.catch(() =>
              Effect.void
            ),
          ), { concurrency: agents, discard: true })
        const measured = live.length === 0 ? 0 : total / live.length
        return {
          agents,
          outcome: failures === 0 ? "ok" : "partial",
          readyMs: round(readyMs, 1),
          totalRssBytes: total,
          perAgentRssBytes: live.length === 0 ? 0 : Math.round(measured),
          failures,
        } satisfies DensityPoint
      }),
    )
  })

const densityProbe = (config: ProcessArmConfig) =>
  Effect.gen(function* () {
    const points = yield* Effect.forEach(
      DENSITY_STEPS,
      (agents) =>
        densityPoint(config, agents).pipe(
          Effect.catch(() => Effect.succeed(skippedPoint(agents))),
        ),
      { concurrency: 1 },
    )
    yield* Effect.sleep("200 millis")
    return points
  })

const isKilled = (
  status: Deno.CommandStatus,
  systemdResult: string,
): boolean =>
  status.signal === "SIGKILL" || status.code === 137 || systemdResult === "signal" ||
  systemdResult === "core-dump"

const crashProbe = (config: ProcessArmConfig) =>
  Effect.scoped(
    Effect.gen(function* () {
      const agents = yield* Effect.forEach(
        range(3),
        (index) => startAgent(config, 930 + index, defaultLimits),
        { concurrency: 3 },
      )
      const victim = agents[0]
      const peers = agents.slice(1)
      if (victim === undefined) {
        return yield* Effect.fail(new ProbeError({ operation: "crash", detail: "no victim" }))
      }
      yield* Effect.forEach(peers, (agent) => agent.request("PING", "PONG"), { discard: true })
      yield* Effect.sync(() => Deno.kill(victim.pid, "SIGKILL"))
      const status = yield* withTimeout(victim.status, 5_000, "crash status")
      const systemdResult = yield* unitResult(victim.unit)
      const survivorReplies = yield* Effect.forEach(
        peers,
        (agent) => agent.request("PING", "PONG"),
        { concurrency: peers.length },
      )
      return {
        id: "crash-isolation" as const,
        status: isKilled(status, systemdResult) && survivorReplies.length === peers.length
          ? ("pass" as const)
          : ("fail" as const),
        detail:
          `SIGKILL on one agent: signal=${status.signal} code=${status.code} Result=${systemdResult}; ${survivorReplies.length}/${peers.length} peers still answered PING`,
      }
    }),
  )

const hangProbe = (config: ProcessArmConfig) =>
  Effect.scoped(
    Effect.gen(function* () {
      const agents = yield* Effect.forEach(
        range(2),
        (index) => startAgent(config, 940 + index, defaultLimits),
        { concurrency: 2 },
      )
      const victim = agents[0]
      const peer = agents[1]
      if (victim === undefined || peer === undefined) {
        return yield* Effect.fail(new ProbeError({ operation: "hang", detail: "missing agent" }))
      }
      yield* victim.send("HANG")
      yield* Effect.sleep("250 millis")
      const killedAt = nowMs()
      yield* runCommand("systemctl", ["--user", "kill", "--signal=SIGKILL", victim.unit])
      const status = yield* withTimeout(victim.status, HANG_PROBE_TIMEOUT_MS + 3_000, "hang status")
      const killMs = nowMs() - killedAt
      const systemdResult = yield* unitResult(victim.unit)
      const peerReply = yield* peer.request("PING", "PONG")
      return {
        id: "hang-containment" as const,
        status: isKilled(status, systemdResult) && peerReply.reply === "PONG"
          ? ("pass" as const)
          : ("fail" as const),
        detail: `infinite loop in one agent killed through its cgroup in ${
          killMs.toFixed(0)
        } ms (Result=${systemdResult}); peer answered PING`,
      }
    }),
  )

const oomProbe = (config: ProcessArmConfig) =>
  Effect.scoped(
    Effect.gen(function* () {
      const agents = yield* Effect.forEach(
        range(2),
        (index) =>
          startAgent(config, 950 + index, {
            memoryMaxMib: OOM_PROBE_MEMORY_MAX_MIB,
            tasksMax: AGENT_TASKS_MAX,
          }),
        { concurrency: 2 },
      )
      const victim = agents[0]
      const peer = agents[1]
      if (victim === undefined || peer === undefined) {
        return yield* Effect.fail(new ProbeError({ operation: "oom", detail: "missing agent" }))
      }
      const oomStartedAt = nowMs()
      yield* victim.send("ALLOC 512")
      const status = yield* withTimeout(victim.status, 20_000, "oom status")
      const oomMs = nowMs() - oomStartedAt
      const result = yield* runCommand("systemctl", [
        "--user",
        "show",
        "--property",
        "Result",
        "--value",
        victim.unit,
      ])
      const peerReply = yield* peer.request("PING", "PONG")
      const contained = result.stdout.trim() === "oom-kill" && peerReply.reply === "PONG"
      return {
        id: "oom-containment" as const,
        status: contained ? ("pass" as const) : ("fail" as const),
        detail:
          `MemoryMax=${OOM_PROBE_MEMORY_MAX_MIB}M, 512 MiB request: Result=${result.stdout.trim()} signal=${status.signal} after ${
            oomMs.toFixed(0)
          } ms; peer answered PING`,
      }
    }),
  )

const forkBombProbe = (config: ProcessArmConfig) =>
  Effect.gen(function* () {
    const unit = `kanbei-iso-${config.runId}-forkbomb`
    const marker = `${config.runId}-forkbomb`
    const script =
      `i=0; while [ $i -lt 200 ]; do ( sh -c 'sleep 30' '${marker}' & ); i=$((i+1)); done; wait`
    yield* runCommand("systemd-run", [
      "--user",
      "--unit",
      unit,
      "--property",
      `TasksMax=${FORK_BOMB_TASKS_MAX}`,
      "--property",
      "MemoryMax=128M",
      "--property",
      "RuntimeMaxSec=10s",
      "sh",
      "-c",
      script,
    ])
    yield* Effect.sleep("900 millis")
    const control = yield* runCommand("systemctl", [
      "--user",
      "show",
      "--property",
      "ControlGroup",
      "--value",
      unit,
    ])
    const cgroupPath = control.stdout.trim()
    const pidsMax = yield* cgroupPidsMax(cgroupPath)
    const samples = yield* Effect.forEach(
      range(8),
      () =>
        Effect.gen(function* () {
          const current = yield* cgroupPidsCurrent(cgroupPath)
          yield* Effect.sleep("50 millis")
          return current
        }),
      { concurrency: 1 },
    )
    const peak = samples.reduce((highest, value) => Math.max(highest, value), 0)
    const stoppedAt = nowMs()
    yield* runCommand("systemctl", ["--user", "kill", "--signal=SIGKILL", unit]).pipe(
      Effect.catch(() => Effect.void),
    )
    yield* runCommand("systemctl", ["--user", "stop", unit]).pipe(Effect.catch(() => Effect.void))
    yield* runCommand("systemctl", ["--user", "reset-failed", unit]).pipe(
      Effect.catch(() => Effect.void),
    )
    const teardownMs = nowMs() - stoppedAt
    yield* Effect.sleep("300 millis")
    const leftovers = yield* Effect.sync(() => processMarkerPids(marker))
    const contained = peak > 0 && pidsMax !== null && peak <= pidsMax && leftovers.length === 0
    return {
      id: "fork-bomb-containment" as const,
      status: contained ? ("pass" as const) : ("fail" as const),
      detail:
        `200 fork attempts with TasksMax=${FORK_BOMB_TASKS_MAX}: peak pids.current=${peak}, pids.max=${
          pidsMax ?? "unknown"
        }, ${leftovers.length} processes survived unit teardown (${teardownMs.toFixed(0)} ms)`,
    }
  })

const cleanupProbe = (config: ProcessArmConfig) =>
  Effect.gen(function* () {
    const marker = `${config.runId}-orphan`
    const target = yield* Effect.scoped(
      Effect.gen(function* () {
        const agent = yield* startAgent(config, 970, defaultLimits, {
          KANBEI_ISO_MARKER: marker,
        })
        yield* agent.send("SPAWN")
        const spawnReply = yield* withTimeout(agent.take, 10_000, "orphan spawn")
        const orphanPid = Number(spawnReply.split(" ")[1] ?? "0")
        yield* Effect.sync(() => Deno.kill(agent.pid, "SIGKILL"))
        const status = yield* withTimeout(agent.status, CLEANUP_TIMEOUT_MS, "orphan kill")
        return { unit: agent.unit, cgroupPath: agent.cgroupPath, orphanPid, status }
      }),
    )
    yield* Effect.sleep("400 millis")
    const leftovers = yield* Effect.sync(() => processMarkerPids(marker))
    const cgroupGone = yield* cgroupExists(target.cgroupPath).pipe(Effect.map((exists) => !exists))
    const unit = yield* runCommand("systemctl", [
      "--user",
      "show",
      "--property",
      "ActiveState",
      "--value",
      target.unit,
    ])
    const state = unit.stdout.trim()
    const inactive = state === "inactive" || state === "failed" || state === ""
    const contained = leftovers.length === 0 && cgroupGone && inactive
    return {
      id: "cleanup-after-kill" as const,
      status: contained ? ("pass" as const) : ("fail" as const),
      detail:
        `SIGKILL of agent (code=${target.status.code}); orphan ${target.orphanPid} reaped, cgroup removed=${cgroupGone}, unit=${unit.stdout.trim()}, ${leftovers.length} marker processes left`,
    }
  })

const orphanProbe = (config: ProcessArmConfig) =>
  Effect.gen(function* () {
    const marker = `${config.runId}-orphan-check`
    const target = yield* Effect.scoped(
      Effect.gen(function* () {
        const agent = yield* startAgent(config, 980, defaultLimits, {
          KANBEI_ISO_MARKER: marker,
        })
        yield* agent.send("SPAWN")
        const spawnReply = yield* withTimeout(agent.take, 10_000, "orphan spawn")
        const orphanPid = Number(spawnReply.split(" ")[1] ?? "0")
        const aliveBefore = yield* Effect.sync(() => processMarkerPids(marker))
        yield* Effect.sync(() => Deno.kill(agent.pid, "SIGKILL"))
        yield* withTimeout(agent.status, CLEANUP_TIMEOUT_MS, "orphan agent kill")
        return { orphanPid, aliveBefore }
      }),
    )
    yield* Effect.sleep("400 millis")
    const aliveAfter = yield* Effect.sync(() => processMarkerPids(marker))
    const reaped = target.aliveBefore.length > 0 && aliveAfter.length === 0
    return {
      id: "orphan-reaping" as const,
      status: reaped ? ("pass" as const) : ("fail" as const),
      detail:
        `grandchild visible before kill (${target.aliveBefore.length}), ${aliveAfter.length} alive after cgroup teardown`,
    }
  })

const ceilingsProbe = (config: ProcessArmConfig) =>
  Effect.scoped(
    Effect.gen(function* () {
      const agent = yield* startAgent(config, 990, defaultLimits)
      const readCgroup = (name: string) =>
        Effect.tryPromise({
          try: () => Deno.readTextFile(`/sys/fs/cgroup${agent.cgroupPath}/${name}`),
          catch: (error) => new ProbeError({ operation: `read ${name}`, detail: String(error) }),
        })
      const memoryMax = yield* readCgroup("memory.max")
      const pidsMax = yield* readCgroup("pids.max")
      const cpuMax = yield* readCgroup("cpu.max")
      const processLimits = yield* readTextFile(`/proc/${agent.pid}/limits`)
      yield* agent.send("EXIT")
      yield* withTimeout(agent.status, 5_000, "ceilings exit")
      const nofile = rlimitSoft(processLimits, "Max open files")
      const fsize = rlimitSoft(processLimits, "Max file size")
      const configured = Number(memoryMax.trim()) === AGENT_MEMORY_MAX_MIB * 1024 * 1024 &&
        Number(pidsMax.trim()) === AGENT_TASKS_MAX &&
        cpuMax.trim().startsWith("100000") &&
        nofile === "256" &&
        fsize === String(32768 * 512)
      return {
        id: "resource-ceilings" as const,
        status: configured ? ("pass" as const) : ("fail" as const),
        detail:
          `memory.max=${memoryMax.trim()} pids.max=${pidsMax.trim()} cpu.max=${cpuMax.trim()} nofile=${nofile} fsize=${fsize}`,
      }
    }),
  )

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
  hostDependencies: ["deno", "systemd user manager with cgroup v2 delegation"],
  requiresRoot: false,
  supervisorLoc,
  packaging: "One process per agent unit; agent entrypoint is a TypeScript module run by deno",
  debuggability: "Standard POSIX process tools, /proc, cgroup files, and systemd unit status",
  limitations: [
    "Cgroup memory, pids, and CPU ceilings require systemd user delegation; without it only rlimits apply",
    "RLIMIT_AS is unusable for a V8 process because the runtime reserves large virtual ranges",
    "Each agent pays systemd unit creation on the cold-start path",
    "Deno denies even `/proc/self` under a narrow `--allow-read` grant, so the agent observes nothing about itself; the supervisor reads all cgroup and rlimit state",
    "Agent capabilities are inner Deno permissions only (`--allow-env`, `--allow-run` here); the OS sandbox remains the real boundary",
  ],
})

export const PROCESS_ARM_FILES = PROCESS_SUPERVISION_FILES

export const measureProcessArm = (
  config: ProcessArmConfig,
  supervisorLoc: number,
): Effect.Effect<ArmResult> =>
  Effect.gen(function* () {
    const coldStart = yield* Effect.result(coldStartProbe(config))
    const cpu = yield* Effect.result(cpuProbe(config))
    const ipc = yield* Effect.result(ipcProbe(config))
    const density = yield* Effect.result(densityProbe(config))
    yield* Effect.logInfo(`process-cgroup cold start: ${describe(coldStart, summarizeLatency)}`)
    yield* Effect.logInfo(`process-cgroup cpu: ${describe(cpu, (value) => `${value.cpuMs} ms`)}`)
    yield* Effect.logInfo(`process-cgroup ipc: ${describe(ipc, summarizeLatency)}`)
    yield* Effect.logInfo(`process-cgroup density: ${describe(density, summarizeDensity)}`)

    const checks = yield* Effect.all([
      guard("crash-isolation", crashProbe(config), (check) => check),
      guard("hang-containment", hangProbe(config), (check) => check),
      guard("oom-containment", oomProbe(config), (check) => check),
      guard("fork-bomb-containment", forkBombProbe(config), (check) => check),
      guard("cleanup-after-kill", cleanupProbe(config), (check) => check),
      guard("orphan-reaping", orphanProbe(config), (check) => check),
      Effect.succeed<ContainmentCheck>(
        Result.isSuccess(ipc)
          ? {
            id: "ipc-round-trip",
            status: "pass",
            detail: `PING/PONG over the supervisor pipe completed ${IPC_ROUND_TRIPS} times, p95 ${
              round(ipc.success.p95Ms, 2)
            } ms`,
          }
          : { id: "ipc-round-trip", status: "fail", detail: ipc.failure.detail },
      ),
      guard("resource-ceilings", ceilingsProbe(config), (check) => check),
      Effect.succeed<ContainmentCheck>({
        id: "hosts-agent-workload",
        status: "pass",
        detail:
          "the reference agent ran as a Deno TypeScript process with environment and process capability under the cgroup",
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
        arm: "process-cgroup",
        status: "inconclusive",
        checks: [...checks],
        operability: operability(supervisorLoc),
        notes: [
          `metric probe failed: ${
            [
              leftDetail(coldStart),
              leftDetail(cpu),
              leftDetail(ipc),
              leftDetail(density),
            ].filter((detail) => detail !== undefined).join("; ")
          }`,
        ],
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
      arm: "process-cgroup",
      status: "measured",
      metrics,
      checks: [...checks],
      operability: operability(supervisorLoc),
      notes: [
        `cold start includes systemd transient unit creation and Deno module loading (${COLD_START_SAMPLES} samples after one warm-up)`,
        `density sweep at ${
          DENSITY_STEPS.join(", ")
        } agents; per-agent RSS is the mean cgroup memory.current at all-ready`,
      ],
    }
  })
