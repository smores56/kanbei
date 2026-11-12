import { Effect, Result, Scope } from "effect"
import { nowMs, ProbeError, readTextFile, withTimeout } from "../exec.ts"
import { memoryAvailableBytes } from "../host.ts"
import { workloadReplyHolds } from "../agent-contract.ts"
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
import { forkBombHolds, orphanContainmentHolds } from "./containment.ts"
import type { ForkBombEvidence, OrphanEvidence } from "./containment.ts"
import {
  awaitEmpty,
  cgroupEvents,
  cgroupExists,
  cgroupProcs,
  cgroupStats,
  HOST_DEPENDENCIES,
  killCgroup,
  parseStartTime,
} from "../supervisor.ts"
import type {
  ArmMetrics,
  ArmResult,
  CheckId,
  ContainmentCheck,
  DensityPoint,
  LatencySummary,
  Operability,
} from "../types.ts"

import { defaultLimits, PROCESS_SUPERVISION_FILES, startAgent } from "./process-supervision.ts"
import type { ProcessArmConfig } from "./process-supervision.ts"

const range = (count: number): ReadonlyArray<number> => Array.from({ length: count }, (_v, i) => i)

/** Probe helper: host pids whose argv still carries the scenario marker. */
const processMarkerPids = (marker: string): ReadonlyArray<number> =>
  [...Deno.readDirSync("/proc")].flatMap((entry) => {
    if (!/^\d+$/u.test(entry.name)) return []
    try {
      const cmdline = new TextDecoder().decode(Deno.readFileSync(`/proc/${entry.name}/cmdline`))
      return cmdline.includes(marker) ? [Number(entry.name)] : []
    } catch {
      return []
    }
  })

/** Numeric /proc entries; the coarse host-wide signal that nothing was left behind. */
export const hostProcessCount = (): number =>
  [...Deno.readDirSync("/proc")].filter((entry) => /^\d+$/u.test(entry.name)).length

const processStartTime = (pid: number): Effect.Effect<number | null> =>
  readTextFile(`/proc/${pid}/stat`).pipe(
    Effect.map((text) => (text === null ? null : parseStartTime(text))),
    Effect.catch(() => Effect.succeed(null)),
  )

/** Start time guards against a pid that was reused by an unrelated process. */
const processIsSame = (pid: number, startTime: number): Effect.Effect<boolean> =>
  processStartTime(pid).pipe(Effect.map((current) => current !== null && current === startTime))

/** Soft value of a `/proc/<pid>/limits` row. */
const rlimitSoft = (text: string | null, label: string): string => {
  const row = (text ?? "").split("\n").find((line) => line.startsWith(label))
  return row === undefined
    ? "unknown"
    : row.slice(label.length).trim().split(/\s+/u)[0] ?? "unknown"
}

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
      const before = yield* cgroupStats(agent.cgroupPath)
      const { reply } = yield* agent.request(`CPU ${CPU_WORK_ITERATIONS}`, "CPU")
      const after = yield* cgroupStats(agent.cgroupPath)
      const columns = reply.split(" ")
      const wallMs = Number(columns[2] ?? "0")
      yield* agent.send("EXIT")
      yield* withTimeout(agent.status, 5_000, "cpu probe exit")
      const cpuMs = round((after.cpuUsageUsec - before.cpuUsageUsec) / 1000, 2)
      return { cpuMs, wallMs: round(wallMs, 2) }
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
          (agent) =>
            cgroupStats(agent.cgroupPath).pipe(Effect.map((stats) => stats.memoryCurrentBytes)),
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
      yield* Effect.sync(() => Deno.kill(victim.agentPid, "SIGKILL"))
      const status = yield* withTimeout(victim.status, 5_000, "crash status")
      const empty = yield* withTimeout(
        awaitEmpty(victim.cgroupPath, CLEANUP_TIMEOUT_MS),
        CLEANUP_TIMEOUT_MS,
        "crash cgroup empty",
      )
      const survivorReplies = yield* Effect.forEach(
        peers,
        (agent) => agent.request("PING", "PONG"),
        { concurrency: peers.length },
      )
      const contained = status.code === 137 && empty && survivorReplies.length === peers.length
      return {
        id: "crash-isolation" as const,
        status: contained ? ("pass" as const) : ("fail" as const),
        detail:
          `SIGKILL to agent host pid ${victim.agentPid} (namespace pid ${victim.namespacePid}): waiter exit code=${status.code} signal=${status.signal}, cgroup populated=0=${empty}; ${survivorReplies.length}/${peers.length} peers still answered PING`,
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
      yield* killCgroup(victim.cgroupPath)
      const empty = yield* awaitEmpty(victim.cgroupPath, CLEANUP_TIMEOUT_MS)
      const killMs = nowMs() - killedAt
      const status = yield* withTimeout(victim.status, HANG_PROBE_TIMEOUT_MS + 3_000, "hang status")
      const peerReply = yield* peer.request("PING", "PONG")
      const contained = empty && killMs <= CLEANUP_TIMEOUT_MS && peerReply.reply === "PONG"
      return {
        id: "hang-containment" as const,
        status: contained ? ("pass" as const) : ("fail" as const),
        detail: `cgroup.kill on a spinning agent: populated=0 in ${
          killMs.toFixed(0)
        } ms (limit ${CLEANUP_TIMEOUT_MS} ms), waiter exit code=${status.code}; peer answered PING`,
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
      yield* victim.send("ALLOC 128")
      const status = yield* withTimeout(victim.status, 20_000, "oom status")
      const oomMs = nowMs() - oomStartedAt
      const stats = yield* cgroupStats(victim.cgroupPath)
      const empty = yield* awaitEmpty(victim.cgroupPath, CLEANUP_TIMEOUT_MS)
      const peerReply = yield* peer.request("PING", "PONG")
      const contained = stats.memoryOomKills >= 1 && empty && peerReply.reply === "PONG"
      return {
        id: "oom-containment" as const,
        status: contained ? ("pass" as const) : ("fail" as const),
        detail:
          `memory.max=${OOM_PROBE_MEMORY_MAX_MIB} MiB, 128 MiB request: memory.events oom_kill=${stats.memoryOomKills}, waiter exit code=${status.code}, cgroup populated=0=${empty} after ${
            oomMs.toFixed(0)
          } ms; peer answered PING`,
      }
    }),
  )

