import { assert, assertEquals } from "@std/assert"
import { Effect, Result } from "effect"
import {
  defaultPasskeySalt,
  derivePasskeyVerifier,
  isPasskeyVerifier,
  parseVerifier,
  PASSKEY_ITERATIONS,
  PasskeyError,
  timingSafeEqualBytes,
  verifyPasskey,
} from "../../../src/channels/chat/passkey.ts"

const derive = (passkey: string, identity = "owner@example.invalid") =>
  Effect.runPromise(
    derivePasskeyVerifier(passkey, defaultPasskeySalt(identity), 100_000),
  )

Deno.test("the verifier is a slow PBKDF2 record with no plaintext inside", async () => {
  const verifier = await derive("correct horse battery staple")
  assertEquals(isPasskeyVerifier(verifier), true)
  assertEquals(verifier.startsWith("pbkdf2-sha256$100000$"), true)
  assertEquals(verifier.includes("correct horse battery staple"), false)
  const parsed = parseVerifier(verifier)
  assert(Result.isSuccess(parsed))
  if (Result.isSuccess(parsed)) {
    assertEquals(parsed.success.iterations, 100_000)
    assertEquals(parsed.success.hash.length, 32)
  }
  assertEquals(PASSKEY_ITERATIONS >= 100_000, true)
})

Deno.test("a verifier accepts only its passkey", async () => {
  const verifier = await derive("correct horse battery staple")
  assertEquals(
    await Effect.runPromise(verifyPasskey(verifier, "correct horse battery staple")),
    true,
  )
  assertEquals(
    await Effect.runPromise(verifyPasskey(verifier, "correct horse battery stapl3")),
    false,
  )
  assertEquals(await Effect.runPromise(verifyPasskey("sha256:abc", "anything")), false)
})

Deno.test("distinct salts produce distinct hashes for the same passkey", async () => {
  const left = await Effect.runPromise(
    derivePasskeyVerifier("same passkey", new Uint8Array(16).fill(1), 100_000),
  )
  const right = await Effect.runPromise(
    derivePasskeyVerifier("same passkey", new Uint8Array(16).fill(2), 100_000),
  )
  assertEquals(left === right, false)
})

Deno.test("a weak iteration count is refused on parse", () => {
  const weak = `pbkdf2-sha256$1000$AAAAAAAAAAAAAAAAAAAAAA==$AAAA`
  assertEquals(isPasskeyVerifier(weak), false)
})

Deno.test("constant-time comparison detects a trailing difference", () => {
  assertEquals(timingSafeEqualBytes(new Uint8Array([1, 2, 3]), new Uint8Array([1, 2, 3])), true)
  assertEquals(timingSafeEqualBytes(new Uint8Array([1, 2, 3]), new Uint8Array([1, 2, 4])), false)
  assertEquals(timingSafeEqualBytes(new Uint8Array([1, 2, 3]), new Uint8Array([1, 2])), false)
})

Deno.test("a derivation error stays in the error channel", async () => {
  const error = await Effect.runPromise(
    Effect.flip(
      derivePasskeyVerifier("passkey", new Uint8Array(16), 0).pipe(
        Effect.catchTag("PasskeyError", (failure) => Effect.fail(failure)),
      ),
    ),
  )
  assertEquals(error instanceof PasskeyError, true)
})
