import { Effect, Layer, Option, Ref, Result } from "effect"
import type { ChatConfig, ChatReceipt } from "../../../src/channels/chat/server.ts"
import {
  ChatResponder,
  ChatServer,
  chatServerLayer,
  ChatViews,
} from "../../../src/channels/chat/server.ts"
import type { ChatPeer, ChatStream } from "../../../src/channels/chat/stream.ts"
import { decodeFrame, encodeFrame } from "../../../src/channels/chat/frame.ts"
import type { ChatOutbound, ChatResponse } from "../../../src/channels/chat/schema.ts"
import { parseChatOutbound } from "../../../src/channels/chat/schema.ts"
import { hash } from "../../support.ts"

export const PASSKEY = "correct horse battery staple"
export const IDENTITY = "owner@example.invalid"
export const ORIGIN = "https://kanbei.sammohr.dev"

export const config = async (overrides: Partial<ChatConfig> = {}): Promise<ChatConfig> => ({
  accessIdentity: IDENTITY,
  passkeyDigest: await hash(PASSKEY),
  allowedOrigin: ORIGIN,
  maxRequestBytes: 8_192,
  maxResponseBytes: 8_192,
  maxTurnChars: 200,
  maxDecisions: 5,
  requestsPerHour: 100,
  sessionTtlMs: 3_600_000,
  ...overrides,
})

export const responderLayer: Layer.Layer<ChatResponder> = Layer.succeed(ChatResponder)({
  reply: (turn) => Effect.succeed({ text: `reply to ${turn.turnId}: ${turn.text}` }),
})

export const viewsLayer: Layer.Layer<ChatViews> = Layer.succeed(ChatViews)({
  decisions: Effect.succeed([
    {
      id: "dec-1",
      title: "Approve plan",
      severity: "decision-required",
      basis: "plan-1",
    },
    {
      id: "dec-2",
      title: "Review stale source",
      severity: "informational",
      basis: "heartbeat",
    },
    {
      id: "dec-3",
      title: "Nothing pending",
      severity: "none",
      basis: "empty",
    },
  ]),
  decision: (id) =>
    Effect.succeed(
      id === "dec-1"
        ? Option.some({
          id: "dec-1",
          title: "Approve plan",
          severity: "decision-required" as const,
          basis: "plan-1",
        })
        : Option.none(),
    ),
  status: Effect.succeed({
    generation: "gen-1",
    sources: [
      { origin: "tuta-mail", state: "fresh" as const, lastSequence: 4 },
      { origin: "finance", state: "stale" as const, lastSequence: 2 },
      { origin: "immich", state: "unknown" as const },
    ],
  }),
})

export const serverLayer = (settings: ChatConfig): Layer.Layer<ChatServer> =>
  chatServerLayer(settings).pipe(Layer.provide(Layer.mergeAll(responderLayer, viewsLayer)))

export interface MemoryStream {
  readonly stream: ChatStream
  readonly written: Ref.Ref<ReadonlyArray<Uint8Array>>
  readonly closed: Ref.Ref<boolean>
}

export const memoryStream = (
  frames: ReadonlyArray<Uint8Array>,
  peer: ChatPeer = "relay",
): Effect.Effect<MemoryStream> =>
  Effect.gen(function* () {
    const queue = yield* Ref.make(frames)
    const written = yield* Ref.make<ReadonlyArray<Uint8Array>>([])
    const closed = yield* Ref.make(false)
    const stream: ChatStream = {
      peer,
      readFrame: Ref.modify(queue, (current) => {
        const [head, ...rest] = current
        return head === undefined
          ? [Option.none<Uint8Array>(), current] as const
          : [Option.some(head), rest] as const
      }),
      writeFrame: (frame) => Ref.update(written, (current) => [...current, frame]),
      close: Ref.set(closed, true),
    }
    return { stream, written, closed }
  })

export interface Exchange {
  readonly outbound: ReadonlyArray<ChatOutbound>
  readonly receipts: ReadonlyArray<ChatReceipt>
  readonly closed: boolean
}

