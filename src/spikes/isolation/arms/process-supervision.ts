import { Effect, Scope } from "effect"
import { nowMs, ProbeError, readTextFile, withTimeout } from "../exec.ts"
import { makeLineChannel } from "../line-channel.ts"
import { AGENT_MEMORY_MAX_MIB, AGENT_TASKS_MAX } from "../protocol.ts"
import { releasePath, sandboxCommand, sandboxPath } from "../sandbox.ts"
import {
  awaitEmpty,
  cgroupDirectory,
  cgroupProcs,
  createAgentCgroup,
  killCgroup,
  migratePid,
  parsePpid,
  parseState,
  removeCgroup,
} from "../supervisor.ts"
import type { AgentLimits, Supervisor } from "../supervisor.ts"

/**
 * Process-arm supervision surface: one agent per delegated cgroup, each inside
 * its own rootless user+PID+mount namespace and chroot with Landlock. The
 * `unshare` child is the namespace waiter, never the agent: signals and limits
 * use the cgroup and the agent's discovered host pid.
 */
export interface ProcessSupervisionConfig {
  readonly runId: string
  readonly agentScript: string
  readonly entryScript: string
  readonly sourceDir: string
  readonly workRoot: string
  readonly supervisor: Supervisor
}

export type ProcessArmConfig = ProcessSupervisionConfig

export interface ProcessAgent {
  /** Host pid of the agent (PID 1 inside its namespace). */
  readonly agentPid: number
  /** Pid the agent reported; relative to the PID namespace, never used for signals. */
  readonly namespacePid: number
  readonly cgroupPath: string
  readonly readyMs: number
  readonly take: Effect.Effect<string, ProbeError>
  readonly send: (line: string) => Effect.Effect<void, ProbeError>
  readonly request: (
    line: string,
    prefix: string,
  ) => Effect.Effect<{ readonly reply: string; readonly ms: number }, ProbeError>
  readonly status: Effect.Effect<Deno.CommandStatus, ProbeError>
  /** cgroup.kill, wait for populated=0, remove the cgroup, reap the waiter. Never fails. */
  readonly stop: Effect.Effect<void>
}

export const PROCESS_SUPERVISION_FILES: ReadonlyArray<string> = [
  "src/spikes/isolation/supervisor.ts",
  "src/spikes/isolation/sandbox.ts",
  "src/spikes/isolation/sandbox-entry.ts",
  "src/spikes/isolation/arms/process-supervision.ts",
  "src/spikes/isolation/agent-process.ts",
  "src/spikes/isolation/line-channel.ts",
]

const fail = (operation: string, detail: string): ProbeError =>
  new ProbeError({ operation, detail })

const supervisorCgroup = (config: ProcessArmConfig): string =>
  cgroupDirectory(config.supervisor, "supervisor")

export interface ProcessStat {
  readonly pid: number
  readonly ppid: number
  readonly state: string
}
export interface ProcessStat {
  readonly pid: number
  readonly ppid: number
  readonly state: string
}

/**
 * The wrapper is the waiter's child in the supervisor cgroup. It cannot SIGSTOP
 * itself once it is the PID-namespace init, so discovery targets the child and
 * the supervisor stops it from the parent namespace.
 */
export const sandboxChildPid = (
  procs: ReadonlyArray<number>,
  waiterPid: number,
  stats: ReadonlyArray<ProcessStat>,
): number | undefined =>
  stats.find((stat) =>
    procs.includes(stat.pid) && stat.pid !== waiterPid && stat.ppid === waiterPid
  )?.pid

const processStat = (pid: number): Effect.Effect<ProcessStat | null> =>
  readTextFile(`/proc/${pid}/stat`).pipe(
    Effect.map((text) => {
      if (text === null) return null
      const ppid = parsePpid(text)
      const state = parseState(text)
      return ppid === null || state === null ? null : { pid, ppid, state }
    }),
    Effect.catch(() => Effect.succeed(null)),
  )

const awaitSandboxChild = (
  config: ProcessArmConfig,
  waiterPid: number,
  timeoutMs: number,
): Effect.Effect<number, ProbeError> =>
  Effect.gen(function* () {
    const deadline = Date.now() + timeoutMs
    const loop = (): Effect.Effect<number, ProbeError> =>
      Effect.gen(function* () {
        const procs = yield* cgroupProcs(supervisorCgroup(config))
        const stats = yield* Effect.forEach(procs, processStat, { concurrency: 1 })
        const child = sandboxChildPid(
          procs,
          waiterPid,
          stats.filter((stat): stat is ProcessStat => stat !== null),
        )
        if (child !== undefined) return child
        if (Date.now() >= deadline) {
          return yield* Effect.fail(
            fail(
              "discover sandbox child",
              `no child of waiter ${waiterPid} in ${
                supervisorCgroup(config)
              } after ${timeoutMs} ms (procs ${procs.join(",") || "<empty>"})`,
            ),
          )
        }
        yield* Effect.sleep("2 millis")
        return yield* loop()
      })
    return yield* loop()
  })

/**
 * Ordering is the POINT of this handshake: the wrapper blocks before exec, the
 * supervisor stops it, migrates the stopped pid into the agent cgroup, resumes
 * it, and only then writes the release file the barrier waits on. Every Deno or
 * agent page allocated after this point is charged to agent-N; resuming first
 * and migrating later leaves the already-touched pages on the supervisor.
 */
