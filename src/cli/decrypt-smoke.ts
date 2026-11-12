/**
 * Packaging smoke entrypoint: decrypt one ciphertext through the packaged age
 * closure. The installed wrapper grants `--allow-run=age` and puts `age` on
 * PATH, so this subcommand is the end-to-end proof that a packaged kernel can
 * read an ingest batch. Ciphertext arrives on stdin because the wrapper's read
 * roots are the VM mounts, not the build sandbox's temporary directory.
 */
import { Data, Effect } from "effect"
import { ageDecryptorLayer } from "../adapters/age.ts"
import { AgeDecryptor } from "../channels/inbox/age.ts"

export const SMOKE_MAX_BYTES = 2 * 1024 * 1024

export class SmokeError extends Data.TaggedError("SmokeError")<{
  readonly reason: string
}> {}

export interface SmokeIo {
  readonly readStdin: Effect.Effect<Uint8Array, SmokeError>
  readonly writeStdout: (bytes: Uint8Array) => Effect.Effect<void, SmokeError>
}

const readChunks = async (
  chunks: Array<Uint8Array>,
  total: number,
): Promise<Uint8Array> => {
  const chunk = new Uint8Array(65_536)
  const read = await Deno.stdin.read(chunk)
  if (read === null) {
    const bytes = new Uint8Array(total)
    let offset = 0
    for (const part of chunks) {
      bytes.set(part, offset)
      offset += part.length
    }
    return bytes
  }
  const next = total + read
  if (next > SMOKE_MAX_BYTES) throw new Error(`stdin exceeds ${SMOKE_MAX_BYTES} bytes`)
  return readChunks([...chunks, chunk.subarray(0, read)], next)
}

const readAll = (): Promise<Uint8Array> => readChunks([], 0)

export const denoSmokeIo: SmokeIo = {
  readStdin: Effect.tryPromise({
    try: readAll,
    catch: (error) =>
      new SmokeError({ reason: error instanceof Error ? error.message : String(error) }),
  }),
  writeStdout: (bytes) =>
    Effect.tryPromise({
      try: async () => {
        await Deno.stdout.write(bytes)
      },
      catch: (error) =>
        new SmokeError({ reason: error instanceof Error ? error.message : String(error) }),
    }),
}

export const identityOf = (args: ReadonlyArray<string>): Effect.Effect<string, SmokeError> => {
  const index = args.indexOf("--identity")
  const value = index === -1 ? undefined : args[index + 1]
  return value === undefined || value === ""
    ? Effect.fail(new SmokeError({ reason: "usage: decrypt-smoke --identity <path>" }))
    : Effect.succeed(value)
}

/** The packaged decrypt path, with the adapter boundary left to the caller. */
export const decryptSmoke = (
  identityPath: string,
  ciphertext: Uint8Array,
): Effect.Effect<Uint8Array, SmokeError> =>
  Effect.gen(function* () {
    const decryptor = yield* AgeDecryptor
    return yield* decryptor.decrypt(ciphertext).pipe(
      Effect.mapError((error) => new SmokeError({ reason: error.reason })),
    )
  }).pipe(Effect.provide(ageDecryptorLayer({ identityPath })))

export const runDecryptSmoke = (
  args: ReadonlyArray<string>,
  io: SmokeIo = denoSmokeIo,
): Promise<number> =>
  Effect.runPromise(
    Effect.gen(function* () {
      const identityPath = yield* identityOf(args)
      const ciphertext = yield* io.readStdin
      const plaintext = yield* decryptSmoke(identityPath, ciphertext)
      yield* io.writeStdout(plaintext)
      return 0
    }).pipe(
      Effect.catch((error: SmokeError) =>
        Effect.sync(() => {
          Deno.stderr.writeSync(new TextEncoder().encode(`decrypt-smoke: ${error.reason}\n`))
          return 2
        })
      ),
    ),
  )
