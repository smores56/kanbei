import { Schema } from "effect"

export const CSR_COMMON_NAME = "kanbei.sammohr.dev"
export const CSR_KEY_ALGORITHM = "ec-p256"
export const CSR_SANS: ReadonlyArray<string> = [CSR_COMMON_NAME]
export const CSR_VERSION = 1

const Base64 = Schema.String.pipe(
  Schema.check(Schema.isPattern(/^[A-Za-z0-9+/]+={0,2}$/u)),
)

/** The only CSR shape the channel can carry: one CN, one key algorithm, one SAN set. */
export const CsrRequest = Schema.Struct({
  version: Schema.Literal(CSR_VERSION),
  commonName: Schema.Literal(CSR_COMMON_NAME),
  keyAlgorithm: Schema.Literal(CSR_KEY_ALGORITHM),
  sans: Schema.Tuple([Schema.Literal(CSR_COMMON_NAME)]),
  publicKeySpki: Base64,
  signature: Base64,
})
export type CsrRequest = typeof CsrRequest.Type

export const CertificateName = Schema.Struct({
  subject: Schema.String,
  issuer: Schema.String,
  spki: Base64,
})
export type CertificateName = typeof CertificateName.Type

export const LeafCertificate = Schema.Struct({
  subject: Schema.Literal(CSR_COMMON_NAME),
  issuer: Schema.String,
  spki: Base64,
  notBefore: Schema.String,
  notAfter: Schema.String,
})
export type LeafCertificate = typeof LeafCertificate.Type

export const HostResponse = Schema.Struct({
  version: Schema.Literal(CSR_VERSION),
  leaf: LeafCertificate,
  chain: Schema.Array(CertificateName),
  proof: Base64,
})
export type HostResponse = typeof HostResponse.Type

export const parseCsrRequest = (value: unknown): CsrRequest =>
  Schema.decodeUnknownSync(CsrRequest, { onExcessProperty: "error" })(value)

export const parseHostResponse = (value: unknown): HostResponse =>
  Schema.decodeUnknownSync(HostResponse, { onExcessProperty: "error" })(value)

/** Fields that must equal the fixed contract; any drift refuses before the CA. */
export const csrFieldsIssue = (fields: {
  readonly commonName: string
  readonly keyAlgorithm: string
  readonly sans: ReadonlyArray<string>
}): string | undefined => {
  if (fields.commonName !== CSR_COMMON_NAME) {
    return `commonName ${fields.commonName} != ${CSR_COMMON_NAME}`
  }
  if (fields.keyAlgorithm !== CSR_KEY_ALGORITHM) {
    return `keyAlgorithm ${fields.keyAlgorithm} != ${CSR_KEY_ALGORITHM}`
  }
  if (fields.sans.length !== CSR_SANS.length) {
    return `sans ${fields.sans.join(",")} != ${CSR_SANS.join(",")}`
  }
  if (fields.sans.some((san, index) => san !== CSR_SANS[index])) {
    return `sans ${fields.sans.join(",")} != ${CSR_SANS.join(",")}`
  }
  return undefined
}
