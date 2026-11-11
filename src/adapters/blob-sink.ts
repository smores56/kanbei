import { Effect, Layer, Option, Ref, Semaphore } from "effect"
import { sha256 } from "../evidence/digest.ts"
import { BlobSink, BlobSinkError } from "../kernel/blob-sink.ts"
import type { ContentDigest } from "../kernel/blob-sink.ts"

const DIGEST = /^sha256:[0-9a-f]{64}$/u

const digestOf = (bytes: Uint8Array): Effect.Effect<ContentDigest, BlobSinkError> =>
  sha256(bytes).pipe(
    Effect.mapError((error) =>
      new BlobSinkError({ operation: "write", digest: "", reason: error.reason })
    ),
  )

const invalidDigest = (digest: ContentDigest): BlobSinkError =>
  new BlobSinkError({
    operation: "read",
    digest,
    reason: "digest is not sha256:<64 lowercase hex>",
  })

/** In-memory CAS for tests and dry runs. */
export const memoryBlobSinkLayer = (): Layer.Layer<BlobSink> =>
  Layer.effect(
    BlobSink,
    Effect.gen(function* () {
      const store = yield* Ref.make<Readonly<Record<string, Uint8Array>>>({})
      return {
        put: (bytes: Uint8Array) =>
          digestOf(bytes).pipe(
            Effect.flatMap((digest) =>
              Ref.update(store, (current) => ({ ...current, [digest]: bytes })).pipe(
                Effect.as(digest),
              )
            ),
          ),
        read: (digest: ContentDigest) =>
          DIGEST.test(digest)
            ? Ref.get(store).pipe(
              Effect.map((current) => {
                const bytes = current[digest]
                return bytes === undefined ? Option.none() : Option.some(bytes)
              }),
            )
            : Effect.fail(invalidDigest(digest)),
        usage: Ref.get(store).pipe(
          Effect.map((current) =>
            Object.values(current).reduce((total, bytes) => total + bytes.length, 0)
          ),
        ),
      }
    }),
  )

/** One file per digest; the name is the full hex, so the mapping is injective. */
const blobPath = (root: string, digest: ContentDigest): string =>
  `${root}/sha256/${digest.slice(7)}`

const syncDirectory = (directory: string): void => {
  try {
    const handle = Deno.openSync(directory, { read: true })
    try {
      handle.syncSync()
    } finally {
      handle.close()
    }
  } catch {
    // Not every filesystem exposes a directory handle; the rename is still atomic.
  }
}

const writeDurable = (target: string, bytes: Uint8Array): void => {
  const directory = target.slice(0, target.lastIndexOf("/"))
  Deno.mkdirSync(directory, { recursive: true })
  const temporary = `${target}.${crypto.randomUUID()}.tmp`
  const file = Deno.openSync(temporary, { write: true, createNew: true, mode: 0o600 })
  try {
    file.writeSync(bytes)
    file.syncSync()
  } finally {
    file.close()
  }
  Deno.renameSync(temporary, target)
  // The rename is only durable once its directory entry is.
  syncDirectory(directory)
}

const usageOf = (root: string): number => {
  const walk = (directory: string): number => {
    let total = 0
    let entries: Iterable<Deno.DirEntry>
    try {
      entries = Deno.readDirSync(directory)
    } catch (error) {
      if (error instanceof Deno.errors.NotFound) return 0
      throw error
    }
    for (const entry of entries) {
      const path = `${directory}/${entry.name}`
      if (entry.isFile) total += Deno.statSync(path).size
      else if (entry.isDirectory) total += walk(path)
    }
    return total
  }
  return walk(`${root}/sha256`)
}

/** Local whole-object CAS; layout is an implementation detail behind BlobSink. */
export const fileBlobSinkLayer = (root: string): Layer.Layer<BlobSink> =>
  Layer.effect(
    BlobSink,
    Effect.gen(function* () {
      // One permit per digest: two writers racing the same object serialize,
      // while independent objects still publish concurrently.
      const locks = yield* Ref.make<ReadonlyMap<string, Semaphore.Semaphore>>(new Map())
      const lockFor = (digest: string): Effect.Effect<Semaphore.Semaphore> =>
        Ref.modify(locks, (current) => {
          const existing = current.get(digest)
          if (existing !== undefined) return [existing, current]
          const created = Semaphore.makeUnsafe(1)
          return [created, new Map([...current, [digest, created]])]
        })
      return {
        put: (bytes: Uint8Array) =>
          digestOf(bytes).pipe(
            Effect.flatMap((digest) =>
              lockFor(digest).pipe(
                Effect.flatMap((permit) =>
                  permit.withPermits(1)(
                    Effect.try({
                      try: () => {
                        const target = blobPath(root, digest)
                        try {
                          Deno.statSync(target)
                          return digest
                        } catch (error) {
                          if (!(error instanceof Deno.errors.NotFound)) throw error
                        }
                        writeDurable(target, bytes)
                        return digest
                      },
                      catch: (error) =>
                        new BlobSinkError({
                          operation: "write",
                          digest,
                          reason: error instanceof Error ? error.message : String(error),
                        }),
                    }),
                  )
                ),
              )
            ),
          ),
        read: (digest: ContentDigest) =>
          DIGEST.test(digest)
            ? Effect.tryPromise({
              try: () => Deno.readFile(blobPath(root, digest)),
              catch: (error) => error,
            }).pipe(
              Effect.map((bytes) => Option.some(bytes)),
              Effect.catch(() => Effect.succeed(Option.none())),
            )
            : Effect.fail(invalidDigest(digest)),
        usage: Effect.try({
          try: () => usageOf(root),
          catch: (error) =>
            new BlobSinkError({
              operation: "read",
              digest: "",
              reason: error instanceof Error ? error.message : String(error),
            }),
        }),
      }
    }),
  )
