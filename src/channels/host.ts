import { Context, Data, Effect, Exit, Layer, Option, Result } from "effect"
import type { CandidateSupervisor } from "../candidates/supervisor.ts"
import { CandidateSupervisor as CandidateSupervisorTag } from "../candidates/supervisor.ts"
import type { ChatHttp, ChatHttpConfig, ChatHttpRequest } from "./chat/http.ts"
import { CHAT_MAX_BODY_BYTES, ChatHttp as ChatHttpTag } from "./chat/http.ts"
import type { CsrClient, CsrConfig } from "./csr/client.ts"
import { CsrClient as CsrClientTag } from "./csr/client.ts"
import type { FreshnessReport } from "./inbox/freshness.ts"
import { FreshnessWatcher as FreshnessWatcherTag, statusSources } from "./inbox/freshness.ts"
import type { InboxSnapshot } from "./inbox/reader.ts"
import { InboxReader as InboxReaderTag } from "./inbox/reader.ts"
import type { SearchClient, SearchConfig } from "./search/client.ts"
import { SearchClient as SearchClientTag } from "./search/client.ts"
import type { TodoLimits, TodoWriter } from "./todo/writer.ts"
import { TodoWriter as TodoWriterTag } from "./todo/writer.ts"

/**
 * The composition point every channel is installed through. Each service is
 * acquired here, so a channel that cannot construct refuses the boot instead of
 * disappearing from the surface silently.
 */
export interface ChannelHostConfig {
  readonly chat: ChatHttpConfig
  readonly csr: CsrConfig
  readonly search: SearchConfig
  readonly todo: TodoLimits
}

export interface StatusSnapshot {
  readonly ok: true
  readonly sources: ReadonlyArray<{ origin: string; state: string; lastSequence?: number }>
  readonly inbox: {
    readonly observations: number
    readonly receipts: number
    readonly heartbeats: number
    readonly gaps: number
    readonly quarantined: number
    readonly duplicates: number
  }
  readonly candidates: number
  readonly channels: ReadonlyArray<string>
}

export interface StatusSource {
  readonly snapshot: Effect.Effect<StatusSnapshot>
}

export const StatusSource = Context.Service<StatusSource>("kanbei/channel/host/StatusSource")

export interface ChannelHost {
  readonly chat: ChatHttp
  readonly csr: CsrClient
  readonly search: SearchClient
  readonly todo: TodoWriter
  readonly candidates: CandidateSupervisor
  readonly status: StatusSource
}

export const ChannelHost = Context.Service<ChannelHost>("kanbei/channel/host/ChannelHost")

/** The declared listeners. Only loopback is reachable inside the NIC-less guest. */
export interface ListenTarget {
  readonly host: string
  readonly port: number
}

export type HttpHandler = (request: Request) => Effect.Effect<Response>

export class HttpListenError extends Data.TaggedError("HttpListenError")<{
  readonly target: string
  readonly reason: string
}> {}

export interface RunningListener {
  readonly target: ListenTarget
  readonly stop: Effect.Effect<void>
}

export interface HttpServer {
  readonly serve: (
    target: ListenTarget,
    handler: HttpHandler,
  ) => Effect.Effect<RunningListener, HttpListenError>
}

export const HttpServer = Context.Service<HttpServer>("kanbei/channel/host/HttpServer")

export class ChannelStartError extends Data.TaggedError("ChannelStartError")<{
  readonly channel: string
  readonly reason: string
}> {}

export const CHANNELS = ["chat", "csr", "search", "todo"] as const

const statusOf = (
  report: FreshnessReport,
  inbox: InboxSnapshot,
  candidates: number,
): StatusSnapshot => ({
  ok: true,
  sources: statusSources(report),
  inbox: {
    observations: inbox.observations.length,
    receipts: inbox.receipts.length,
    heartbeats: inbox.heartbeats.length,
    gaps: inbox.gaps.length,
    quarantined: inbox.quarantined.length,
    duplicates: inbox.duplicates,
  },
  candidates,
  channels: [...CHANNELS, "inbox", "status"],
})

export const channelHostLayer = (
  _config: ChannelHostConfig,
): Layer.Layer<
  ChannelHost,
  never,
  | ChatHttp
  | CsrClient
  | SearchClient
  | TodoWriter
  | CandidateSupervisorTag
  | InboxReaderTag
  | FreshnessWatcherTag
> =>
  Layer.effect(
    ChannelHost,
    Effect.gen(function* () {
      const chat = yield* ChatHttpTag
      const csr = yield* CsrClientTag
      const search = yield* SearchClientTag
      const todo = yield* TodoWriterTag
      const candidates = yield* CandidateSupervisorTag
      const reader = yield* InboxReaderTag
      const freshness = yield* FreshnessWatcherTag
      return {
        chat,
        csr,
        search,
        todo,
        candidates,
        status: {
          snapshot: Effect.gen(function* () {
            const report = yield* freshness.report
            const inbox = yield* reader.state
            const staged = yield* candidates.candidates
            return statusOf(report, inbox, staged.length)
          }),
        },
      }
    }),
  )

