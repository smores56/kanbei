import { Context, Data, Effect, Layer, Option, Result } from "effect"
import { decodeFrame, encodeFrame } from "./frame.ts"
import type { ChatEvent, ChatRequest, ChatResponse } from "./schema.ts"
import { parseChatOutbound } from "./schema.ts"
import type { ChatStream, ChatTransportError } from "./stream.ts"

export type ChatClientErrorCode =
  | "chat_client_frame_invalid"
  | "chat_client_out_of_order"
  | "chat_client_incomplete"

export class ChatClientError extends Data.TaggedError("ChatClientError")<{
  readonly code: ChatClientErrorCode
  readonly detail: string
}> {}

export interface ChatExchange {
  readonly events: ReadonlyArray<ChatEvent>
  readonly response: ChatResponse
  readonly sessionId: string
}

export interface ChatClient {
  readonly request: (
    stream: ChatStream,
    request: Omit<ChatRequest, "sessionId"> & { readonly sessionId?: string },
  ) => Effect.Effect<ChatExchange, ChatClientError | ChatTransportError>
}

export const ChatClient = Context.Service<ChatClient>("kanbei/channel/chat/Client")

const malformed = (detail: string): ChatClientError =>
  new ChatClientError({ code: "chat_client_frame_invalid", detail })

export const chatClientLayer = (
  maxFrameBytes: number,
): Layer.Layer<ChatClient> =>
  Layer.effect(
    ChatClient,
    Effect.succeed({
      request: (stream, request) => {
        const read = (
          events: ReadonlyArray<ChatEvent>,
        ): Effect.Effect<ChatExchange, ChatClientError | ChatTransportError> =>
          Effect.gen(function* () {
            const raw = yield* stream.readFrame
            if (Option.isNone(raw)) {
              return yield* Effect.fail(
                new ChatClientError({
                  code: "chat_client_incomplete",
                  detail: "the stream ended before a response",
                }),
              )
            }
            const decoded = decodeFrame(raw.value, maxFrameBytes)
            if (Result.isFailure(decoded)) {
              return yield* Effect.fail(malformed(decoded.failure.detail))
            }
            const parsed = Result.try({
              try: () => parseChatOutbound(decoded.success),
              catch: (error) =>
                malformed(error instanceof Error ? error.message.slice(0, 200) : String(error)),
            })
            if (Result.isFailure(parsed)) return yield* Effect.fail(parsed.failure)
            if (parsed.success.kind === "response") {
              return {
                events,
                response: parsed.success,
                sessionId: parsed.success.sessionId ?? "",
              }
            }
            if (parsed.success.seq !== events.length) {
              return yield* Effect.fail(
                new ChatClientError({
                  code: "chat_client_out_of_order",
                  detail: `event ${parsed.success.seq} arrived at position ${events.length}`,
                }),
              )
            }
            return yield* read([...events, parsed.success])
          })

        return stream.writeFrame(encodeFrame(request)).pipe(
          Effect.flatMap(() => read([])),
          Effect.ensuring(Effect.ignore(stream.close)),
        )
      },
    }),
  )
