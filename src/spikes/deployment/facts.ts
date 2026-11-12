import { Effect, Option } from "effect"
import type { ProbeEnvShape } from "./probe.ts"
import { recover } from "./recover.ts"
import { ProbeEnv, unavailable } from "./probe.ts"
import type { CommandResult } from "./host.ts"

export const EVIDENCE_LIMIT = 600

export const truncate = (text: string, limit = EVIDENCE_LIMIT): string =>
  text.length <= limit ? text : `${text.slice(0, limit)}…`

/** Reads a `/proc/sys` value by dotted key, e.g. `kernel.yama.ptrace_scope`. */
export const readSysctl = (key: string): Effect.Effect<string, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const target = `/proc/sys/${key.replaceAll(".", "/")}`
    const exists = yield* env.fs.exists(target)
    if (!exists) {
      return yield* unavailable(`${key} is not exposed by this kernel`, target)
    }
    const value = yield* env.fs.readFileString(target)
    return value.trim()
  })

export const readProc = (target: string) =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const exists = yield* env.fs.exists(target)
    if (!exists) {
      return yield* unavailable(`${target} is not readable or does not exist`, target)
    }
    return (yield* env.fs.readFileString(target)).trim()
  })

export const readProcOr = (
  target: string,
  fallback: string,
): Effect.Effect<string, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const exists = yield* env.fs.exists(target).pipe(recover(() => Effect.succeed(false)))
    if (!exists) return fallback
    return yield* env.fs.readFileString(target).pipe(
      Effect.map((value) => value.trim()),
      recover(() => Effect.succeed(fallback)),
    )
  })

export const ensureDir = (
  ...segments: ReadonlyArray<string>
): Effect.Effect<string, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const target = env.path.join(env.scratch, ...segments)
    yield* env.fs.makeDirectory(target, { recursive: true })
    return target
  })

export const optionalProgram = (
  name: string,
): Effect.Effect<Option.Option<string>, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    return yield* env.command.locate(name).pipe(recover(() => Effect.succeed(Option.none())))
  })

export const requireProgram = (name: string): Effect.Effect<string, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const found = yield* optionalProgram(name)
    if (Option.isNone(found)) {
      return yield* unavailable(`required program ${name} is not installed`)
    }
    return found.value
  })

export const describe = (result: CommandResult): string =>
  truncate(
    [`exit=${result.code}`, result.stdout, result.stderr]
      .filter((part) => part !== "")
      .join(" | "),
  )

export const filesIn = (
  dir: string,
): Effect.Effect<ReadonlyArray<string>, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const entries = yield* env.fs.readDirectory(dir).pipe(
      recover(() => Effect.succeed([] as ReadonlyArray<string>)),
    )
    return entries.toSorted()
  })

export const fileMode = (target: string): Effect.Effect<string, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    return yield* env.fs.stat(target).pipe(
      Effect.map((info) =>
        info.mode === undefined ? "unknown" : `0o${info.mode.toString(8).padStart(4, "0")}`
      ),
      recover(() => Effect.succeed("absent")),
    )
  })

export const canWriteFile = (
  target: string,
  content: string,
): Effect.Effect<boolean, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    return yield* env.fs.writeFileString(target, content).pipe(
      Effect.as(true),
      recover(() => Effect.succeed(false)),
    )
  })

export const canReadFile = (target: string): Effect.Effect<boolean, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    return yield* env.fs.readFile(target).pipe(
      Effect.as(true),
      recover(() => Effect.succeed(false)),
    )
  })

export const decodeText = (bytes: Uint8Array): string => new TextDecoder().decode(bytes)

/** Parses interface names from `/proc/net/dev`, ignoring the two header lines. */
export const interfaceNames = (procNetDev: string): ReadonlyArray<string> =>
  procNetDev
    .split("\n")
    .slice(2)
    .map((line) => line.split(":")[0]?.trim() ?? "")
    .filter((name) => /^[A-Za-z0-9_.@-]+$/u.test(name))

/** Last non-empty line of command output; tooling frequently reports on it. */
export const lastLine = (text: string): string | undefined =>
  text.split("\n").map((line) => line.trim()).toReversed().find((line) => line !== "")
