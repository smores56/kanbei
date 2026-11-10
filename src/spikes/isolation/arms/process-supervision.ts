import { Effect, Ref, Scope } from "effect"
import { nowMs, ProbeError, readTextFile, runCommand, withTimeout } from "../exec.ts"
import { makeLineChannel } from "../line-channel.ts"
import {
  AGENT_CPU_QUOTA_PERCENT,
  AGENT_MEMORY_MAX_MIB,
  AGENT_TASKS_MAX,
  AGENT_WALL_CLOCK_SECONDS,
} from "../protocol.ts"

/**
 * Minimal process-arm supervision surface: start one agent under a cgroup and
 * rlimits, speak the line protocol, expose its cgroup path, and stop the unit.
 * Measurement probes live in `process-cgroup.ts` and are excluded from the
 * operability line count.
 */
export interface ProcessSupervisionConfig {
  readonly runId: string
  readonly agentScript: string
  readonly memoryMaxMib: number
  readonly tasksMax: number
}

/** Alias kept for the probe module, which configures the same shape. */
export type ProcessArmConfig = ProcessSupervisionConfig

interface AgentLimits {
  readonly memoryMaxMib: number
  readonly tasksMax: number
}

export interface ProcessAgent {
  readonly unit: string
  readonly pid: number
  readonly cgroupPath: string
  readonly readyMs: number
  readonly take: Effect.Effect<string, ProbeError>
  readonly send: (line: string) => Effect.Effect<void, ProbeError>
  readonly request: (
    line: string,
    prefix: string,
  ) => Effect.Effect<{ readonly reply: string; readonly ms: number }, ProbeError>
  readonly status: Effect.Effect<Deno.CommandStatus, ProbeError>
  readonly stderrText: Effect.Effect<string>
}

export const PROCESS_SUPERVISION_FILES: ReadonlyArray<string> = [
  "src/spikes/isolation/arms/process-supervision.ts",
  "src/spikes/isolation/agent-process.ts",
  "src/spikes/isolation/line-channel.ts",
]
export const stopUnit = (unit: string): Effect.Effect<void> =>
  Effect.gen(function* () {
    yield* runCommand("systemctl", ["--user", "stop", unit]).pipe(Effect.catch(() => Effect.void))
    yield* runCommand("systemctl", ["--user", "reset-failed", unit]).pipe(
      Effect.catch(() => Effect.void),
    )
  })

export const resolveCgroupPath = (unit: string): Effect.Effect<string, ProbeError> =>
  runCommand("systemctl", ["--user", "show", "--property", "ControlGroup", "--value", unit]).pipe(
    Effect.map((output) => output.stdout.trim()),
  )

export const unitResult = (unit: string): Effect.Effect<string, ProbeError> =>
  runCommand("systemctl", ["--user", "show", "--property", "Result", "--value", unit]).pipe(
    Effect.map((output) => output.stdout.trim()),
  )

export const cgroupCpuUsec = (cgroupPath: string): Effect.Effect<number> =>
  readTextFile(`/sys/fs/cgroup${cgroupPath}/cpu.stat`).pipe(
    Effect.map((text) => {
      const match = /^usage_usec\s+(\d+)$/mu.exec(text ?? "")
      return match?.[1] === undefined ? 0 : Number(match[1])
    }),
    Effect.catch(() => Effect.succeed(0)),
  )

/** Soft value of a `/proc/<pid>/limits` row. */
export const rlimitSoft = (text: string | null, label: string): string => {
  const row = (text ?? "").split("\n").find((line) => line.startsWith(label))
  if (row === undefined) return "unknown"
  return row.slice(label.length).trim().split(/\s+/u)[0] ?? "unknown"
}

const drain = (
  stream: ReadableStream<Uint8Array>,
  sink: Ref.Ref<ReadonlyArray<string>>,
): Effect.Effect<void> =>
  Effect.gen(function* () {
    const reader = stream.getReader()
    const decoder = new TextDecoder()
    const loop: Effect.Effect<void> = Effect.gen(function* () {
      const chunk = yield* Effect.tryPromise(() => reader.read()).pipe(
        Effect.catch(() => Effect.succeed({ done: true as const, value: undefined })),
      )
      if (chunk.done) return
      const text = decoder.decode(chunk.value, { stream: true })
      yield* Effect.forEach(
        text.split("\n").map((line) => line.trim()).filter((line) => line !== ""),
        (line) => Ref.update(sink, (lines) => [...lines.slice(-19), line]),
        { discard: true },
      )
      return yield* loop
    })
    yield* loop
  })