const forkBombProbe = (config: ProcessArmConfig) =>
  Effect.scoped(
    Effect.gen(function* () {
      const marker = `${config.runId}-forkbomb`
      const agent = yield* startAgent(
        config,
        960,
        { memoryMaxMib: AGENT_MEMORY_MAX_MIB, tasksMax: FORK_BOMB_TASKS_MAX },
        { KANBEI_ISO_MARKER: marker },
      )
      yield* agent.send("FORKBOMB")
      const reply = yield* withTimeout(agent.take, 10_000, "fork bomb spawn")
      // The marker leaves must be resident before the sample; otherwise the
      // pre-kill liveness signal races the fork storm.
      yield* Effect.sleep("200 millis")
      const markerPidsBefore = yield* Effect.sync(() => processMarkerPids(marker))
      const samples = yield* Effect.forEach(
        range(16),
        () =>
          Effect.gen(function* () {
            const stats = yield* cgroupStats(agent.cgroupPath)
            yield* Effect.sleep("50 millis")
            return stats.pidsCurrent
          }),
        { concurrency: 1 },
      )
      const stats = yield* cgroupStats(agent.cgroupPath)
      const peak = samples.reduce((highest, value) => Math.max(highest, value), 0)
      yield* killCgroup(agent.cgroupPath)
      yield* awaitEmpty(agent.cgroupPath, CLEANUP_TIMEOUT_MS)
      const populatedAfter = yield* cgroupEvents(agent.cgroupPath)
      const procsAfter = yield* cgroupProcs(agent.cgroupPath)
      yield* agent.stop
      yield* Effect.sleep("300 millis")
      const markerPidsAfter = yield* Effect.sync(() => processMarkerPids(marker))
      const evidence: ForkBombEvidence = {
        markerPidsBefore: markerPidsBefore.length,
        pidsMaxEvents: stats.pidsMaxEvents,
        populatedAfter,
        procsAfter: procsAfter.length,
        markerPidsAfter: markerPidsAfter.length,
      }
      const contained = forkBombHolds(evidence)
      return {
        id: "fork-bomb-containment" as const,
        status: contained ? ("pass" as const) : ("fail" as const),
        detail:
          `200 fork attempts with pids.max=${FORK_BOMB_TASKS_MAX}: ${reply}, pids.events max=${stats.pidsMaxEvents} (refusals observed), peak pids.current=${peak} (observation only; the kernel caps it at pids.max), marker processes before kill=${markerPidsBefore.length}, after kill populated=${populatedAfter}, cgroup.procs=${procsAfter.length}, marker survivors=${markerPidsAfter.length}`,
      }
    }),
  )

interface OrphanObservation {
  readonly agentPid: number
  readonly agentNamespacePid: number
  readonly orphanHostPid: number | null
  readonly orphanNamespacePid: number
  readonly waiterCode: number
  readonly evidence: OrphanEvidence
}

