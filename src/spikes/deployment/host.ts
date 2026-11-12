import { Context, Effect, Layer, Option } from "effect"
import { CommandError } from "./errors.ts"

export interface CommandResult {
  readonly code: number
  readonly stdout: string
  readonly stderr: string
}

export interface RunOptions {
  readonly timeoutMs?: number
  readonly stdin?: string
  readonly env?: Readonly<Record<string, string>>
}

export interface ChildHandle {
  readonly pid: number
  readonly kill: (signal?: Deno.Signal) => Effect.Effect<void>
}

export interface HostCommandShape {
  readonly run: (
    program: string,
    args: ReadonlyArray<string>,
    options?: RunOptions,
  ) => Effect.Effect<CommandResult, CommandError>
  readonly locate: (name: string) => Effect.Effect<Option.Option<string>, CommandError>
  readonly spawn: (
    program: string,
    args: ReadonlyArray<string>,
    options?: Pick<RunOptions, "env">,
  ) => Effect.Effect<ChildHandle, CommandError>
}

export const HostCommand = Context.Service<HostCommandShape>("kanbei/spike/HostCommand")

const DEFAULT_TIMEOUT_MS = 30_000

const decode = (bytes: Uint8Array): string => new TextDecoder().decode(bytes)

const reasonOf = (error: unknown): string => error instanceof Error ? error.message : String(error)

const runProcess = async (
  program: string,
  args: ReadonlyArray<string>,
  options: RunOptions,
): Promise<CommandResult> => {
  const command = new Deno.Command(program, {
    args: [...args],
    stdout: "piped",
    stderr: "piped",
    signal: AbortSignal.timeout(options.timeoutMs ?? DEFAULT_TIMEOUT_MS),
    ...(options.stdin === undefined ? {} : { stdin: "piped" }),
    ...(options.env === undefined ? {} : { env: { ...options.env } }),
  })
  const child = command.spawn()
  if (options.stdin !== undefined) {
    const writer = child.stdin.getWriter()
    await writer.write(new TextEncoder().encode(options.stdin))
    await writer.close()
  }
  const output = await child.output()
  return {
    code: output.code,
    stdout: decode(output.stdout).trimEnd(),
    stderr: decode(output.stderr).trimEnd(),
  }
}

export const hostCommandLayer: Layer.Layer<HostCommandShape> = Layer.succeed(HostCommand, {
  run: (program, args, options) =>
    Effect.tryPromise({
      try: () => runProcess(program, args, options ?? {}),
      catch: (error) => new CommandError({ program, reason: reasonOf(error) }),
    }),
  locate: (name) =>
    Effect.tryPromise({
      try: async () => {
        const output = await runProcess("/bin/sh", ["-c", `command -v ${name}`], {
          timeoutMs: 5_000,
        })
        const first = output.stdout.split("\n")[0]?.trim() ?? ""
        return output.code === 0 && first !== "" ? Option.some(first) : Option.none()
      },
      catch: (error) => new CommandError({ program: "sh", reason: reasonOf(error) }),
    }),
  spawn: (program, args, options) =>
    Effect.tryPromise({
      try: () => {
        const child = new Deno.Command(program, {
          args: [...args],
          stdin: "null",
          stdout: "null",
          stderr: "null",
          ...(options?.env === undefined ? {} : { env: { ...options.env } }),
        }).spawn()
        return Promise.resolve({
          pid: child.pid,
          kill: (signal: Deno.Signal | undefined) =>
            Effect.sync(() => {
              try {
                child.kill(signal ?? "SIGKILL")
              } catch {
                // the child already exited
              }
            }),
        })
      },
      catch: (error) => new CommandError({ program, reason: reasonOf(error) }),
    }),
})
