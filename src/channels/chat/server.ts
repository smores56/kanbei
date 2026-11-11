import { Clock, Context, Data, Effect, Layer, Option, Ref, Result, Schema } from "effect"
import { canonicalize } from "../../evidence/canonical.ts"
import { sha256Text } from "../../evidence/digest.ts"
import { decodeFrame, encodeFrame } from "./frame.ts"
import type { ChatErrorCode, ChatEvent, ChatRequest, ChatResponse } from "./schema.ts"
import {
  ChatStatus,
  ChatTurnPayload,
  DecisionListPayload,
  DecisionRulingLinkPayload,
  DecisionSummary,
  parseChatRequest,
} from "./schema.ts"
import type { ChatPeer, ChatStream } from "./stream.ts"
import type { ChatTransportError } from "./stream.ts"

export interface ChatConfig {
  readonly accessIdentity: string
  readonly passkeyDigest: string
  readonly allowedOrigin: string
  readonly maxRequestBytes: number
  readonly maxResponseBytes: number
  readonly maxTurnChars: number
  readonly maxDecisions: number
  readonly requestsPerHour: number
  readonly sessionTtlMs: number
}

export class ChatError extends Data.TaggedError("ChatError")<{
  readonly code: ChatErrorCode
  readonly detail: string
}> {}

export class ChatTurnFailure extends Data.TaggedError("ChatTurnFailure")<{
  readonly reason: string
}> {}

export interface ChatTurn {
  readonly turnId: string
  readonly text: string
}

/** The model seam; T28 serves the route shape, not a model. */
export interface ChatResponder {
  readonly reply: (turn: ChatTurn) => Effect.Effect<{ readonly text: string }, ChatTurnFailure>
}

export const ChatResponder = Context.Service<ChatResponder>("kanbei/channel/chat/Responder")

/** Read-only projections the status and decision routes render. */
export interface ChatViews {
  readonly decisions: Effect.Effect<ReadonlyArray<DecisionSummary>>
  readonly decision: (
    id: string,
  ) => Effect.Effect<Option.Option<DecisionSummary>>
  readonly status: Effect.Effect<ChatStatus>
}

export const ChatViews = Context.Service<ChatViews>("kanbei/channel/chat/Views")

export interface ChatReceipt {
  readonly route: string
  readonly peer: ChatPeer
  readonly requestDigest: string
  readonly responseDigest: string
  readonly bytesIn: number
  readonly bytesOut: number
  readonly outcome: "ok" | "denied"
  readonly at: string
}

export interface ChatServer {
  /** One request per stream; the response is always a JSON frame. */
  readonly serve: (stream: ChatStream) => Effect.Effect<void, ChatTransportError>
  readonly receipts: Effect.Effect<ReadonlyArray<ChatReceipt>>
}

export const ChatServer = Context.Service<ChatServer>("kanbei/channel/chat/Server")

interface SessionRecord {
  readonly sessionId: string
  readonly accessIdentity: string
  readonly passkeyDigest: string
  readonly origin: string
  readonly expiresAt: number
}

interface ChatState {
  readonly sessions: Readonly<Record<string, SessionRecord>>
  readonly requests: ReadonlyArray<number>
  readonly receipts: ReadonlyArray<ChatReceipt>
  readonly sessionCounter: number
}

const emptyState: ChatState = {
  sessions: {},
  requests: [],
  receipts: [],
  sessionCounter: 0,
}

const fail = (code: ChatErrorCode, detail: string): ChatError => new ChatError({ code, detail })

const now = (): Effect.Effect<number> => Clock.currentTimeMillis

const passkeyDigest = (passkey: string): Effect.Effect<string> =>
  sha256Text(passkey).pipe(Effect.orDie)

const configIssues = (config: ChatConfig): ReadonlyArray<string> => {
  const issues: Array<string> = []
  for (
    const [name, value] of Object.entries({
      maxRequestBytes: config.maxRequestBytes,
      maxResponseBytes: config.maxResponseBytes,
      maxTurnChars: config.maxTurnChars,
      maxDecisions: config.maxDecisions,
      requestsPerHour: config.requestsPerHour,
      sessionTtlMs: config.sessionTtlMs,
    })
  ) {
    if (!Number.isFinite(value) || value <= 0) issues.push(`${name} must be positive`)
  }
  if (config.accessIdentity.trim() === "") issues.push("accessIdentity is required")
  if (!config.passkeyDigest.startsWith("sha256:")) issues.push("passkeyDigest must be a digest")
  if (config.allowedOrigin !== "https://kanbei.sammohr.dev") {
    issues.push("allowedOrigin must be the owner origin")
  }
  return issues
}

