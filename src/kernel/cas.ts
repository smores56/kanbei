import { Context, Data, Effect, Layer, Option, Schema } from "effect"
import { canonicalize } from "../evidence/canonical.ts"
import { sha256 } from "../evidence/digest.ts"
import { BlobSink } from "./blob-sink.ts"

export const ContentManifest = Schema.Struct({
  digest: Schema.String,
  size: Schema.Number,
  mediaType: Schema.String,
})
export type ContentManifest = typeof ContentManifest.Type

export type CasErrorCode =
  | "missing_blob"
  | "truncated_blob"
  | "tampered_blob"
  | "manifest_invalid"
  | "store_failed"

export class CasError extends Data.TaggedError("CasError")<{
  readonly code: CasErrorCode
  readonly digest: string
  readonly detail: string
}> {}

export interface PublishedContent {
  readonly manifest: ContentManifest
  readonly manifestDigest: string
}

/**
 * Whole-object CAS plus its manifests. Bytes are durable before the manifest
 * becomes visible, and the manifest is durable before a ledger append can
 * reference it (`I-08`).
 */
export interface ContentStore {
  readonly publish: (
    bytes: Uint8Array,
    mediaType: string,
  ) => Effect.Effect<PublishedContent, CasError>
  readonly read: (
    digest: string,
    expectedSize: number,
  ) => Effect.Effect<Uint8Array, CasError>
  readonly readManifest: (manifestDigest: string) => Effect.Effect<ContentManifest, CasError>
}

export const ContentStore = Context.Service<ContentStore>("kanbei/kernel/ContentStore")

const manifestBytes = (manifest: ContentManifest): Uint8Array =>
  new TextEncoder().encode(
    canonicalize(Schema.decodeUnknownSync(Schema.Json)(manifest)),
  )

const storedBytes = (
  sink: BlobSink["read"],
  digest: string,
): Effect.Effect<Uint8Array, CasError> =>
  Effect.gen(function* () {
    const stored = yield* sink(digest).pipe(
      Effect.mapError(
        (error) => new CasError({ code: "store_failed", digest, detail: error.reason }),
      ),
    )
    if (Option.isNone(stored)) {
      return yield* Effect.fail(new CasError({ code: "missing_blob", digest, detail: "absent" }))
    }
    return stored.value
  })

const readVerified = (
  sink: BlobSink["read"],
  digest: string,
): Effect.Effect<Uint8Array, CasError> =>
  storedBytes(sink, digest).pipe(
    Effect.flatMap((bytes) =>
      sha256(bytes).pipe(
        Effect.orDie,
        Effect.flatMap((actual) =>
          actual === digest ? Effect.succeed(bytes) : Effect.fail(
            new CasError({ code: "tampered_blob", digest, detail: `hashes to ${actual}` }),
          )
        ),
      )
    ),
  )

export const contentStoreLayer: Layer.Layer<ContentStore, never, BlobSink> = Layer.effect(
  ContentStore,
  Effect.gen(function* () {
    const sink = yield* BlobSink
    return {
      publish: (bytes, mediaType) =>
        Effect.gen(function* () {
          const digest = yield* sink.put(bytes).pipe(
            Effect.mapError(
              (error) => new CasError({ code: "store_failed", digest: "", detail: error.reason }),
            ),
          )
          const manifest: ContentManifest = { digest, size: bytes.length, mediaType }
          const manifestDigest = yield* sink.put(manifestBytes(manifest)).pipe(
            Effect.mapError(
              (error) => new CasError({ code: "store_failed", digest, detail: error.reason }),
            ),
          )
          return { manifest, manifestDigest }
        }),
      read: (digest, expectedSize) =>
        storedBytes(sink.read, digest).pipe(
          Effect.flatMap((bytes) =>
            bytes.length < expectedSize
              ? Effect.fail(
                new CasError({
                  code: "truncated_blob",
                  digest,
                  detail: `${bytes.length} of ${expectedSize} bytes`,
                }),
              )
              : bytes.length > expectedSize
              ? Effect.fail(
                new CasError({
                  code: "tampered_blob",
                  digest,
                  detail: `${bytes.length} bytes exceeds manifest size ${expectedSize}`,
                }),
              )
              : sha256(bytes).pipe(
                Effect.orDie,
                Effect.flatMap((actual) =>
                  actual === digest ? Effect.succeed(bytes) : Effect.fail(
                    new CasError({
                      code: "tampered_blob",
                      digest,
                      detail: `hashes to ${actual}`,
                    }),
                  )
                ),
              )
          ),
        ),
      readManifest: (manifestDigest) =>
        readVerified(sink.read, manifestDigest).pipe(
          Effect.flatMap((bytes) =>
            Effect.try({
              try: () =>
                Schema.decodeUnknownSync(ContentManifest, { onExcessProperty: "error" })(
                  JSON.parse(new TextDecoder().decode(bytes)),
                ),
              catch: (error) =>
                new CasError({
                  code: "manifest_invalid",
                  digest: manifestDigest,
                  detail: error instanceof Error ? error.message : String(error),
                }),
            })
          ),
        ),
    }
  }),
)
