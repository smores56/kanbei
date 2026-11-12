import { Context, Data, Effect, Layer } from "effect"
import { CHAT_MAX_BODY_BYTES, CHAT_MAX_MESSAGE_CHARS } from "../channels/chat/http.ts"

/**
 * Client for the deployed chat authority (`POST /api/chat` behind the relay).
 * It exists so a Deno caller can speak the host contract instead of the
 * experimental frame API; the HTTP listener surface itself is mirrored by
 * `channels/chat/http.ts`.
 */
export class ChatHttpClientError extends Data.TaggedError("ChatHttpClientError")<{
  readonly status: number
  readonly reason: string
}> {}

export interface ChatHttpClientOptions {
  readonly baseUrl: string
  /** The passkey session token; `POST /api/chat` refuses without one. */
  readonly session: () => Effect.Effect<string | undefined>
  readonly maxMessageChars?: number
}

export interface ChatHttpClient {
  readonly health: Effect.Effect<{ readonly ok: boolean }, ChatHttpClientError>
  readonly chat: (
    message: string,
  ) => Effect.Effect<{ readonly reply: string; readonly lane: string }, ChatHttpClientError>
}

export const ChatHttpClient = Context.Service<ChatHttpClient>("kanbei/channel/chat/HttpClient")

const failure = (status: number, reason: string): ChatHttpClientError =>
  new ChatHttpClientError({ status, reason })

const decoder = new TextDecoder()

interface RawResponse {
  readonly status: number
  readonly body: Uint8Array
}

const request = async (
  options: ChatHttpClientOptions,
  path: string,
  init: RequestInit,
): Promise<RawResponse> => {
  const response = await fetch(`${options.baseUrl.replace(/\/$/u, "")}${path}`, init)
  return { status: response.status, body: new Uint8Array(await response.arrayBuffer()) }
}

const parse = (body: Uint8Array): unknown => JSON.parse(decoder.decode(body))

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null

const errorOf = (response: RawResponse): ChatHttpClientError => {
  try {
    const decoded = parse(response.body)
    if (isRecord(decoded) && typeof decoded["error"] === "string") {
      return failure(response.status, decoded["error"])
    }
  } catch {
    // A non-JSON body is still a failure; the status is the reason.
  }
  return failure(response.status, `HTTP ${response.status}`)
}

export const httpChatClientLayer = (
  options: ChatHttpClientOptions,
): Layer.Layer<ChatHttpClient> =>
  Layer.succeed(ChatHttpClient)({
    health: Effect.tryPromise({
      try: () => request(options, "/api/health", { method: "GET" }),
      catch: (error) => failure(0, error instanceof Error ? error.message : String(error)),
    }).pipe(
      Effect.flatMap((response) =>
        response.status === 200 ? Effect.succeed({ ok: true }) : Effect.fail(errorOf(response))
      ),
    ),
    chat: (message) =>
      Effect.gen(function* () {
        if (message.trim() === "") {
          return yield* Effect.fail(failure(400, "message must be a non-empty string"))
        }
        if (message.length > (options.maxMessageChars ?? CHAT_MAX_MESSAGE_CHARS)) {
          return yield* Effect.fail(failure(413, "message is too long"))
        }
        const session = yield* options.session()
        if (session === undefined) {
          return yield* Effect.fail(failure(401, "a passkey session is required"))
        }
        const encoded = new TextEncoder().encode(JSON.stringify({ message }))
        if (encoded.length > CHAT_MAX_BODY_BYTES) {
          return yield* Effect.fail(failure(413, "body is missing or too large"))
        }
        const response = yield* Effect.tryPromise({
          try: () =>
            request(options, "/api/chat", {
              method: "POST",
              headers: {
                "content-type": "application/json",
                authorization: `Bearer ${session}`,
              },
              body: encoded,
            }),
          catch: (error) => failure(0, error instanceof Error ? error.message : String(error)),
        })
        if (response.status !== 200) return yield* Effect.fail(errorOf(response))
        const decoded = yield* Effect.try({
          try: () => parse(response.body),
          catch: () => failure(502, "reply is not JSON"),
        })
        if (!isRecord(decoded) || typeof decoded["reply"] !== "string") {
          return yield* Effect.fail(failure(502, "reply has no text"))
        }
        return {
          reply: decoded["reply"],
          lane: typeof decoded["lane"] === "string" ? decoded["lane"] : "local",
        }
      }),
  })