const isStaticRoute = (route: string): boolean =>
  route.startsWith("/") || route.includes(".js") || route.includes(".css") ||
  route === "static" || route === "proxy"

const prune = (current: ChatState, at: number): ChatState => ({
  ...current,
  sessions: Object.fromEntries(
    Object.entries(current.sessions).filter(([, session]) => session.expiresAt > at),
  ),
  requests: current.requests.filter((stamp) => at - stamp < 60 * 60 * 1000),
})

const parsePayload = <A>(
  schema: Schema.ConstraintDecoder<A>,
  payload: Schema.Json | undefined,
): Result.Result<A, ChatError> => {
  if (payload === undefined) {
    return Result.fail(fail("chat_schema_invalid", "payload is required"))
  }
  try {
    return Result.succeed(
      Schema.decodeUnknownSync(schema, { onExcessProperty: "error" })(payload),
    )
  } catch (error) {
    return Result.fail(
      fail(
        "chat_schema_invalid",
        error instanceof Error ? error.message.slice(0, 200) : String(error),
      ),
    )
  }
}

const response = (
  route: string,
  payload: Schema.Json,
  sessionId: string,
): ChatResponse => ({
  kind: "response",
  route,
  ok: true,
  sessionId,
  payload,
})

const errorResponse = (route: string, error: ChatError): ChatResponse => ({
  kind: "response",
  route,
  ok: false,
  error: { code: error.code, detail: error.detail },
})

const turnEvents = (text: string): ReadonlyArray<ChatEvent> => [
  { kind: "event", seq: 0, route: "chat.turn", event: "start" },
  { kind: "event", seq: 1, route: "chat.turn", event: "delta", text },
  { kind: "event", seq: 2, route: "chat.turn", event: "end" },
]

