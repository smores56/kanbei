import { Effect, Layer, Option, Schema } from "effect"
import { CertificateStore, CsrStoreError } from "../channels/csr/store.ts"
import type { InstalledCertificate } from "../channels/csr/store.ts"

const StoredCertificate = Schema.Struct({
  installedAt: Schema.String,
  privateKeyPkcs8: Schema.String,
  leaf: Schema.Struct({
    subject: Schema.Literal("kanbei.sammohr.dev"),
    issuer: Schema.String,
    spki: Schema.String,
    notBefore: Schema.String,
    notAfter: Schema.String,
  }),
  chain: Schema.Array(
    Schema.Struct({ subject: Schema.String, issuer: Schema.String, spki: Schema.String }),
  ),
})
type StoredCertificate = typeof StoredCertificate.Type

const storeError = (operation: CsrStoreError["operation"], error: unknown): CsrStoreError =>
  new CsrStoreError({
    operation,
    reason: error instanceof Error ? error.message : String(error),
  })

const stagedPath = (root: string): string => `${root}/staged.json`
const activePath = (root: string): string => `${root}/active.json`

const readCertificate = (
  path: string,
): Effect.Effect<Option.Option<InstalledCertificate>, CsrStoreError> =>
  Effect.tryPromise({
    try: async () => {
      try {
        const parsed = Schema.decodeUnknownSync(StoredCertificate)(
          JSON.parse(await Deno.readTextFile(path)),
        )
        return Option.some({
          leaf: parsed.leaf,
          chain: parsed.chain,
          installedAt: parsed.installedAt,
          privateKeyPkcs8: parsed.privateKeyPkcs8,
        })
      } catch (error) {
        if (error instanceof Deno.errors.NotFound) return Option.none()
        throw error
      }
    },
    catch: (error) => storeError("read", error),
  })

const writeAtomic = (
  path: string,
  value: InstalledCertificate,
): Effect.Effect<void, CsrStoreError> =>
  Effect.tryPromise({
    try: async () => {
      const temporary = `${path}.${crypto.randomUUID()}.tmp`
      await Deno.writeTextFile(temporary, JSON.stringify(value), { mode: 0o600 })
      await Deno.rename(temporary, path)
    },
    catch: (error) => storeError("stage", error),
  })

/** Two files on the LUKS volume: the staged pair and the serving pair. */
export const fileCertificateStoreLayer = (root: string): Layer.Layer<CertificateStore> =>
  Layer.succeed(CertificateStore)({
    staged: readCertificate(stagedPath(root)),
    active: readCertificate(activePath(root)),
    stage: (certificate) => writeAtomic(stagedPath(root), certificate),
    promote: Effect.gen(function* () {
      const staged = yield* readCertificate(stagedPath(root))
      if (Option.isNone(staged)) return Option.none()
      yield* Effect.tryPromise({
        try: () => Deno.rename(stagedPath(root), activePath(root)),
        catch: (error) => storeError("promote", error),
      })
      return staged
    }),
  })
