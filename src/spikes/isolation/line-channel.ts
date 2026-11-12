import { Effect, Queue, Scope } from "effect"
import { ProbeError } from "./exec.ts"

const encoder = new TextEncoder()
const decoder = new TextDecoder()

export interface LineChannel {
  readonly take: Effect.Effect<string, ProbeError>
  readonly send: (line: string) => Effect.Effect<void, ProbeError>
}

const pump = (
  reader: ReadableStreamDefaultReader<Uint8Array>,
  queue: Queue.Queue<string>,
  buffer: string,
): Effect.Effect<void> =>
  Effect.gen(function* () {
    const chunk = yield* Effect.tryPromise({
      try: () => reader.read(),
      catch: (error) =>
        new ProbeError({
          operation: "read-line",
          detail: error instanceof Error ? error.message : String(error),
        }),
    }).pipe(Effect.catch(() => Effect.succeed({ done: true, value: undefined })))
    if (chunk.done) return
    const text = buffer + decoder.decode(chunk.value, { stream: true })
    const parts = text.split("\n")
    const rest = parts.pop() ?? ""
    yield* Effect.forEach(
      parts.map((line) => line.trim()).filter((line) => line !== ""),
      (line) => Queue.offer(queue, line),
      { discard: true },
    )
    return yield* pump(reader, queue, rest)
  })

/** Line-oriented pipe channel; adapter code owns the stream handles. */
export const makeLineChannel = (
  stdout: ReadableStream<Uint8Array>,
  stdin: WritableStream<Uint8Array>,
): Effect.Effect<LineChannel, ProbeError, Scope.Scope> =>
  Effect.gen(function* () {
    const queue = yield* Queue.unbounded<string>()
    const reader = stdout.getReader()
    const writer = stdin.getWriter()
    yield* Effect.addFinalizer(() =>
      Effect.gen(function* () {
        yield* Effect.tryPromise(() => reader.cancel()).pipe(Effect.catch(() => Effect.void))
        yield* Effect.tryPromise(() => writer.close()).pipe(Effect.catch(() => Effect.void))
      })
    )
    yield* Effect.forkScoped(pump(reader, queue, ""))
    return {
      take: Queue.take(queue),
      send: (line: string) =>
        Effect.tryPromise({
          try: async () => {
            await writer.write(encoder.encode(`${line}\n`))
          },
          catch: (error) =>
            new ProbeError({
              operation: "write-line",
              detail: error instanceof Error ? error.message : String(error),
            }),
        }),
    }
  })