export interface Batch {
  readonly exchanges: ReadonlyArray<Exchange>
  readonly receipts: ReadonlyArray<ChatReceipt>
}

export const serveAll = async (
  frames: ReadonlyArray<unknown>,
  options: {
    readonly settings?: Partial<ChatConfig>
    readonly peer?: ChatPeer
  } = {},
): Promise<Batch> => {
  const settings = await config(options.settings)
  const exchanges = await Effect.runPromise(
    Effect.gen(function* () {
      const server = yield* ChatServer
      const served = yield* Effect.forEach(
        frames,
        (frame) =>
          Effect.gen(function* () {
            const memory = yield* memoryStream([encodeFrame(frame)], options.peer)
            yield* server.serve(memory.stream)
            const written = yield* Ref.get(memory.written)
            const outbound = written.map((bytes) => {
              const decoded = decodeFrame(bytes, 1_000_000)
              return Result.isSuccess(decoded)
                ? Option.some(parseChatOutbound(decoded.success))
                : Option.none()
            })
            return {
              outbound: outbound.flatMap((value) => Option.isSome(value) ? [value.value] : []),
              closed: yield* Ref.get(memory.closed),
            }
          }),
        { concurrency: 1 },
      )
      const receipts = yield* server.receipts
      return { served, receipts }
    }).pipe(Effect.provide(serverLayer(settings)), Effect.orDie),
  )
  return {
    exchanges: exchanges.served.map((entry) => ({ ...entry, receipts: exchanges.receipts })),
    receipts: exchanges.receipts,
  }
}

export const serveOne = async (
  frame: unknown,
  options: {
    readonly settings?: Partial<ChatConfig>
    readonly peer?: ChatPeer
  } = {},
): Promise<Exchange> => {
  const batch = await serveAll([frame], options)
  const first = batch.exchanges[0]
  if (first === undefined) throw new Error("no exchange")
  return first
}

export const request = (
  overrides: Record<string, unknown> = {},
): Record<string, unknown> => ({
  route: "status",
  accessIdentity: IDENTITY,
  passkey: PASSKEY,
  origin: ORIGIN,
  ...overrides,
})

export const responseOf = (exchange: Exchange): ChatResponse => {
  const last = exchange.outbound.at(-1)
  if (last === undefined || last.kind !== "response") throw new Error("no response")
  return last
}

/** Serves a chain of requests on one server so a minted session can be reused. */
export const serveChain = async (
  steps: ReadonlyArray<(sessionId: string) => unknown>,
  options: {
    readonly settings?: Partial<ChatConfig>
    readonly peer?: ChatPeer
  } = {},
): Promise<Batch> => {
  const settings = await config(options.settings)
  const result = await Effect.runPromise(
    Effect.gen(function* () {
      const server = yield* ChatServer
      const session = yield* Ref.make("")
      const exchanges = yield* Effect.forEach(
        steps,
        (step) =>
          Effect.gen(function* () {
            const frame = step(yield* Ref.get(session))
            const memory = yield* memoryStream([encodeFrame(frame)], options.peer)
            yield* server.serve(memory.stream)
            const written = yield* Ref.get(memory.written)
            const outbound = written.map((bytes) => {
              const decoded = decodeFrame(bytes, 1_000_000)
              return Result.isSuccess(decoded)
                ? Option.some(parseChatOutbound(decoded.success))
                : Option.none()
            })
            const frames = outbound.flatMap((value) => Option.isSome(value) ? [value.value] : [])
            const last = frames.at(-1)
            if (last !== undefined && last.kind === "response" && last.sessionId !== undefined) {
              yield* Ref.set(session, last.sessionId)
            }
            return { outbound: frames, closed: yield* Ref.get(memory.closed) }
          }),
        { concurrency: 1 },
      )
      const receipts = yield* server.receipts
      return { exchanges, receipts }
    }).pipe(Effect.provide(serverLayer(settings)), Effect.orDie),
  )
  return {
    exchanges: result.exchanges.map((entry) => ({ ...entry, receipts: result.receipts })),
    receipts: result.receipts,
  }
}
