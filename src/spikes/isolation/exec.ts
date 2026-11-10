import { Data, Effect } from "effect"

export class ProbeError extends Data.TaggedError("ProbeError")<{
  readonly operation: string
  readonly detail: string
}> {}

export interface CommandOutput {
  readonly code: number
  readonly signal: string | null
  readonly stdout: string
  readonly stderr: string
}

const detailOf = (
  error: unknown,
): string => (error instanceof Error ? error.message : String(error))

export const runCommand = (
  command: string,
  args: ReadonlyArray<string>,
): Effect.Effect<CommandOutput, ProbeError> =>
  Effect.tryPromise({
    try: async () => {
      const output = await new Deno.Command(command, {
        args: [...args],
        stdin: "null",
        stdout: "piped",
        stderr: "piped",
      }).output()
      return {
        code: output.code,
        signal: output.signal,
        stdout: new TextDecoder().decode(output.stdout),
        stderr: new TextDecoder().decode(output.stderr),
      }
    },
    catch: (error) => new ProbeError({ operation: command, detail: detailOf(error) }),
  })

export const runShell = (script: string): Effect.Effect<CommandOutput, ProbeError> =>
  runCommand("sh", ["-c", script])

/** Reads a host file, returning `None`-like `null` when the file does not exist. */
export const readTextFile = (path: string): Effect.Effect<string | null, ProbeError> =>
  Effect.tryPromise({
    try: async () => {
      try {
        return await Deno.readTextFile(path)
      } catch (error) {
        if (error instanceof Deno.errors.NotFound) return null
        throw error
      }
    },
    catch: (error) => new ProbeError({ operation: `read ${path}`, detail: detailOf(error) }),
  })

export const readNumericFile = (path: string): Effect.Effect<number | null, ProbeError> =>
  readTextFile(path).pipe(
    Effect.map((text) => {
      if (text === null) return null
      const value = Number(text.trim().split(/\s+/u)[0])
      return Number.isFinite(value) ? value : null
    }),
  )

export const pathExists = (path: string): Effect.Effect<boolean, ProbeError> =>
  Effect.tryPromise({
    try: () => Deno.stat(path).then(() => true).catch(() => false),
    catch: (error) => new ProbeError({ operation: `stat ${path}`, detail: detailOf(error) }),
  })

/** Wall-clock milliseconds for measurement adapters; domain code uses `Clock`. */
export const nowMs = (): number => performance.now()

/** Fails with `ProbeError` instead of a timeout cause, so callers keep one error channel. */
export const withTimeout = <A>(
  effect: Effect.Effect<A, ProbeError>,
  milliseconds: number,
  operation: string,
): Effect.Effect<A, ProbeError> =>
  effect.pipe(
    Effect.timeout(`${milliseconds} millis`),
    Effect.catch((error) =>
      Effect.fail(
        error instanceof ProbeError
          ? error
          : new ProbeError({ operation, detail: `timed out after ${milliseconds} ms` }),
      )
    ),
  )
