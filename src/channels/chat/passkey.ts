import { Data, Effect, Result } from "effect"

/**
 * The passkey verifier. A plain SHA-256 of the passkey lets an offline guesser
 * test billions of candidates per second; PBKDF2 raises the cost of a captured
 * verifier to a rate the owner's passphrase budget can survive.
 */
export const PASSKEY_ALGORITHM = "pbkdf2-sha256"
export const PASSKEY_ITERATIONS = 210_000
export const PASSKEY_SALT_BYTES = 16
export const PASSKEY_HASH_BYTES = 32

export class PasskeyError extends Data.TaggedError("PasskeyError")<{
  readonly reason: string
}> {}

export interface PasskeyVerifier {
  readonly iterations: number
  readonly salt: Uint8Array
  readonly hash: Uint8Array
}

const encoder = new TextEncoder()

/** Byte compare that does not short-circuit on the first differing byte. */
export const timingSafeEqualBytes = (left: Uint8Array, right: Uint8Array): boolean => {
  const length = Math.max(left.length, right.length)
  let difference = left.length ^ right.length
  for (let index = 0; index < length; index += 1) {
    difference |= (left[index] ?? 0) ^ (right[index] ?? 0)
  }
  return difference === 0
}

const derive = (
  passkey: string,
  salt: Uint8Array,
  iterations: number,
): Effect.Effect<Uint8Array, PasskeyError> =>
  Effect.tryPromise({
    try: async () => {
      const key = await crypto.subtle.importKey("raw", encoder.encode(passkey), "PBKDF2", false, [
        "deriveBits",
      ])
      const bits = await crypto.subtle.deriveBits(
        { name: "PBKDF2", hash: "SHA-256", salt: salt as unknown as BufferSource, iterations },
        key,
        PASSKEY_HASH_BYTES * 8,
      )
      return new Uint8Array(bits)
    },
    catch: (error) =>
      new PasskeyError({ reason: error instanceof Error ? error.message : String(error) }),
  })

/** `pbkdf2-sha256$<iterations>$<salt-base64>$<hash-base64>`; no plaintext inside. */
export const encodeVerifier = (verifier: PasskeyVerifier): string =>
  `${PASSKEY_ALGORITHM}$${verifier.iterations}$${verifier.salt.toBase64()}$${verifier.hash.toBase64()}`

export const parseVerifier = (text: string): Result.Result<PasskeyVerifier, string> => {
  const [algorithm, iterations, salt, hash] = text.split("$")
  if (algorithm !== PASSKEY_ALGORITHM) return Result.fail(`algorithm ${algorithm} is not supported`)
  if (iterations === undefined || salt === undefined || hash === undefined) {
    return Result.fail("verifier is not pbkdf2-sha256$iterations$salt$hash")
  }
  const rounds = Number(iterations)
  if (!Number.isInteger(rounds) || rounds < 100_000) {
    return Result.fail(`iterations ${iterations} below the 100000 floor`)
  }
  try {
    const saltBytes = Uint8Array.fromBase64(salt)
    const hashBytes = Uint8Array.fromBase64(hash)
    if (saltBytes.length < PASSKEY_SALT_BYTES || hashBytes.length !== PASSKEY_HASH_BYTES) {
      return Result.fail("verifier salt or hash has the wrong length")
    }
    return Result.succeed({ iterations: rounds, salt: saltBytes, hash: hashBytes })
  } catch {
    return Result.fail("verifier salt or hash is not base64")
  }
}

export const isPasskeyVerifier = (text: string): boolean => Result.isSuccess(parseVerifier(text))

/** Derives a verifier; the caller supplies the salt so a deployment can pin it. */
export const derivePasskeyVerifier = (
  passkey: string,
  salt: Uint8Array,
  iterations = PASSKEY_ITERATIONS,
): Effect.Effect<string, PasskeyError> =>
  derive(passkey, salt, iterations).pipe(
    Effect.map((hash) => encodeVerifier({ iterations, salt, hash })),
  )

/** The deployed salt is deterministic so a restart re-derives the same verifier. */
export const defaultPasskeySalt = (identity: string): Uint8Array =>
  encoder.encode(`kanbei.chat.passkey.salt.v1|${identity}`).slice(0, PASSKEY_SALT_BYTES)

export const verifyPasskey = (
  verifierText: string,
  passkey: string,
): Effect.Effect<boolean, PasskeyError> => {
  const parsed = parseVerifier(verifierText)
  if (Result.isFailure(parsed)) return Effect.succeed(false)
  return derive(passkey, parsed.success.salt, parsed.success.iterations).pipe(
    Effect.map((hash) => timingSafeEqualBytes(hash, parsed.success.hash)),
  )
}
