import { Data, Effect } from "effect"
import { rawEcdsaToDer } from "./pkcs10.ts"

export class CsrKeyError extends Data.TaggedError("CsrKeyError")<{
  readonly operation: "generate" | "sign" | "export"
  readonly reason: string
}> {}

export interface SessionKey {
  readonly privateKey: CryptoKey
  /** SubjectPublicKeyInfo DER, the exact bytes a CSR carries. */
  readonly publicKeySpkiDer: Uint8Array
  readonly publicKeySpki: string
}

const keyError = (operation: CsrKeyError["operation"], error: unknown): CsrKeyError =>
  new CsrKeyError({
    operation,
    reason: error instanceof Error ? error.message : String(error),
  })

const ecP256: EcKeyGenParams = { name: "ECDSA", namedCurve: "P-256" }

/** A fresh P-256 key is generated inside the renewal session and never leaves it. */
export const generateSessionKey = (): Effect.Effect<SessionKey, CsrKeyError> =>
  Effect.tryPromise({
    try: async () => {
      const pair = await crypto.subtle.generateKey(ecP256, true, ["sign", "verify"])
      const spki = new Uint8Array(await crypto.subtle.exportKey("spki", pair.publicKey))
      return {
        privateKey: pair.privateKey,
        publicKeySpkiDer: spki,
        publicKeySpki: spki.toBase64(),
      }
    },
    catch: (error) => keyError("generate", error),
  })

/** PKCS#10 wants a DER ECDSA-Sig-Value; WebCrypto returns the raw r||s pair. */
export const signDerBytes = async (
  key: SessionKey,
  info: Uint8Array,
): Promise<Uint8Array> => {
  const data = new ArrayBuffer(info.length)
  new Uint8Array(data).set(info)
  const signature = await crypto.subtle.sign(
    { name: "ECDSA", hash: "SHA-256" },
    key.privateKey,
    data,
  )
  return rawEcdsaToDer(new Uint8Array(signature))
}

export const signDer = (
  key: SessionKey,
  info: Uint8Array,
): Effect.Effect<Uint8Array, CsrKeyError> =>
  Effect.tryPromise({
    try: () => signDerBytes(key, info),
    catch: (error) => keyError("sign", error),
  })

/** The serving key is persisted with the certificate, never in a receipt or log. */
export const exportPkcs8 = (key: SessionKey): Effect.Effect<string, CsrKeyError> =>
  Effect.tryPromise({
    try: async () => {
      const pkcs8 = await crypto.subtle.exportKey("pkcs8", key.privateKey)
      return new Uint8Array(pkcs8).toBase64()
    },
    catch: (error) => keyError("export", error),
  })
