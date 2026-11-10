import { Effect } from "effect"
import { DigestError } from "./errors.ts"

const encoder = new TextEncoder()
const digestPattern = /^sha256:[0-9a-f]{64}$/u

const toHex = (buffer: ArrayBuffer): string =>
  Array.from(new Uint8Array(buffer), (byte) => byte.toString(16).padStart(2, "0")).join("")

export const sha256 = (bytes: Uint8Array): Effect.Effect<string, DigestError> =>
  Effect.tryPromise({
    try: async () => {
      const copy = new Uint8Array(bytes.length)
      copy.set(bytes)
      return `sha256:${toHex(await crypto.subtle.digest("SHA-256", copy.buffer))}`
    },
    catch: (error) => new DigestError({ reason: String(error) }),
  })

export const sha256Text = (text: string): Effect.Effect<string, DigestError> =>
  sha256(encoder.encode(text))

export const isContentDigest = (value: string): boolean => digestPattern.test(value)
