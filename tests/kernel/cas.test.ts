import { assertEquals } from "@std/assert"
import { Effect, Layer } from "effect"
import { fileBlobSinkLayer } from "../../src/adapters/blob-sink.ts"
import { CasError, ContentStore, contentStoreLayer } from "../../src/kernel/cas.ts"
import { withTempDir } from "../support.ts"

const blobPath = (root: string, digest: string): string =>
  `${root}/sha256/${digest.slice(7, 9)}/${digest.slice(9)}`

const withCas = <A, E>(
  root: string,
  effect: Effect.Effect<A, E, ContentStore>,
): Promise<A> =>
  Effect.runPromise(
    effect.pipe(Effect.provide(contentStoreLayer.pipe(Layer.provide(fileBlobSinkLayer(root))))),
  )

const casFailure = <A, E extends CasError>(
  root: string,
  effect: Effect.Effect<A, E, ContentStore>,
): Promise<CasError> =>
  Effect.runPromise(
    effect.pipe(
      Effect.provide(contentStoreLayer.pipe(Layer.provide(fileBlobSinkLayer(root)))),
      Effect.flip,
    ),
  )

const encoder = new TextEncoder()

Deno.test("publish stores bytes behind a manifest that reads back", async () => {
  await withTempDir("cas", async (root) => {
    const result = await withCas(
      root,
      Effect.gen(function* () {
        const cas = yield* ContentStore
        const published = yield* cas.publish(encoder.encode("hello world"), "text/plain")
        const bytes = yield* cas.read(published.manifest.digest, published.manifest.size)
        const manifest = yield* cas.readManifest(published.manifestDigest)
        return { published, bytes, manifest }
      }),
    )
    assertEquals(result.published.manifest.size, 11)
    assertEquals(result.published.manifest.mediaType, "text/plain")
    assertEquals(new TextDecoder().decode(result.bytes), "hello world")
    assertEquals(result.manifest, result.published.manifest)
  })
})

Deno.test("read refuses a blob truncated below the manifest size", async () => {
  await withTempDir("cas", async (root) => {
    const digest = await withCas(
      root,
      Effect.gen(function* () {
        const cas = yield* ContentStore
        const published = yield* cas.publish(encoder.encode("hello world"), "text/plain")
        yield* Effect.promise(() =>
          Deno.writeFile(blobPath(root, published.manifest.digest), encoder.encode("hello"))
        )
        return published.manifest.digest
      }),
    )
    const error = await casFailure(
      root,
      Effect.gen(function* () {
        const cas = yield* ContentStore
        return yield* cas.read(digest, 11)
      }),
    )
    assertEquals(error.code, "truncated_blob")
  })
})

Deno.test("read refuses same-length bytes that hash differently", async () => {
  await withTempDir("cas", async (root) => {
    const digest = await withCas(
      root,
      Effect.gen(function* () {
        const cas = yield* ContentStore
        const published = yield* cas.publish(encoder.encode("hello world"), "text/plain")
        yield* Effect.promise(() =>
          Deno.writeFile(blobPath(root, published.manifest.digest), encoder.encode("HELLO WORLD"))
        )
        return published.manifest.digest
      }),
    )
    const error = await casFailure(
      root,
      Effect.gen(function* () {
        const cas = yield* ContentStore
        return yield* cas.read(digest, 11)
      }),
    )
    assertEquals(error.code, "tampered_blob")
  })
})

Deno.test("read refuses a missing blob", async () => {
  await withTempDir("cas", async (root) => {
    const digest = `sha256:${"a".repeat(64)}`
    const error = await casFailure(
      root,
      Effect.gen(function* () {
        const cas = yield* ContentStore
        return yield* cas.read(digest, 1)
      }),
    )
    assertEquals(error.code, "missing_blob")
  })
})

Deno.test("identical bytes publish one object and one manifest", async () => {
  await withTempDir("cas", async (root) => {
    const result = await withCas(
      root,
      Effect.gen(function* () {
        const cas = yield* ContentStore
        const first = yield* cas.publish(encoder.encode("same"), "text/plain")
        const second = yield* cas.publish(encoder.encode("same"), "text/plain")
        return { first, second }
      }),
    )
    assertEquals(result.first.manifest.digest, result.second.manifest.digest)
    assertEquals(result.first.manifestDigest, result.second.manifestDigest)
  })
})
