import { assertEquals } from "@std/assert"
import { Effect, Layer, Option } from "effect"
import { fileCertificateStoreLayer } from "../../src/adapters/csr-store.ts"
import type { InstalledCertificate } from "../../src/channels/csr/store.ts"
import { CertificateStore } from "../../src/channels/csr/store.ts"
import { withTempDir } from "../support.ts"

const certificate: InstalledCertificate = {
  chainPem:
    "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n",
  leafPem: "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n",
  privateKeyPkcs8: "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQg",
  spki: "MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE",
  notAfter: "2026-04-01T00:00:00.000Z",
  installedAt: "2026-01-01T00:00:00.000Z",
}

const onStore = <A, E>(
  layer: Layer.Layer<CertificateStore>,
  effect: Effect.Effect<A, E, CertificateStore>,
): Promise<A> => Effect.runPromise(effect.pipe(Effect.provide(layer), Effect.orDie))

Deno.test("the file store stages a pair and keeps the old one until promote", async () => {
  await withTempDir("csr-store", async (root) => {
    const layer = fileCertificateStoreLayer(root)
    const outcome = await onStore(
      layer,
      Effect.gen(function* () {
        const store = yield* CertificateStore
        const empty = yield* store.active
        yield* store.stage(certificate)
        const staged = yield* store.staged
        const before = yield* store.active
        const promoted = yield* store.promote
        const after = yield* store.active
        return { empty, staged, before, promoted, after }
      }),
    )
    assertEquals(Option.isNone(outcome.empty), true)
    assertEquals(Option.isSome(outcome.staged), true)
    assertEquals(Option.isNone(outcome.before), true)
    assertEquals(Option.isSome(outcome.promoted), true)
    assertEquals(outcome.after, Option.some(certificate))
  })
})

Deno.test("the active pair survives a new layer over the same directory", async () => {
  await withTempDir("csr-store", async (root) => {
    await onStore(
      fileCertificateStoreLayer(root),
      Effect.gen(function* () {
        const store = yield* CertificateStore
        yield* store.stage(certificate)
        yield* store.promote
      }),
    )
    const active = await onStore(
      fileCertificateStoreLayer(root),
      Effect.gen(function* () {
        const store = yield* CertificateStore
        return yield* store.active
      }),
    )
    assertEquals(active, Option.some(certificate))
  })
})
