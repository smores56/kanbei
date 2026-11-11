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

/**
 * Domain-separated digest: `label` then the parts joined by a NUL byte, per the
 * binary preimages in `07 §4.2.3`. Parts are UTF-8 strings or raw bytes.
 */
export const sha256Labeled = (
  label: string,
  parts: ReadonlyArray<string | Uint8Array>,
): Effect.Effect<string, DigestError> =>
  Effect.suspend(() => {
    const nul = new Uint8Array(1)
    const pieces = parts.flatMap((part, index) => {
      const bytes = typeof part === "string" ? encoder.encode(part) : part
      return index === parts.length - 1 ? [bytes] : [bytes, nul]
    })
    const all = [encoder.encode(label), nul, ...pieces]
    const total = all.reduce((sum, piece) => sum + piece.length, 0)
    const joined = new Uint8Array(new ArrayBuffer(total))
    all.reduce((offset, piece) => {
      joined.set(piece, offset)
      return offset + piece.length
    }, 0)
    return sha256(joined)
  })

export const isContentDigest = (value: string): boolean => digestPattern.test(value)
