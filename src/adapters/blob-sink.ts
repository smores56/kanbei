import { Effect, Layer, Option, Ref } from "effect"
import { sha256 } from "../evidence/digest.ts"
import { BlobSink, BlobSinkError } from "../kernel/blob-sink.ts"
import type { ContentDigest } from "../kernel/blob-sink.ts"

const digestOf = (bytes: Uint8Array): Effect.Effect<ContentDigest, BlobSinkError> =>
  sha256(bytes).pipe(
    Effect.mapError((error) =>
      new BlobSinkError({ operation: "write", digest: "", reason: error.reason })
    ),
  )

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
          Ref.get(store).pipe(
            Effect.map((current) => {
              const bytes = current[digest]
              return bytes === undefined ? Option.none() : Option.some(bytes)
            }),
          ),
      }
    }),
  )

const blobPath = (root: string, digest: ContentDigest): string =>
  `${root}/sha256/${digest.slice(7, 9)}/${digest.slice(9)}`

/** Local whole-object CAS; layout is an implementation detail behind BlobSink. */
export const fileBlobSinkLayer = (root: string): Layer.Layer<BlobSink> =>
  Layer.effect(
    BlobSink,
    Effect.succeed({
      put: (bytes: Uint8Array) =>
        digestOf(bytes).pipe(
          Effect.flatMap((digest) =>
            Effect.tryPromise({
              try: async () => {
                const target = blobPath(root, digest)
                const existing = await Deno.stat(target).catch(() => null)
                if (existing === null) {
                  await Deno.mkdir(target.slice(0, target.lastIndexOf("/")), { recursive: true })
                  await Deno.writeFile(target, bytes)
                }
                return digest
              },
              catch: (error) =>
                new BlobSinkError({
                  operation: "write",
                  digest,
                  reason: error instanceof Error ? error.message : String(error),
                }),
            })
          ),
        ),
      read: (digest: ContentDigest) =>
        Effect.tryPromise({
          try: () => Deno.readFile(blobPath(root, digest)),
          catch: (error) => error,
        }).pipe(
          Effect.map((bytes) => Option.some(bytes)),
          Effect.catch(() => Effect.succeed(Option.none())),
        ),
    }),
  )
