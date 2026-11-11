import { Schema } from "effect"

/**
 * The deployed certificate channel (`nix-config modules/lib/kanbei/vsock.py` and
 * `cert_guest.py`): a `KANBEI-AUTH 1 <token>` line, then a 4-byte big-endian
 * length-prefixed JSON frame `{v,cn,csr}` carrying a PEM CSR. The reply is
 * either `{v,cn,chain,not_after}` or `{v,error}`.
 */
export const CSR_COMMON_NAME = "kanbei.sammohr.dev"
export const CSR_VERSION = 1
export const CSR_MAX_FRAME_BYTES = 64 * 1024
export const CSR_AUTH_PREFIX = "KANBEI-AUTH 1 "

const strict = { onExcessProperty: "error" } as const

export const CsrWireRequest = Schema.Struct({
  v: Schema.Literal(CSR_VERSION),
  cn: Schema.Literal(CSR_COMMON_NAME),
  csr: Schema.String,
})
export type CsrWireRequest = typeof CsrWireRequest.Type

export const CsrWireSuccess = Schema.Struct({
  v: Schema.Literal(CSR_VERSION),
  cn: Schema.Literal(CSR_COMMON_NAME),
  chain: Schema.String,
  not_after: Schema.String,
})
export type CsrWireSuccess = typeof CsrWireSuccess.Type

export const CsrWireError = Schema.Struct({
  v: Schema.Literal(CSR_VERSION),
  error: Schema.String,
})
export type CsrWireError = typeof CsrWireError.Type

export const CsrWireResponse = Schema.Union([CsrWireSuccess, CsrWireError])
export type CsrWireResponse = typeof CsrWireResponse.Type

export const parseCsrWireResponse = (value: unknown): CsrWireResponse =>
  Schema.decodeUnknownSync(CsrWireResponse, strict)(value)

export const encodeCsrWireRequest = (csr: string): Uint8Array =>
  new TextEncoder().encode(
    JSON.stringify(
      Schema.encodeSync(CsrWireRequest)({
        v: CSR_VERSION,
        cn: CSR_COMMON_NAME,
        csr,
      }),
    ),
  )

/** Fields that must equal the fixed contract; any drift refuses before the CA. */
export const csrFieldsIssue = (fields: {
  readonly commonName: string
}): string | undefined =>
  fields.commonName === CSR_COMMON_NAME
    ? undefined
    : `commonName ${fields.commonName} != ${CSR_COMMON_NAME}`
