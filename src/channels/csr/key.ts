import { Data, Effect } from "effect"

export class CsrKeyError extends Data.TaggedError("CsrKeyError")<{
  readonly operation: "generate" | "sign" | "verify" | "import"
  readonly reason: string
}> {}

export interface SessionKey {
  readonly privateKey: CryptoKey
  readonly publicKeySpki: string
}

const encoder = new TextEncoder()

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
      const spki = await crypto.subtle.exportKey("spki", pair.publicKey)
      return {
        privateKey: pair.privateKey,
        publicKeySpki: new Uint8Array(spki).toBase64(),
      }
    },
    catch: (error) => keyError("generate", error),
  })

export const signText = (
  key: SessionKey,
  text: string,
): Effect.Effect<string, CsrKeyError> =>
  Effect.tryPromise({
    try: async () => {
      const signature = await crypto.subtle.sign(
        { name: "ECDSA", hash: "SHA-256" },
        key.privateKey,
        encoder.encode(text),
      )
      return new Uint8Array(signature).toBase64()
    },
    catch: (error) => keyError("sign", error),
  })

export const importSpki = (
  spki: string,
): Effect.Effect<CryptoKey, CsrKeyError> =>
  Effect.tryPromise({
    try: () =>
      crypto.subtle.importKey(
        "spki",
        Uint8Array.fromBase64(spki),
        ecP256,
        true,
        ["verify"],
      ),
    catch: (error) => keyError("import", error),
  })

export const verifyText = (
  spki: string,
  signature: string,
  text: string,
): Effect.Effect<boolean, CsrKeyError> =>
  importSpki(spki).pipe(
    Effect.flatMap((publicKey) =>
      Effect.tryPromise({
        try: () =>
          crypto.subtle.verify(
            { name: "ECDSA", hash: "SHA-256" },
            publicKey,
            Uint8Array.fromBase64(signature),
            encoder.encode(text),
          ),
        catch: (error) => keyError("verify", error),
      })
    ),
  )

export const isP256Spki = (spki: string): Effect.Effect<boolean, CsrKeyError> =>
  importSpki(spki).pipe(
    Effect.map(() => true),
    Effect.catch(() => Effect.succeed(false)),
  )

/** The serving key is persisted with the certificate, never in a receipt or log. */
export const exportPkcs8 = (key: SessionKey): Effect.Effect<string, CsrKeyError> =>
  Effect.tryPromise({
    try: async () => {
      const pkcs8 = await crypto.subtle.exportKey("pkcs8", key.privateKey)
      return new Uint8Array(pkcs8).toBase64()
    },
    catch: (error) => keyError("generate", error),
  })
