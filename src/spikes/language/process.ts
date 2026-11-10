import { Context, Effect, Layer } from "effect"
import { CommandError } from "./errors.ts"

export interface CommandSpec {
  readonly argv: ReadonlyArray<string>
  readonly cwd?: string
  readonly env?: Readonly<Record<string, string>>
  readonly stdin?: string
  readonly timeoutMs: number
  readonly sampleRss?: boolean
}

export interface CommandOutcome {
  readonly exitCode: number | null
  readonly stdout: string
  readonly stderr: string
  readonly timedOut: boolean
  readonly wallMs: number
  readonly peakRssBytes: number
  readonly rssSampled: boolean
}

export interface CommandRunner {
  readonly run: (spec: CommandSpec) => Effect.Effect<CommandOutcome, CommandError>
}

export const CommandRunner = Context.Service<CommandRunner>("kanbei/spike/language/CommandRunner")

const text = (stream: ReadableStream<Uint8Array>): Promise<string> => new Response(stream).text()

const readPeakRss = async (pid: number): Promise<number | undefined> => {
  try {
    const status = await Deno.readTextFile(`/proc/${pid}/status`)
    const match = /VmHWM:\s+(\d+)\s+kB/u.exec(status)
    if (match === null) return undefined
    const matched = match[1]
    return matched === undefined ? undefined : Number(matched) * 1024
  } catch {
    // The process may exit between samples; the high-water mark is monotonic, so a miss is safe.
    return undefined
  }
}

const runMeasured = async (spec: CommandSpec): Promise<CommandOutcome> => {
  const [program, ...args] = spec.argv
  if (program === undefined) throw new Error("command argv is empty")

  const startedAt = performance.now()
  const child = new Deno.Command(program, {
    args,
    ...(spec.cwd === undefined ? {} : { cwd: spec.cwd }),
    env: {
      PATH: Deno.env.get("PATH") ?? "/usr/local/bin:/usr/bin:/bin",
      HOME: Deno.env.get("HOME") ?? "/tmp",
      LANG: "C.UTF-8",
      ...(Deno.env.get("XDG_CACHE_HOME") === undefined
        ? {}
        : { XDG_CACHE_HOME: Deno.env.get("XDG_CACHE_HOME") ?? "" }),
      ...(Deno.env.get("TMPDIR") === undefined ? {} : { TMPDIR: Deno.env.get("TMPDIR") ?? "" }),
      ...spec.env,
    },
    clearEnv: true,
    stdin: "piped",
    stdout: "piped",
    stderr: "piped",
  }).spawn()

  const stdinWriter = (() => {
    if (spec.stdin === undefined) {
      child.stdin.close()
      return Promise.resolve()
    }
    const writer = child.stdin.getWriter()
    return writer
      .write(new TextEncoder().encode(spec.stdin))
      .then(() => writer.close())
  })()

  let timedOut = false
  let peakRssBytes = 0
  let rssSampled = false

  const timer = setTimeout(() => {
    timedOut = true
    try {
      child.kill("SIGKILL")
    } catch {
      // Already exited.
    }
  }, spec.timeoutMs)

  const sampler = spec.sampleRss === true
    ? setInterval(() => {
      void readPeakRss(child.pid).then((peak) => {
        if (peak !== undefined) {
          rssSampled = true
          peakRssBytes = Math.max(peakRssBytes, peak)
        }
        return undefined
      })
    }, 5)
    : undefined

  const stdoutPromise = text(child.stdout)
  const stderrPromise = text(child.stderr)

  const [status, stdout, stderr] = await Promise.all([
    child.status,
    stdoutPromise,
    stderrPromise,
    stdinWriter,
  ])
  clearTimeout(timer)
  if (sampler !== undefined) clearInterval(sampler)

  return {
    exitCode: status.code,
    stdout,
    stderr,
    timedOut,
    wallMs: performance.now() - startedAt,
    peakRssBytes,
    rssSampled,
  }
}

export const liveCommandRunner: CommandRunner = {
  run: (spec) =>
    Effect.tryPromise({
      try: () => runMeasured(spec),
      catch: (error) =>
        new CommandError({
          argv: spec.argv,
          reason: error instanceof Error ? error.message : String(error),
        }),
    }),
}

export const liveCommandRunnerLayer = Layer.succeed(CommandRunner, liveCommandRunner)

export const percentile = (values: ReadonlyArray<number>, fraction: number): number => {
  if (values.length === 0) return 0
  const sorted = values.toSorted((a, b) => a - b)
  const index = Math.min(sorted.length - 1, Math.max(0, Math.ceil(fraction * sorted.length) - 1))
  return sorted[index] ?? 0
}
