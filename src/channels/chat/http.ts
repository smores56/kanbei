import { Clock, Context, Data, Effect, Layer, Ref, Result } from "effect"

/**
 * The deployed chat contract (`nix-config modules/lib/kanbei/ui/server.py`):
 * JSON only behind the relay token; `/api/chat` additionally needs a bearer
 * session issued after passkey authentication. These are the host's constants.
 */
export const CHAT_MAX_BODY_BYTES = 64 * 1024
export const CHAT_MAX_MESSAGE_CHARS = 8000
export const CHAT_SESSION_TTL_MS = 12 * 60 * 60 * 1000
export const CHAT_SESSION_MAX = 32
export const CHAT_STUB_REPLY = "Kanbei stub: no chat lane is attached to this guest yet."

export interface ChatHttpRequest {
  readonly method: "GET" | "POST" | "HEAD"
  readonly path: string
  readonly headers: Readonly<Record<string, string>>
  readonly body: Uint8Array
  /** True once the listener consumed the relay `KANBEI-AUTH` line. */
  readonly relayAuthenticated: boolean
}

export interface ChatHttpResponse {
  readonly status: number
  readonly headers: Readonly<Record<string, string>>
  readonly body: Uint8Array
}

export class ChatUpstreamError extends Data.TaggedError("ChatUpstreamError")<{
  readonly reason: string
}> {}

/** The model seam: the local llama.cpp lane, or a stub when none is attached. */
export interface ChatUpstream {
  readonly reply: (message: string) => Effect.Effect<string, ChatUpstreamError>
}

export const ChatUpstream = Context.Service<ChatUpstream>("kanbei/channel/chat/Upstream")

export interface ChatHttpConfig {
  readonly upstream: boolean
  readonly stubReply: string
  readonly maxBodyBytes: number
  readonly maxMessageChars: number
}

export const defaultChatHttpConfig: ChatHttpConfig = {
  upstream: false,
  stubReply: CHAT_STUB_REPLY,
  maxBodyBytes: CHAT_MAX_BODY_BYTES,
  maxMessageChars: CHAT_MAX_MESSAGE_CHARS,
}

interface Session {
  readonly token: string
  readonly expiresAt: number
}

export interface SessionStore {
  readonly issue: Effect.Effect<string>
  readonly validate: (token: string) => Effect.Effect<boolean>
}

export const SessionStore = Context.Service<SessionStore>("kanbei/channel/chat/SessionStore")

/** Bearer sessions expire and are capped, exactly like the host's store. */
export const sessionStoreLayer = (ttlMs = CHAT_SESSION_TTL_MS): Layer.Layer<SessionStore> =>
  Layer.effect(
    SessionStore,
    Effect.gen(function* () {
      const sessions = yield* Ref.make<ReadonlyArray<Session>>([])
      const counter = yield* Ref.make(0)
      return {
        issue: Effect.gen(function* () {
          const at = yield* Clock.currentTimeMillis
          const index = yield* Ref.updateAndGet(counter, (value) => value + 1)
          const token = `kb-${at.toString(36)}-${index.toString(36)}-${
            crypto.randomUUID().replaceAll("-", "")
          }`
          yield* Ref.update(sessions, (current) => {
            const live = current.filter((session) => session.expiresAt > at)
            const capped = live.slice(-(CHAT_SESSION_MAX - 1))
            return [...capped, { token, expiresAt: at + ttlMs }]
          })
          return token
        }),
        validate: (token) =>
          Effect.gen(function* () {
            const at = yield* Clock.currentTimeMillis
            const current = yield* Ref.get(sessions)
            const found = current.find((session) => session.token === token)
            return found !== undefined && found.expiresAt > at
          }),
      }
    }),
  )

const encoder = new TextEncoder()

const json = (
  status: number,
  payload: unknown,
  extra: Readonly<Record<string, string>> = {},
): ChatHttpResponse => ({
  status,
  headers: {
    "content-type": "application/json; charset=utf-8",
    "cache-control": "no-store",
    "x-content-type-options": "nosniff",
    ...extra,
  },
  body: encoder.encode(JSON.stringify(payload)),
})

const error = (status: number, message: string): ChatHttpResponse =>
  json(status, { error: message })

const decodeBody = (body: Uint8Array): unknown => JSON.parse(new TextDecoder().decode(body))

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null

