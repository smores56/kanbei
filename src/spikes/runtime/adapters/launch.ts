import { Effect } from "effect"

/**
 * Boundary adapter for the process-level measurements.
 *
 * The driver is the harness host: it spawns each arm as a fresh process, watches
 * stdout for the arm's readiness marker, and times spawn→marker→exit from
 * outside the arm. Deno APIs stay here so the rest of the driver is portable.
 * Every function resolves to a fallback instead of failing: the driver must
 * record a blocked arm, not crash on it.
 */
export interface LaunchSpec {
  readonly command: string
  readonly args: ReadonlyArray<string>
  readonly env?: Readonly<Record<string, string>>
}

export interface LaunchOutcome {
  readonly code: number
  readonly stdout: string
  readonly stderr: string
  readonly readyDelayMs: number
  readonly totalMs: number
  readonly signalSent: boolean
}

export interface LaunchOptions {
  /** When set, the harness sends this signal once the arm prints its ready marker. */
  readonly signalAfterReady?: "SIGTERM"
  readonly readyMarker?: string
  readonly timeoutMs?: number
}

const decoder = new TextDecoder()
const READY = "@@READY@@"

const environment = (spec: LaunchSpec): Record<string, string> =>
  spec.env === undefined ? Deno.env.toObject() : { ...Deno.env.toObject(), ...spec.env }

const failed: LaunchOutcome = {
  code: -1,
  stdout: "",
  stderr: "spawn failed",
  readyDelayMs: -1,
  totalMs: 0,
  signalSent: false,
}

export const launch = (
  spec: LaunchSpec,
  options: LaunchOptions,
  now: () => number,
): Effect.Effect<LaunchOutcome> =>
  Effect.promise(async () => {
    try {
      const startedAt = now()
      const readyMarker = options.readyMarker ?? READY
      const child = new Deno.Command(spec.command, {
        args: [...spec.args],
        env: environment(spec),
        stdin: "null",
        stdout: "piped",
        stderr: "piped",
      }).spawn()

      const timer = setTimeout(() => {
        try {
          child.kill("SIGKILL")
        } catch {
          // Already exited.
        }
      }, options.timeoutMs ?? 180_000)

      const state = { stdout: "", stderr: "", readyAt: 0, signalSent: false }
      const watch = async (stream: ReadableStream<Uint8Array>, stdout: boolean) => {
        const reader = stream.getReader()
        for (;;) {
          // oxlint-disable-next-line eslint/no-await-in-loop -- stdout must be drained chunk by chunk while the child runs
          const chunk = await reader.read()
          if (chunk.done) break
          const text = decoder.decode(chunk.value)
          if (stdout) {
            state.stdout += text
            if (state.readyAt === 0 && state.stdout.includes(readyMarker)) {
              state.readyAt = now()
              if (options.signalAfterReady !== undefined) {
                child.kill(options.signalAfterReady)
                state.signalSent = true
              }
            }
          } else {
            state.stderr += text
          }
        }
      }

      const [status] = await Promise.all([
        child.status,
        watch(child.stdout, true),
        watch(child.stderr, false),
      ])
      clearTimeout(timer)
      return {
        code: status.code,
        stdout: state.stdout,
        stderr: state.stderr,
        readyDelayMs: state.readyAt === 0 ? -1 : state.readyAt - startedAt,
        totalMs: now() - startedAt,
        signalSent: state.signalSent,
      }
    } catch {
      return failed
    }
  })

export const capture = (spec: LaunchSpec): Effect.Effect<string> =>
  Effect.promise(async () => {
    try {
      const output = await new Deno.Command(spec.command, {
        args: [...spec.args],
        env: environment(spec),
        stdout: "piped",
        stderr: "piped",
      }).output()
      return decoder.decode(output.stdout)
    } catch {
      return ""
    }
  })

export const fileSize = (path: string): Effect.Effect<number> =>
  Effect.promise(async () => {
    try {
      return (await Deno.stat(path)).size
    } catch {
      return 0
    }
  })

export const readText = (path: string): Effect.Effect<string> =>
  Effect.promise(async () => {
    try {
      return await Deno.readTextFile(path)
    } catch {
      return ""
    }
  })

/** Synchronous read for callers that already sit inside `Effect.sync`. */
export const readTextSync = (path: string): string => {
  try {
    return Deno.readTextFileSync(path)
  } catch {
    return ""
  }
}

export const writeText = (path: string, text: string): Effect.Effect<void> =>
  Effect.promise(async () => {
    try {
      await Deno.writeTextFile(path, text)
    } catch {
      // The caller validates afterwards.
    }
  })

export const ensureDirectory = (path: string): Effect.Effect<void> =>
  Effect.promise(async () => {
    try {
      await Deno.mkdir(path, { recursive: true })
    } catch {
      // Existing directory.
    }
  })

export const removeIfExists = (path: string): Effect.Effect<void> =>
  Effect.promise(async () => {
    try {
      await Deno.remove(path, { recursive: true })
    } catch {
      // Absent.
    }
  })

export const directoryBytes = (path: string): Effect.Effect<number> =>
  Effect.promise(async () => {
    try {
      const output = await new Deno.Command("du", {
        args: ["-sb", path],
        stdout: "piped",
        stderr: "null",
      }).output()
      const text = decoder.decode(output.stdout).trim()
      const value = Number.parseInt(text.split(/\s+/u)[0] ?? "0", 10)
      return Number.isFinite(value) ? value : 0
    } catch {
      return 0
    }
  })