const headerMap = (headers: Headers): Readonly<Record<string, string>> => {
  const entries: Record<string, string> = {}
  headers.forEach((value, name) => {
    entries[name.toLowerCase()] = value
  })
  return entries
}

const jsonResponse = (status: number, payload: unknown): Response =>
  new Response(JSON.stringify(payload), {
    status,
    headers: {
      "content-type": "application/json; charset=utf-8",
      "cache-control": "no-store",
      "x-content-type-options": "nosniff",
    },
  })

const declaredLength = (request: Request): Option.Option<number> => {
  const header = request.headers.get("content-length")
  if (header === null) return Option.none()
  const value = Number(header)
  return Number.isInteger(value) && value >= 0 ? Option.some(value) : Option.none()
}

/**
 * The chat surface. The listener is loopback-only: the in-guest shell is the
 * relay boundary (it consumed the host relay token before TLS), and `/api/chat`
 * still requires a bearer passkey session.
 */
export const chatHandler = (chat: ChatHttp): HttpHandler => (request) =>
  Effect.gen(function* () {
    const length = declaredLength(request)
    if (Option.isSome(length) && length.value > CHAT_MAX_BODY_BYTES) {
      return jsonResponse(413, { error: `body exceeds ${CHAT_MAX_BODY_BYTES} bytes` })
    }
    const method = request.method === "POST" ? "POST" : request.method === "HEAD" ? "HEAD" : "GET"
    const body = method === "GET" || method === "HEAD"
      ? Result.succeed(new Uint8Array())
      : yield* Effect.result(Effect.tryPromise({
        try: async () => new Uint8Array(await request.arrayBuffer()),
        catch: (error) => error,
      }))
    if (Result.isFailure(body)) return jsonResponse(400, { error: "body could not be read" })
    const chatRequest: ChatHttpRequest = {
      method,
      path: new URL(request.url).pathname,
      headers: headerMap(request.headers),
      body: body.success,
      relayAuthenticated: true,
    }
    const response = yield* chat.handle(chatRequest)
    const bytes = new Uint8Array(response.body.length)
    bytes.set(response.body)
    return new Response(bytes, { status: response.status, headers: response.headers })
  })

export const statusHandler = (status: StatusSource): HttpHandler => (request) =>
  Effect.gen(function* () {
    const path = new URL(request.url).pathname
    if (request.method !== "GET" && request.method !== "HEAD") {
      return jsonResponse(405, { error: "method not allowed" })
    }
    if (path === "/healthz") return jsonResponse(200, { ok: true })
    if (path === "/status") return jsonResponse(200, yield* status.snapshot)
    return jsonResponse(404, { error: "not found" })
  })

const readiness = (host: ChannelHost): ReadonlyArray<readonly [string, Effect.Effect<unknown>]> => [
  ["chat", Effect.succeed(host.chat)],
  ["csr", host.csr.state],
  ["search", host.search.receipts],
  ["todo", host.todo.receipts],
  ["candidates", host.candidates.candidates],
  ["inbox", host.status.snapshot],
]

export interface RunningChannels {
  readonly chat: RunningListener
  readonly status: RunningListener
  readonly stop: Effect.Effect<void>
}

/**
 * Fail closed: every channel service proves itself readable, then both declared
 * listeners bind. Any refusal stops the already-bound listener and fails the boot.
 */
export const startChannelHost = (targets: {
  readonly chat: ListenTarget
  readonly status: ListenTarget
}): Effect.Effect<RunningChannels, ChannelStartError, ChannelHost | HttpServer> =>
  Effect.gen(function* () {
    const host = yield* ChannelHost
    const server = yield* HttpServer
    const results = yield* Effect.forEach(
      readiness(host),
      ([channel, effect]) => Effect.exit(effect).pipe(Effect.map((exit) => ({ channel, exit }))),
      { concurrency: 1 },
    )
    const refused = results.find((result) => !Exit.isSuccess(result.exit))
    if (refused !== undefined) {
      return yield* Effect.fail(
        new ChannelStartError({
          channel: refused.channel,
          reason: "the channel did not answer its readiness check",
        }),
      )
    }
    const chat = yield* server.serve(targets.chat, chatHandler(host.chat)).pipe(
      Effect.mapError((error) => new ChannelStartError({ channel: "chat", reason: error.reason })),
    )
    const status = yield* server.serve(targets.status, statusHandler(host.status)).pipe(
      Effect.mapError((error) =>
        new ChannelStartError({ channel: "status", reason: error.reason })
      ),
      // A status listener that cannot bind must not leave chat serving alone.
      Effect.tapError(() => chat.stop),
    )
    return {
      chat,
      status,
      stop: chat.stop.pipe(Effect.flatMap(() => status.stop)),
    }
  })
