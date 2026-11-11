import { Data, Effect } from "effect"
import type { ChatConfig } from "../channels/chat/server.ts"
import { defaultPasskeySalt, derivePasskeyVerifier } from "../channels/chat/passkey.ts"

export interface ChatEnvironment {
  readonly get: (name: string) => string | undefined
}

export class ChatConfigError extends Data.TaggedError("ChatConfigError")<{
  readonly variable: string
  readonly reason: string
}> {}

const required = (
  env: ChatEnvironment,
  name: string,
): Effect.Effect<string, ChatConfigError> => {
  const value = env.get(name)
  return value === undefined || value.trim() === ""
    ? Effect.fail(new ChatConfigError({ variable: name, reason: "is required" }))
    : Effect.succeed(value)
}

const positiveInteger = (
  env: ChatEnvironment,
  name: string,
): Effect.Effect<number, ChatConfigError> =>
  required(env, name).pipe(
    Effect.flatMap((value) => {
      const parsed = Number(value)
      return Number.isInteger(parsed) && parsed > 0
        ? Effect.succeed(parsed)
        : Effect.fail(new ChatConfigError({ variable: name, reason: "must be a positive integer" }))
    }),
  )

/**
 * Guest chat credentials come only from the operator environment; the passkey
 * is stored in the config as a digest so the plaintext never lives in the server.
 */
export const chatConfigFromEnv = (
  env: ChatEnvironment,
): Effect.Effect<ChatConfig, ChatConfigError> =>
  Effect.gen(function* () {
    const accessIdentity = yield* required(env, "KANBEI_ACCESS_IDENTITY")
    const passkey = yield* required(env, "KANBEI_CHAT_PASSKEY")
    const passkeyVerifier = yield* derivePasskeyVerifier(
      passkey,
      defaultPasskeySalt(accessIdentity),
    ).pipe(
      Effect.mapError((error) =>
        new ChatConfigError({ variable: "KANBEI_CHAT_PASSKEY", reason: error.reason })
      ),
    )
    const allowedOrigin = yield* required(env, "KANBEI_CHAT_ORIGIN")
    const maxRequestBytes = yield* positiveInteger(env, "KANBEI_CHAT_MAX_REQUEST_BYTES")
    const maxResponseBytes = yield* positiveInteger(env, "KANBEI_CHAT_MAX_RESPONSE_BYTES")
    const maxTurnChars = yield* positiveInteger(env, "KANBEI_CHAT_MAX_TURN_CHARS")
    const maxDecisions = yield* positiveInteger(env, "KANBEI_CHAT_MAX_DECISIONS")
    const requestsPerHour = yield* positiveInteger(env, "KANBEI_CHAT_REQUESTS_PER_HOUR")
    const sessionTtlMs = yield* positiveInteger(env, "KANBEI_CHAT_SESSION_TTL_MS")
    return {
      accessIdentity,
      passkeyVerifier,
      allowedOrigin,
      maxRequestBytes,
      maxResponseBytes,
      maxTurnChars,
      maxDecisions,
      requestsPerHour,
      sessionTtlMs,
    }
  })

export const denoChatEnvironment: ChatEnvironment = {
  get: (name) => Deno.env.get(name),
}
