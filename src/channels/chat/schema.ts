import { Schema } from "effect"

/** The fixed route list: no static file, proxy, or upgrade route exists. */
export const ChatRoute = Schema.Literals([
  "chat.turn",
  "decision.list",
  "decision.ruling_link",
  "status",
])
export type ChatRoute = typeof ChatRoute.Type

export const ChatErrorCode = Schema.Literals([
  "chat_unauthenticated",
  "chat_origin_denied",
  "chat_route_unknown",
  "chat_static_denied",
  "chat_upgrade_denied",
  "chat_schema_invalid",
  "chat_too_large",
  "chat_response_too_large",
  "chat_rate_limited",
  "chat_not_found",
  "chat_turn_failed",
])
export type ChatErrorCode = typeof ChatErrorCode.Type

/** Inbound frame. `upgrade` exists only so the server can refuse it explicitly. */
export const ChatRequest = Schema.Struct({
  route: Schema.String,
  accessIdentity: Schema.optional(Schema.String),
  sessionId: Schema.optional(Schema.String),
  passkey: Schema.optional(Schema.String),
  origin: Schema.optional(Schema.String),
  payload: Schema.optional(Schema.Json),
  upgrade: Schema.optional(Schema.String),
})
export type ChatRequest = typeof ChatRequest.Type

export const ChatTurnPayload = Schema.Struct({
  turnId: Schema.String,
  text: Schema.String,
})
export type ChatTurnPayload = typeof ChatTurnPayload.Type

export const DecisionListPayload = Schema.Struct({
  limit: Schema.optional(Schema.Number),
})
export type DecisionListPayload = typeof DecisionListPayload.Type

export const DecisionRulingLinkPayload = Schema.Struct({
  decisionId: Schema.String,
})
export type DecisionRulingLinkPayload = typeof DecisionRulingLinkPayload.Type

export const DecisionSummary = Schema.Struct({
  id: Schema.String,
  title: Schema.String,
  severity: Schema.Literals(["none", "informational", "decision-required"]),
  basis: Schema.String,
})
export type DecisionSummary = typeof DecisionSummary.Type

export const ChatStatus = Schema.Struct({
  generation: Schema.String,
  sources: Schema.Array(
    Schema.Struct({
      origin: Schema.String,
      state: Schema.Literals(["fresh", "empty", "stale", "unknown"]),
      lastSequence: Schema.optional(Schema.Number),
    }),
  ),
})
export type ChatStatus = typeof ChatStatus.Type

export const ChatEvent = Schema.Struct({
  kind: Schema.Literal("event"),
  seq: Schema.Number,
  route: Schema.Literal("chat.turn"),
  event: Schema.Literals(["start", "delta", "end"]),
  text: Schema.optional(Schema.String),
})
export type ChatEvent = typeof ChatEvent.Type

export const ChatResponse = Schema.Struct({
  kind: Schema.Literal("response"),
  route: Schema.String,
  ok: Schema.Boolean,
  sessionId: Schema.optional(Schema.String),
  payload: Schema.optional(Schema.Json),
  error: Schema.optional(
    Schema.Struct({
      code: ChatErrorCode,
      detail: Schema.String,
    }),
  ),
})
export type ChatResponse = typeof ChatResponse.Type

export const ChatOutbound = Schema.Union([ChatEvent, ChatResponse])
export type ChatOutbound = typeof ChatOutbound.Type

const strict = { onExcessProperty: "error" } as const

export const parseChatRequest = (value: unknown): ChatRequest =>
  Schema.decodeUnknownSync(ChatRequest, strict)(value)

export const parseChatOutbound = (value: unknown): ChatOutbound =>
  Schema.decodeUnknownSync(ChatOutbound, strict)(value)
