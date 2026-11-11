import { Context, Data, Effect } from "effect"
import type { Option } from "effect"
import type { CertificateName, LeafCertificate } from "./schema.ts"

export interface InstalledCertificate {
  readonly leaf: LeafCertificate
  readonly chain: ReadonlyArray<CertificateName>
  readonly privateKeyPkcs8: string
  readonly installedAt: string
}

export class CsrStoreError extends Data.TaggedError("CsrStoreError")<{
  readonly operation: "stage" | "promote" | "read"
  readonly reason: string
}> {}

/**
 * Certificate install is two-phase so a reload failure keeps the old
 * certificate serving: `stage` writes the new pair, `promote` flips it.
 */
export interface CertificateStore {
  readonly staged: Effect.Effect<Option.Option<InstalledCertificate>, CsrStoreError>
  readonly active: Effect.Effect<Option.Option<InstalledCertificate>, CsrStoreError>
  readonly stage: (certificate: InstalledCertificate) => Effect.Effect<void, CsrStoreError>
  readonly promote: Effect.Effect<Option.Option<InstalledCertificate>, CsrStoreError>
}

export const CertificateStore = Context.Service<CertificateStore>(
  "kanbei/channel/csr/CertificateStore",
)
