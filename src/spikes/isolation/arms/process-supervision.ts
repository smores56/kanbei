import { Effect, Scope } from "effect"
import { nowMs, ProbeError, readTextFile, withTimeout } from "../exec.ts"
import { makeLineChannel } from "../line-channel.ts"
import { AGENT_MEMORY_MAX_MIB, AGENT_TASKS_MAX } from "../protocol.ts"
import { sandboxCommand, sandboxPath } from "../sandbox.ts"
import {
  awaitEmpty,
  cgroupDirectory,
  cgroupProcs,
  createAgentCgroup,
  killCgroup,
  migratePid,
  parsePpid,
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

const parentOf = (pid: number): Effect.Effect<number | null> =>
  readTextFile(`/proc/${pid}/stat`).pipe(
    Effect.map((text) => (text === null ? null : parsePpid(text))),
    Effect.catch(() => Effect.succeed(null)),
  )

/**
 * Moves any child of the waiter that forked before the waiter was migrated out
 * of the supervisor cgroup; later forks inherit the agent cgroup directly.
 */
const adoptChildren = (
  config: ProcessArmConfig,
  cgroupPath: string,
  waiterPid: number,
): Effect.Effect<void, ProbeError> =>
  Effect.gen(function* () {
    const procs = yield* cgroupProcs(supervisorCgroup(config))
    yield* Effect.forEach(
      procs.filter((pid) => pid !== waiterPid),
      (pid) =>
        parentOf(pid).pipe(
          Effect.flatMap((ppid) =>
            ppid === waiterPid
              ? migratePid(cgroupPath, pid).pipe(Effect.catch(() => Effect.void))
              : Effect.void
          ),
        ),
      { discard: true },
    )
  })

const pollAgentPid = (
  config: ProcessArmConfig,
  cgroupPath: string,
  waiterPid: number,
  timeoutMs: number,
): Effect.Effect<number, ProbeError> =>
  Effect.gen(function* () {
    const deadline = Date.now() + timeoutMs
    const loop = (): Effect.Effect<number, ProbeError> =>
      Effect.gen(function* () {
        yield* adoptChildren(config, cgroupPath, waiterPid)
        const procs = yield* cgroupProcs(cgroupPath)
        const agent = procs.find((pid) => pid !== waiterPid)
        if (agent !== undefined) return agent
        if (Date.now() >= deadline) {
          return yield* Effect.fail(
            fail(
              "discover agent pid",
              `no agent pid in ${cgroupPath} after ${timeoutMs} ms (waiter ${waiterPid}, cgroup ${
                procs.join(",") || "<empty>"
              })`,
            ),
          )
        }
        yield* Effect.sleep("5 millis")
        return yield* loop()
      })
    return yield* loop()
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
    // Migrate before the agent touches memory so all its pages are charged to its
    // own cgroup; migrating a running V8 heap leaves old charges behind.
    yield* migratePid(cgroupPath, waiterPid).pipe(Effect.catch(() => Effect.void))
    const agentPid = yield* pollAgentPid(config, cgroupPath, waiterPid, 5_000)
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