const bearer = (headers: Readonly<Record<string, string>>): string | undefined => {
  const header = headers["authorization"] ?? ""
  const [scheme, ...rest] = header.split(" ")
  return scheme?.toLowerCase() === "bearer" ? rest.join(" ").trim() : undefined
}

const contentType = (headers: Readonly<Record<string, string>>): string =>
  (headers["content-type"] ?? "").split(";")[0]?.trim().toLowerCase() ?? ""

export interface ChatHttp {
  readonly handle: (request: ChatHttpRequest) => Effect.Effect<ChatHttpResponse>
}

export const ChatHttp = Context.Service<ChatHttp>("kanbei/channel/chat/Http")

/** The whole route table as a pure function of its inputs and injected stores. */
export const handleChatHttp = (
  config: ChatHttpConfig,
  sessions: SessionStore,
  upstream: ChatUpstream,
) =>
(request: ChatHttpRequest): Effect.Effect<ChatHttpResponse> =>
  Effect.gen(function* () {
    const path = request.path.split("?")[0] ?? request.path
    if (path.startsWith("/api/") && !request.relayAuthenticated) {
      return error(401, "this connection did not present the relay token")
    }
    if (request.method === "GET" || request.method === "HEAD") {
      return path === "/api/health" ? json(200, { ok: true }) : error(404, "not found")
    }
    if (path !== "/api/chat") return error(404, "not found")
    if (contentType(request.headers) !== "application/json") {
      return error(415, "content-type must be application/json")
    }
    const token = bearer(request.headers)
    if (token === undefined || !(yield* sessions.validate(token))) {
      return error(401, "a passkey session is required")
    }
    if (request.body.length === 0 || request.body.length > config.maxBodyBytes) {
      return error(413, "body is missing or too large")
    }
    const parsed = yield* Effect.result(Effect.try(() => decodeBody(request.body)))
    if (Result.isFailure(parsed) || !isRecord(parsed.success)) {
      return error(400, "body must be a UTF-8 JSON object")
    }
    const message = parsed.success["message"]
    if (typeof message !== "string" || message.trim() === "") {
      return error(400, "message must be a non-empty string")
    }
    if (message.length > config.maxMessageChars) {
      return error(413, "message is too long")
    }
    if (!config.upstream) return json(200, { reply: config.stubReply, lane: "stub" })
    const reply = yield* upstream.reply(message).pipe(Effect.result)
    if (Result.isFailure(reply)) return error(502, "the chat lane did not answer")
    return json(200, { reply: reply.success, lane: "local" })
  })

export const chatHttpLayer = (
  config: ChatHttpConfig = defaultChatHttpConfig,
): Layer.Layer<ChatHttp, never, SessionStore | ChatUpstream> =>
  Layer.effect(
    ChatHttp,
    Effect.gen(function* () {
      const sessions = yield* SessionStore
      const upstream = yield* ChatUpstream
      return { handle: handleChatHttp(config, sessions, upstream) }
    }),
  )

/** Effect-free factories for the listener adapter, which owns the process clock. */
export const makeSessionStore = (
  clock: () => number,
  ttlMs = CHAT_SESSION_TTL_MS,
): SessionStore => {
  let sessions: ReadonlyArray<Session> = []
  let counter = 0
  return {
    issue: Effect.sync(() => {
      const at = clock()
      counter += 1
      const token = `kb-${at.toString(36)}-${counter.toString(36)}-${
        crypto.randomUUID().replaceAll("-", "")
      }`
      const live = sessions.filter((session) => session.expiresAt > at)
      sessions = [...live.slice(-(CHAT_SESSION_MAX - 1)), { token, expiresAt: at + ttlMs }]
      return token
    }),
    validate: (token) =>
      Effect.sync(() => {
        const at = clock()
        const found = sessions.find((session) => session.token === token)
        return found !== undefined && found.expiresAt > at
      }),
  }
}

export const makeChatHttp = (
  config: ChatHttpConfig,
  sessions: SessionStore,
  reply: (message: string) => Promise<string>,
): ChatHttp => {
  const upstream: ChatUpstream = {
    reply: (message) =>
      Effect.tryPromise({
        try: () => reply(message),
        catch: (failure) =>
          new ChatUpstreamError({
            reason: failure instanceof Error ? failure.message : String(failure),
          }),
      }),
  }
  return { handle: handleChatHttp(config, sessions, upstream) }
}