export const processMarkerPids = (marker: string): ReadonlyArray<number> =>
  [...Deno.readDirSync("/proc")].flatMap((entry) => {
    if (!/^\d+$/u.test(entry.name)) return []
    try {
      const cmdline = new TextDecoder().decode(Deno.readFileSync(`/proc/${entry.name}/cmdline`))
      return cmdline.includes(marker) ? [Number(entry.name)] : []
    } catch {
      return []
    }
  })

export const startAgent = (
  config: ProcessArmConfig,
  slot: number,
  limits: AgentLimits,
  extraEnvironment?: Readonly<Record<string, string>>,
): Effect.Effect<ProcessAgent, ProbeError, Scope.Scope> =>
  Effect.gen(function* () {
    const unit = `kanbei-iso-${config.runId}-${slot}`
    const startedAt = nowMs()
    const args = [
      "--user",
      "--unit",
      unit,
      "--pipe",
      "--wait",
      "--quiet",
      "--property",
      `MemoryMax=${limits.memoryMaxMib}M`,
      "--property",
      "MemorySwapMax=0",
      "--property",
      `TasksMax=${limits.tasksMax}`,
      "--property",
      `CPUQuota=${AGENT_CPU_QUOTA_PERCENT}%`,
      "--property",
      `RuntimeMaxSec=${AGENT_WALL_CLOCK_SECONDS}s`,
      "--property",
      "KillMode=control-group",
      "--property",
      "TimeoutStopSec=2",
      ...Object.entries(extraEnvironment ?? {}).flatMap(([key, value]) => [
        "--setenv",
        `${key}=${value}`,
      ]),
      "sh",
      "-c",
      `ulimit -c 0; ulimit -n 256; ulimit -f 32768; exec '${Deno.execPath()}' run --allow-env --allow-run ${config.agentScript}`,
    ]

    const child = yield* Effect.acquireRelease(
      Effect.sync(() =>
        new Deno.Command("systemd-run", {
          args,
          stdin: "piped",
          stdout: "piped",
          stderr: "piped",
        }).spawn()
      ),
      (handle) =>
        Effect.gen(function* () {
          yield* stopUnit(unit)
          yield* Effect.tryPromise(() => handle.status).pipe(
            Effect.timeout("5 seconds"),
            Effect.catch(() => Effect.void),
          )
        }),
    )

    const stderrSink = yield* Ref.make<ReadonlyArray<string>>([])
    yield* Effect.forkScoped(drain(child.stderr, stderrSink))
    const channel = yield* makeLineChannel(child.stdout, child.stdin)
    const readyLine = yield* Effect.race(
      withTimeout(channel.take, 15_000, `${unit} ready`),
      Effect.tryPromise({
        try: () => child.status,
        catch: (error) => new ProbeError({ operation: unit, detail: String(error) }),
      }).pipe(
        Effect.flatMap((status) =>
          Ref.get(stderrSink).pipe(
            Effect.flatMap((lines) =>
              Effect.fail(
                new ProbeError({
                  operation: unit,
                  detail:
                    `agent exited before ready (code=${status.code} signal=${status.signal}): ${
                      lines.join(" | ")
                    }`,
                }),
              )
            ),
          )
        ),
      ),
    )
    const [tag = "", pidText = "0"] = readyLine.split(" ")
    if (tag !== "READY") {
      return yield* Effect.fail(
        new ProbeError({ operation: unit, detail: `unexpected startup line: ${readyLine}` }),
      )
    }
    const cgroupPath = yield* resolveCgroupPath(unit)
    const statusPromise = child.status
    const readyMs = nowMs() - startedAt
    return {
      unit,
      pid: Number(pidText),
      cgroupPath,
      readyMs,
      take: channel.take,
      send: channel.send,
      request: (line, prefix) =>
        Effect.gen(function* () {
          const sentAt = nowMs()
          yield* channel.send(line)
          const reply = yield* withTimeout(channel.take, 10_000, `${unit} ${line}`)
          if (!reply.startsWith(prefix)) {
            return yield* Effect.fail(
              new ProbeError({ operation: unit, detail: `expected ${prefix}, received ${reply}` }),
            )
          }
          return { reply, ms: nowMs() - sentAt }
        }),
      status: Effect.tryPromise({
        try: () => statusPromise,
        catch: (error) =>
          new ProbeError({
            operation: `${unit} status`,
            detail: error instanceof Error ? error.message : String(error),
          }),
      }),
      stderrText: Ref.get(stderrSink).pipe(Effect.map((lines) => lines.join(" | "))),
    }
  })

export const defaultLimits: AgentLimits = {
  memoryMaxMib: AGENT_MEMORY_MAX_MIB,
  tasksMax: AGENT_TASKS_MAX,
}