export const chatServerLayer = (
  config: ChatConfig,
): Layer.Layer<ChatServer, never, ChatResponder | ChatViews> =>
  Layer.effect(
    ChatServer,
    Effect.gen(function* () {
      const issues = configIssues(config)
      if (issues.length > 0) {
        return yield* Effect.die(new Error(`invalid chat config: ${issues.join("; ")}`))
      }
      const responder = yield* ChatResponder
      const views = yield* ChatViews
      const state = yield* Ref.make(emptyState)

      const record = (receipt: ChatReceipt): Effect.Effect<void> =>
        Ref.update(state, (current) => ({
          ...current,
          receipts: [...current.receipts, receipt],
        }))

      const receipt = (
        request: ChatRequest,
        peer: ChatPeer,
        bytesIn: number,
        bytesOut: number,
        outcome: ChatReceipt["outcome"],
      ): Effect.Effect<void> =>
        Effect.gen(function* () {
          const at = new Date(yield* now()).toISOString()
          yield* record({
            route: request.route,
            peer,
            requestDigest: yield* sha256Text(
              canonicalize(Schema.decodeUnknownSync(Schema.Json)(request)),
            ).pipe(Effect.orDie),
            responseDigest: yield* sha256Text(`${outcome}:${request.route}:${bytesOut}`).pipe(
              Effect.orDie,
            ),
            bytesIn,
            bytesOut,
            outcome,
            at,
          })
        })

      const sessionIdFor = (identity: string, window: number): Effect.Effect<string> =>
        Effect.gen(function* () {
          const counter = yield* Ref.modify(state, (current) =>
            [
              current.sessionCounter + 1,
              { ...current, sessionCounter: current.sessionCounter + 1 },
            ] as const)
          const digest = yield* sha256Text(
            `${identity}|${config.allowedOrigin}|${window}|${counter}`,
          ).pipe(Effect.orDie)
          return digest
        })

      const authenticate = (
        request: ChatRequest,
      ): Effect.Effect<{ readonly sessionId: string }, ChatError> =>
        Effect.gen(function* () {
          if (request.origin !== config.allowedOrigin) {
            return yield* Effect.fail(
              fail(
                "chat_origin_denied",
                `origin ${request.origin ?? "<missing>"} is not the owner origin`,
              ),
            )
          }
          const passkey = request.passkey
          if (passkey === undefined || passkey === "") {
            return yield* Effect.fail(fail("chat_unauthenticated", "passkey is required"))
          }
          if (request.accessIdentity !== config.accessIdentity) {
            return yield* Effect.fail(
              fail("chat_unauthenticated", "Access identity is missing or foreign"),
            )
          }
          const digest = yield* passkeyDigest(passkey)
          if (digest !== config.passkeyDigest) {
            return yield* Effect.fail(fail("chat_unauthenticated", "passkey does not match"))
          }
          const at = yield* now()
          const sessionId = request.sessionId
          if (sessionId === undefined) {
            const id = yield* sessionIdFor(config.accessIdentity, at)
            const session: SessionRecord = {
              sessionId: id,
              accessIdentity: config.accessIdentity,
              passkeyDigest: digest,
              origin: config.allowedOrigin,
              expiresAt: at + config.sessionTtlMs,
            }
            yield* Ref.update(state, (current) => ({
              ...prune(current, at),
              sessions: { ...current.sessions, [id]: session },
            }))
            return { sessionId: id }
          }
          const current = yield* Ref.get(state)
          const session = current.sessions[sessionId]
          if (
            session === undefined ||
            session.expiresAt <= at ||
            session.accessIdentity !== config.accessIdentity ||
            session.origin !== request.origin ||
            session.passkeyDigest !== digest
          ) {
            return yield* Effect.fail(
              fail(
                "chat_unauthenticated",
                "session is unknown, expired, or not bound to this passkey",
              ),
            )
          }
          return { sessionId }
        })

      const rateLimit = (): Effect.Effect<void, ChatError> =>
        Effect.gen(function* () {
          const at = yield* now()
          const current = prune(yield* Ref.get(state), at)
          if (current.requests.length >= config.requestsPerHour) {
            yield* Ref.update(state, () => current)
            return yield* Effect.fail(
              fail("chat_rate_limited", `${config.requestsPerHour} requests in the window`),
            )
          }
          yield* Ref.update(state, () => ({ ...current, requests: [...current.requests, at] }))
        })

      const route = (
        request: ChatRequest,
        sessionId: string,
      ): Effect.Effect<
        { readonly events: ReadonlyArray<ChatEvent>; readonly response: ChatResponse },
        ChatError
      > =>
        Effect.gen(function* () {
          if (request.upgrade !== undefined) {
            return yield* Effect.fail(
              fail("chat_upgrade_denied", "the chat channel has no upgrade"),
            )
          }
          if (isStaticRoute(request.route)) {
            return yield* Effect.fail(
              fail("chat_static_denied", `route ${request.route} is not a JSON route`),
            )
          }
          if (request.route === "chat.turn") {
            const payload = parsePayload(ChatTurnPayload, request.payload)
            if (Result.isFailure(payload)) return yield* Effect.fail(payload.failure)
            if (payload.success.text.length > config.maxTurnChars) {
              return yield* Effect.fail(
                fail(
                  "chat_too_large",
                  `${payload.success.text.length} > ${config.maxTurnChars} characters`,
                ),
              )
            }
            const reply = yield* responder.reply(payload.success).pipe(
              Effect.mapError((failure) => fail("chat_turn_failed", failure.reason)),
            )
            return {
              events: turnEvents(reply.text),
              response: response(
                "chat.turn",
                { turnId: payload.success.turnId, text: reply.text },
                sessionId,
              ),
            }
          }
          if (request.route === "decision.list") {
            const payload = parsePayload(DecisionListPayload, request.payload ?? {})
            if (Result.isFailure(payload)) return yield* Effect.fail(payload.failure)
            const limit = payload.success.limit ?? config.maxDecisions
            if (!Number.isInteger(limit) || limit < 1 || limit > config.maxDecisions) {
              return yield* Effect.fail(
                fail("chat_schema_invalid", `limit must be 1..${config.maxDecisions}`),
              )
            }
            const decisions = yield* views.decisions
            return {
              events: [],
              response: response(
                "decision.list",
                { decisions: decisions.slice(0, limit) },
                sessionId,
              ),
            }
          }
          if (request.route === "decision.ruling_link") {
            const payload = parsePayload(DecisionRulingLinkPayload, request.payload)
            if (Result.isFailure(payload)) return yield* Effect.fail(payload.failure)
            const decision = yield* views.decision(payload.success.decisionId)
            if (Option.isNone(decision)) {
              return yield* Effect.fail(
                fail("chat_not_found", `decision ${payload.success.decisionId}`),
              )
            }
            return {
              events: [],
              response: response(
                "decision.ruling_link",
                {
                  decisionId: payload.success.decisionId,
                  link: `/consent/decisions/${encodeURIComponent(payload.success.decisionId)}`,
                },
                sessionId,
              ),
            }
          }
          if (request.route === "status") {
            const status = yield* views.status
            return {
              events: [],
              response: response(
                "status",
                {
                  generation: status.generation,
                  sources: status.sources.map((source) =>
                    source.lastSequence === undefined
                      ? { origin: source.origin, state: source.state }
                      : {
                        origin: source.origin,
                        state: source.state,
                        lastSequence: source.lastSequence,
                      }
                  ),
                },
                sessionId,
              ),
            }
          }
          return yield* Effect.fail(fail("chat_route_unknown", `${request.route} is not a route`))
        })

      const emit = (
        stream: ChatStream,
        routeName: string,
        outbound: ReadonlyArray<ChatResponse | ChatEvent>,
      ): Effect.Effect<number, ChatTransportError> => {
        const encoded = outbound.map(encodeFrame)
        const oversized = encoded.find((bytes) => bytes.length > config.maxResponseBytes)
        if (oversized !== undefined) {
          const fallback = encodeFrame(
            errorResponse(
              routeName,
              fail("chat_response_too_large", `${oversized.length} > ${config.maxResponseBytes}`),
            ),
          )
          return stream.writeFrame(fallback).pipe(Effect.as(fallback.length))
        }
        const total = encoded.reduce((sum, bytes) => sum + bytes.length, 0)
        return Effect.forEach(encoded, (bytes) => stream.writeFrame(bytes), {
          concurrency: 1,
        }).pipe(Effect.as(total))
      }

      const serve: ChatServer["serve"] = (stream) =>
        Effect.gen(function* () {
          const raw = yield* stream.readFrame
          if (Option.isNone(raw)) {
            yield* stream.close
            return
          }
          const decoded = decodeFrame(raw.value, config.maxRequestBytes)
          if (Result.isFailure(decoded)) {
            const code = decoded.failure.code === "frame_too_large"
              ? "chat_too_large"
              : "chat_schema_invalid"
            const failure = fail(code, decoded.failure.detail)
            const responseFrame = errorResponse("<unparsed>", failure)
            const responseBytes = encodeFrame(responseFrame)
            yield* stream.writeFrame(responseBytes)
            yield* receipt(
              { route: "<unparsed>" },
              stream.peer,
              raw.value.length,
              responseBytes.length,
              "denied",
            )
            yield* stream.close
            return
          }
          const parsed = Result.try({
            try: () => parseChatRequest(decoded.success),
            catch: (error) =>
              fail(
                "chat_schema_invalid",
                error instanceof Error ? error.message.slice(0, 200) : String(error),
              ),
          })
          if (Result.isFailure(parsed)) {
            const responseFrame = errorResponse("<invalid>", parsed.failure)
            const responseBytes = encodeFrame(responseFrame)
            yield* stream.writeFrame(responseBytes)
            yield* receipt(
              { route: "<invalid>" },
              stream.peer,
              raw.value.length,
              responseBytes.length,
              "denied",
            )
            yield* stream.close
            return
          }
          const request = parsed.success
          const handled = yield* Effect.gen(function* () {
            const session = yield* authenticate(request)
            yield* rateLimit()
            const routed = yield* route(request, session.sessionId)
            return routed
          }).pipe(Effect.result)
          const outbound = Result.isFailure(handled)
            ? [errorResponse(request.route, handled.failure)]
            : [...handled.success.events, handled.success.response]
          const bytesOut = yield* emit(stream, request.route, outbound)
          yield* receipt(
            request,
            stream.peer,
            raw.value.length,
            bytesOut,
            Result.isFailure(handled) ? "denied" : "ok",
          )
          yield* stream.close
        })

      return { serve, receipts: Ref.get(state).pipe(Effect.map((value) => value.receipts)) }
    }),
  )
