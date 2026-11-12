/* EXPERIMENTAL: the length-prefixed frame API is not the deployed chat contract.
 * The authority is the HTTP `POST /api/chat {message}` + bearer session surface in
 * `nix-config modules/lib/kanbei/ui/server.py`, implemented in `./http.ts`.
 * This frame API stays only for spike/property tests and must not be exposed.
 */
import { Data, Effect, Option } from "effect"

export class ChatTransportError extends Data.TaggedError("ChatTransportError")<{
  readonly operation: "read" | "write" | "close"
  readonly reason: string
}> {}

/** The host relay presents itself as `relay`; loopback is never authority. */
export type ChatPeer = "loopback" | "relay"

/** One authenticated byte stream. Only whole JSON frames cross it. */
export interface ChatStream {
  readonly peer: ChatPeer
  readonly readFrame: Effect.Effect<Option.Option<Uint8Array>, ChatTransportError>
  readonly writeFrame: (frame: Uint8Array) => Effect.Effect<void, ChatTransportError>
  readonly close: Effect.Effect<void, ChatTransportError>
}