/**
 * One scenario for both orphan probes: spawn a marker child, prove it alive in
 * the agent cgroup, kill the namespace init, then collect every post-kill
 * signal. Nothing here trusts the cmdline scan on its own.
 */
const observeOrphan = (
  config: ProcessArmConfig,
  slot: number,
  marker: string,
  operation: string,
): Effect.Effect<OrphanObservation, ProbeError> =>
  Effect.scoped(
    Effect.gen(function* () {
      const hostCountBefore = yield* Effect.sync(hostProcessCount)
      const agent = yield* startAgent(config, slot, defaultLimits, { KANBEI_ISO_MARKER: marker })
      yield* agent.send("SPAWN")
      const spawnReply = yield* withTimeout(agent.take, 10_000, `${operation} spawn`)
      const orphanNamespacePid = Number(spawnReply.split(" ")[1] ?? "0")
      // Let the marker child exec before sampling; the pre-kill proof is
      // otherwise a sub-millisecond race that always "passes".
      yield* Effect.sleep("200 millis")
      const pidsBefore = yield* cgroupStats(agent.cgroupPath)
      const procsBefore = yield* cgroupProcs(agent.cgroupPath)
      const markerPidsBefore = yield* Effect.sync(() => processMarkerPids(marker))
      const orphanHostPid = procsBefore.find((pid) => pid !== agent.agentPid) ?? null
      const orphanStartTime = orphanHostPid === null ? null : yield* processStartTime(orphanHostPid)
      yield* Effect.sync(() => Deno.kill(agent.agentPid, "SIGKILL"))
      const status = yield* withTimeout(agent.status, CLEANUP_TIMEOUT_MS, `${operation} kill`)
      yield* awaitEmpty(agent.cgroupPath, CLEANUP_TIMEOUT_MS)
      const populatedAfter = yield* cgroupEvents(agent.cgroupPath)
      const procsAfter = yield* cgroupProcs(agent.cgroupPath)
      yield* agent.stop
      const cgroupRemoved = !(yield* cgroupExists(agent.cgroupPath))
      yield* Effect.sleep("400 millis")
      const markerPidsAfter = yield* Effect.sync(() => processMarkerPids(marker))
      const orphanAliveAfter = orphanHostPid === null || orphanStartTime === null
        ? true
        : yield* processIsSame(orphanHostPid, orphanStartTime)
      return {
        agentPid: agent.agentPid,
        agentNamespacePid: agent.namespacePid,
        orphanHostPid,
        orphanNamespacePid,
        waiterCode: status.code,
        evidence: {
          markerPidsBefore: markerPidsBefore.length,
          pidsCurrentBefore: pidsBefore.pidsCurrent,
          orphanPidPresent: orphanHostPid !== null,
          orphanAliveAfter,
          hostCountBefore,
          hostCountAfter: yield* Effect.sync(hostProcessCount),
          populatedAfter,
          procsAfter: procsAfter.length,
          markerPidsAfter: markerPidsAfter.length,
          cgroupRemoved,
        },
      }
    }),
  )

const cleanupProbe = (config: ProcessArmConfig) =>
  Effect.scoped(
    Effect.gen(function* () {
      const observation = yield* observeOrphan(
        config,
        970,
        `${config.runId}-orphan`,
        "orphan cleanup",
      )
      const evidence = observation.evidence
      const contained = orphanContainmentHolds(evidence)
      return {
        id: "cleanup-after-kill" as const,
        status: contained ? ("pass" as const) : ("fail" as const),
        detail:
          `SIGKILL of agent host pid ${observation.agentPid} (namespace pid ${observation.agentNamespacePid}, waiter code=${observation.waiterCode}): orphan host pid ${
            observation.orphanHostPid ?? "not found"
          } (namespace pid ${observation.orphanNamespacePid}) ${
            evidence.orphanAliveAfter ? "STILL ALIVE" : "confirmed gone"
          }; pids.current before kill=${evidence.pidsCurrentBefore}, marker pids before kill=${evidence.markerPidsBefore}, after kill populated=${evidence.populatedAfter}, cgroup.procs=${evidence.procsAfter}, marker survivors=${evidence.markerPidsAfter}, cgroup removed=${evidence.cgroupRemoved}, host processes ${evidence.hostCountBefore}->${evidence.hostCountAfter}`,
      }
    }),
  )