const handshakeAgentPid = (
  config: ProcessArmConfig,
  cgroupPath: string,
  waiterPid: number,
  releaseFile: string,
): Effect.Effect<number, ProbeError> =>
  Effect.gen(function* () {
    const sandboxPid = yield* awaitSandboxChild(config, waiterPid, 5_000)
    yield* Effect.sync(() => Deno.kill(sandboxPid, "SIGSTOP"))
    yield* migratePid(cgroupPath, sandboxPid)
    const migrated = yield* cgroupProcs(cgroupPath)
    if (!migrated.includes(sandboxPid)) {
      return yield* Effect.fail(
        fail(
          "migrate stopped sandbox",
          `pid ${sandboxPid} is not in ${cgroupPath} after migration (procs ${
            migrated.join(",") || "<empty>"
          })`,
        ),
      )
    }
    yield* Effect.sync(() => Deno.kill(sandboxPid, "SIGCONT"))
    yield* Effect.tryPromise({
      try: () => Deno.writeTextFile(releaseFile, "1"),
      catch: (error) => fail("release sandbox", `${releaseFile}: ${String(error)}`),
    })
    return sandboxPid
  })

const workDirectory = (config: ProcessArmConfig, slot: number): string =>
  `${config.workRoot}/slot-${slot}`

const startSandbox = (config: ProcessArmConfig, slot: number): Effect.Effect<void, ProbeError> =>
  Effect.tryPromise({
    try: () => Deno.mkdir(`${workDirectory(config, slot)}/scratch/deno-cache`, { recursive: true }),
    catch: (error) => fail("prepare sandbox", String(error)),
  })

const stopAgent = (
  cgroupPath: string,
  work: string,
  handle: Deno.ChildProcess,
): Effect.Effect<void> =>
  killCgroup(cgroupPath).pipe(
    Effect.catch(() => Effect.void),
    Effect.flatMap(() =>
      awaitEmpty(cgroupPath, 2_000).pipe(Effect.catch(() => Effect.succeed(false)))
    ),
    Effect.flatMap(() => removeCgroup(cgroupPath).pipe(Effect.catch(() => Effect.void))),
    Effect.flatMap(() => Effect.tryPromise(() => handle.status).pipe(Effect.timeout("2 seconds"))),
    Effect.catch(() => Effect.void),
    Effect.flatMap(() =>
      Effect.tryPromise(() => Deno.remove(work, { recursive: true })).pipe(
        Effect.catch(() => Effect.void),
      )
    ),
    Effect.catch(() => Effect.void),
  )

export const startAgent = (
  config: ProcessArmConfig,
  slot: number,
  limits: AgentLimits,
  extraEnvironment?: Readonly<Record<string, string>>,
): Effect.Effect<ProcessAgent, ProbeError, Scope.Scope> =>
  Effect.gen(function* () {
    const startedAt = nowMs()
    const cgroupPath = yield* createAgentCgroup(config.supervisor, slot, limits)
    const work = workDirectory(config, slot)
    yield* startSandbox(config, slot)
    const command = sandboxCommand({
      sandboxRoot: `${work}/root`,
      scratchDir: `${work}/scratch`,
      sourceDir: config.sourceDir,
      denoPath: Deno.execPath(),
      entryScript: sandboxPath(config.sourceDir, config.entryScript),
      agentScript: sandboxPath(config.sourceDir, config.agentScript),
      environment: extraEnvironment ?? {},
    })
    const child = yield* Effect.acquireRelease(
      Effect.try({
        try: () =>
          new Deno.Command(command.command, {
            args: [...command.args],
            env: { ...command.env },
            stdin: "piped",
            stdout: "piped",
            stderr: "inherit",
          }).spawn(),
        catch: (error) => fail(`${config.runId} spawn`, String(error)),
      }),
      (handle) => stopAgent(cgroupPath, work, handle),
    )
    const waiterPid = child.pid
    const statusPromise = child.status
    const channel = yield* makeLineChannel(child.stdout, child.stdin)
    const agentPid = yield* handshakeAgentPid(
      config,
      cgroupPath,
      waiterPid,
      releasePath(`${work}/scratch`),
    )
    const readyLine = yield* Effect.race(
      withTimeout(channel.take, 20_000, `agent ${slot} ready`),
      Effect.tryPromise({
        try: () => statusPromise,
        catch: (error) => fail(`agent ${slot}`, String(error)),
      })
        .pipe(
          Effect.flatMap((status) =>
            Effect.fail(
              fail(
                `agent ${slot}`,
                `sandbox exited before ready (code=${status.code}); see supervisor stderr`,
              ),
            )
          ),
        ),
    )
    const [tag = "", pidText = "0"] = readyLine.split(" ")
    if (tag !== "READY") {
      return yield* Effect.fail(fail(`agent ${slot}`, `unexpected startup line: ${readyLine}`))
    }
    const status: Effect.Effect<Deno.CommandStatus, ProbeError> = Effect.tryPromise({
      try: () => statusPromise,
      catch: (error) => fail(`agent ${slot} status`, String(error)),
    })
    return {
      agentPid,
      waiterPid,
      namespacePid: Number(pidText),
      cgroupPath,
      readyMs: nowMs() - startedAt,
      take: channel.take,
      send: channel.send,
      request: (line, prefix) =>
        Effect.gen(function* () {
          const sentAt = nowMs()
          yield* channel.send(line)
          const reply = yield* withTimeout(channel.take, 10_000, `agent ${slot} ${line}`)
          if (!reply.startsWith(prefix)) {
            return yield* Effect.fail(
              fail(`agent ${slot}`, `expected ${prefix}, received ${reply}`),
            )
          }
          return { reply, ms: nowMs() - sentAt }
        }),
      status,
      stop: stopAgent(cgroupPath, work, child),
    }
  })

export const defaultLimits: AgentLimits = {
  memoryMaxMib: AGENT_MEMORY_MAX_MIB,
  tasksMax: AGENT_TASKS_MAX,
}
