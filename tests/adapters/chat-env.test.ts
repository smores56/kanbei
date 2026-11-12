import { assertEquals } from "@std/assert"
import { Effect } from "effect"
import { chatConfigFromEnv } from "../../src/adapters/chat-env.ts"
import { verifyPasskey } from "../../src/channels/chat/passkey.ts"

const values = {
  KANBEI_ACCESS_IDENTITY: "owner@example.invalid",
  KANBEI_CHAT_PASSKEY: "passkey-123",
  KANBEI_CHAT_ORIGIN: "https://kanbei.sammohr.dev",
  KANBEI_CHAT_MAX_REQUEST_BYTES: "8192",
  KANBEI_CHAT_MAX_RESPONSE_BYTES: "16384",
  KANBEI_CHAT_MAX_TURN_CHARS: "200",
  KANBEI_CHAT_MAX_DECISIONS: "5",
  KANBEI_CHAT_REQUESTS_PER_HOUR: "30",
  KANBEI_CHAT_SESSION_TTL_MS: "3600000",
}

const env = (overrides: Record<string, string | undefined> = {}) => ({
  get: (name: string): string | undefined => {
    const value = { ...values, ...overrides }[name]
    return value
  },
})

Deno.test("the guest config comes from the environment and stores a PBKDF2 verifier", async () => {
  const config = await Effect.runPromise(chatConfigFromEnv(env()))
  assertEquals(config.accessIdentity, "owner@example.invalid")
  assertEquals(config.passkeyVerifier.startsWith("pbkdf2-sha256$"), true)
  assertEquals(config.passkeyVerifier.includes("passkey-123"), false)
  assertEquals(
    await Effect.runPromise(verifyPasskey(config.passkeyVerifier, "passkey-123")),
    true,
  )
  assertEquals(
    await Effect.runPromise(verifyPasskey(config.passkeyVerifier, "passkey-124")),
    false,
  )
  assertEquals(config.requestsPerHour, 30)
})

Deno.test("a missing credential refuses the config rather than defaulting", async () => {
  const failure = await Effect.runPromise(
    Effect.flip(chatConfigFromEnv(env({ KANBEI_CHAT_PASSKEY: undefined }))),
  )
  assertEquals(failure.variable, "KANBEI_CHAT_PASSKEY")
})

Deno.test("a non-numeric ceiling refuses the config rather than defaulting", async () => {
  const failure = await Effect.runPromise(
    Effect.flip(chatConfigFromEnv(env({ KANBEI_CHAT_MAX_RESPONSE_BYTES: "lots" }))),
  )
  assertEquals(failure.variable, "KANBEI_CHAT_MAX_RESPONSE_BYTES")
})