const orphanProbe = (config: ProcessArmConfig) =>
  Effect.scoped(
    Effect.gen(function* () {
      const observation = yield* observeOrphan(
        config,
        980,
        `${config.runId}-orphan-check`,
        "orphan reaping",
      )
      const evidence = observation.evidence
      const reaped = orphanContainmentHolds(evidence)
      return {
        id: "orphan-reaping" as const,
        status: reaped ? ("pass" as const) : ("fail" as const),
        detail: `orphan host pid ${
          observation.orphanHostPid ?? "not found"
        } (namespace pid ${observation.orphanNamespacePid}) visible before the kill via cgroup membership (pids.current=${evidence.pidsCurrentBefore}) and marker scan (${evidence.markerPidsBefore} pids); after the namespace init died: populated=${evidence.populatedAfter}, cgroup.procs=${evidence.procsAfter}, marker survivors=${evidence.markerPidsAfter}, orphan ${
          evidence.orphanAliveAfter ? "STILL ALIVE" : "confirmed gone"
        }, host processes ${evidence.hostCountBefore}->${evidence.hostCountAfter}`,
      }
    }),
  )

const ceilingsProbe = (config: ProcessArmConfig) =>
  Effect.scoped(
    Effect.gen(function* () {
      const agent = yield* startAgent(config, 990, defaultLimits)
      const readCgroup = (name: string) =>
        Effect.tryPromise({
          try: () => Deno.readTextFile(`${agent.cgroupPath}/${name}`),
          catch: (error) => new ProbeError({ operation: `read ${name}`, detail: String(error) }),
        })
      const memoryMax = yield* readCgroup("memory.max")
      const pidsMax = yield* readCgroup("pids.max")
      const cpuMax = yield* readCgroup("cpu.max")
      const processLimits = yield* readTextFile(`/proc/${agent.agentPid}/limits`)
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

const workloadProbe = (config: ProcessArmConfig) =>
  Effect.scoped(
    Effect.gen(function* () {
      const agent = yield* startAgent(config, 995, defaultLimits)
      const { reply } = yield* agent.request("WORKLOAD", "WORKLOAD")
      yield* agent.send("EXIT")
      yield* withTimeout(agent.status, 5_000, "workload exit")
      const holds = workloadReplyHolds(reply)
      return {
        id: "hosts-agent-workload" as const,
        status: holds ? ("pass" as const) : ("fail" as const),
        detail: holds
          ? "agent reported `WORKLOAD ok`: read /etc/os-release, wrote and read /scratch/workload.txt, spawned true, and reached 127.0.0.1:9 with ECONNREFUSED"
          : `agent workload probe did not report WORKLOAD ok: ${reply}`,
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
  hostDependencies: [...HOST_DEPENDENCIES],
  requiresRoot: false,
  supervisorLoc,
  packaging:
    "One process per agent inside a per-agent rootless namespace and chroot, under a delegated cgroup; the agent entrypoint is a TypeScript module run by deno",
  debuggability:
    "Host-side /proc and cgroup files; the agent pid is the namespace init and cgroup membership is the signal path. The agent sees no /sys and no systemd unit exists to inspect",
  limitations: [
    "Agent, supervisor, and broker share one UID; isolation is namespace, chroot, Landlock, and cgroup, not UID-based",
    "No seccomp policy and io_uring remains enabled, which Landlock does not cover",
    "/sys is intentionally not mounted, which is what reverses SD-10: the agent cannot reach its own cgroup to raise its limits",
    "Landlock covers path-based filesystem access only; network and signal policy stay outside this spike",
    "Each agent pays user/PID/mount namespace setup, bind mounts, chroot, Landlock, and cgroup creation on the cold-start path",
    "RLIMIT_AS is unusable for a V8 process because the runtime reserves large virtual ranges",
    "Agent capabilities are inner Deno permissions only (`--allow-env`, `--allow-run`, `--allow-read=/etc`, `--allow-write=/scratch`, `--allow-net=127.0.0.1:9` here); the OS sandbox remains the real boundary",
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
      guard("hosts-agent-workload", workloadProbe(config), (check) => check),
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
        scope:
          "Nine containment checks, cold start, CPU, IPC, and density were attempted for one Deno agent per cgroup; the metric probe failed, so no operability or density conclusion follows",
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
      scope:
        "The nine containment ids, cold start, CPU, IPC, and density describe one Deno agent per rootless namespace, chroot, Landlock ruleset, and cgroup; no network, credential, or trust-root boundary is claimed",
      metrics,
      checks: [...checks],
      operability: operability(supervisorLoc),
      notes: [
        `cold start includes delegated-cgroup creation, user/PID/mount namespace setup, bind mounts, chroot, Landlock, and Deno module loading (${COLD_START_SAMPLES} samples after one warm-up)`,
        `density sweep at ${
          DENSITY_STEPS.join(", ")
        } agents; per-agent RSS is the mean cgroup memory.current at all-ready`,
      ],
    }
  })
