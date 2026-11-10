import { Effect, Option } from "effect"
import type { CommandResult } from "./host.ts"
import { recover } from "./recover.ts"
import { ProbeEnv } from "./probe.ts"
import type { ProbeEnvShape } from "./probe.ts"

export interface UnitInvocation {
  readonly properties?: ReadonlyArray<string>
  readonly unit?: string
  readonly timeoutMs?: number
}

/**
 * Runs a transient user unit through `systemd-run --user --wait --pipe`.
 * Transient units are the only unit form this unprivileged run may create.
 */
export const runUserUnit = (
  invocation: UnitInvocation,
  program: string,
  args: ReadonlyArray<string>,
): Effect.Effect<CommandResult, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const systemdRun = yield* env.command.locate("systemd-run")
    if (Option.isNone(systemdRun)) {
      return { code: 127, stdout: "", stderr: "systemd-run is not installed" }
    }
    return yield* env.command
      .run(
        systemdRun.value,
        [
          "--user",
          "--wait",
          "--pipe",
          "--quiet",
          ...(invocation.unit === undefined ? [] : [`--unit=${invocation.unit}`]),
          ...(invocation.properties ?? []).flatMap((property) => ["--property", property]),
          program,
          ...args,
        ],
        { timeoutMs: invocation.timeoutMs ?? 25_000 },
      )
      .pipe(recover((error) => Effect.succeed({ code: 125, stdout: "", stderr: error.reason })))
  })

/** Reads the effective properties of a live transient unit. */
export const showProperties = (
  unit: string,
  properties: ReadonlyArray<string>,
): Effect.Effect<Readonly<Record<string, string>>, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const systemctl = yield* env.command.locate("systemctl")
    if (Option.isNone(systemctl)) return {}
    const result = yield* env.command
      .run(systemctl.value, [
        "--user",
        "show",
        unit,
        ...properties.flatMap((property) => ["-p", property]),
      ], { timeoutMs: 10_000 })
      .pipe(recover(() => Effect.succeed({ code: 1, stdout: "", stderr: "" })))
    return Object.fromEntries(
      result.stdout
        .split("\n")
        .filter((line) => line.includes("="))
        .map((line) => {
          const index = line.indexOf("=")
          return [line.slice(0, index), line.slice(index + 1)]
        }),
    )
  })

export const stopUnit = (unit: string): Effect.Effect<void, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const systemctl = yield* env.command.locate("systemctl")
    if (Option.isNone(systemctl)) return
    yield* env.command
      .run(systemctl.value, ["--user", "stop", unit], { timeoutMs: 10_000 })
      .pipe(Effect.ignore)
    yield* env.command
      .run(systemctl.value, ["--user", "reset-failed", unit], { timeoutMs: 10_000 })
      .pipe(Effect.ignore)
  })
