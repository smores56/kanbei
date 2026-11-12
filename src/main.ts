import { Data, Effect, Layer, Option, Schema } from "effect"
import { ageDecryptorLayer } from "./adapters/age.ts"
import { fileBlobSinkLayer } from "./adapters/blob-sink.ts"
import { chatUpstreamLayer } from "./adapters/chat-upstream.ts"
import { fileCertificateStoreLayer } from "./adapters/csr-store.ts"
import { vsockCsrTransportLayer } from "./adapters/csr-vsock.ts"
import { denoHttpServerLayer } from "./adapters/http-listener.ts"
import { fileInboxCheckpointLayer } from "./adapters/inbox-checkpoint.ts"
import { fileInboxQuarantineLayer } from "./adapters/inbox-quarantine.ts"
import { directoryLayer } from "./adapters/inbox-directory.ts"
import { fileLedgerStoreLayer } from "./adapters/ledger-file.ts"
import { pointerFileLayer } from "./adapters/pointer-file.ts"
import { searchBudgetFileLayer } from "./adapters/search-budget-file.ts"
import { vsockSearchChannelLayer } from "./adapters/search-vsock.ts"
import { todoFilesLayer } from "./adapters/todo-files.ts"
import { candidateSupervisorLayer } from "./candidates/supervisor.ts"
import { HumanSessions, refusingConsentDeliveryLayer } from "./candidates/consent.ts"
import {
  CHAT_MAX_BODY_BYTES,
  CHAT_MAX_MESSAGE_CHARS,
  CHAT_STUB_REPLY,
  chatHttpLayer,
  ChatUpstream,
  ChatUpstreamError,
  SessionStore,
  sessionStoreLayer,
} from "./channels/chat/http.ts"
import { AlertSink, CSR_RENEWAL_WINDOW_MS, csrClientLayer } from "./channels/csr/client.ts"
import { CSR_COMMON_NAME } from "./channels/csr/schema.ts"
import { ChannelHost, channelHostLayer, HttpServer, startChannelHost } from "./channels/host.ts"
import type { ChannelHostConfig, ListenTarget, RunningChannels } from "./channels/host.ts"
import type { InboxConfig } from "./channels/inbox/reader.ts"
import { InboxReader, inboxReaderLayer } from "./channels/inbox/reader.ts"
import type { StalenessAlert } from "./channels/inbox/freshness.ts"
import {
  FreshnessWatcher,
  freshnessWatcherLayer,
  inboxFreshnessFeedLayer,
  StalenessAlertSink,
} from "./channels/inbox/freshness.ts"
import { OriginId } from "./channels/inbox/schema.ts"
import { MAX_BATCH_BYTES, MAX_WIRE_ITEMS } from "./channels/inbox/wire.ts"
import { searchClientLayer } from "./channels/search/client.ts"
import {
  SEARCH_MAX_QUERY_CHARS,
  SEARCH_MAX_RESULTS,
  SEARCH_MAX_SNIPPET_CHARS,
} from "./channels/search/schema.ts"
import type { TodoLimits } from "./channels/todo/writer.ts"
import { todoWriterLayer } from "./channels/todo/writer.ts"
import { ContentStore, contentStoreLayer } from "./kernel/cas.ts"
import { Ledger, ledgerLayer } from "./kernel/ledger.ts"
import type { ObservedPosture, SandboxManifest } from "./kernel/manifest.ts"
import {
  ObservedPosture as ObservedPostureSchema,
  SandboxManifest as ManifestSchema,
} from "./kernel/manifest.ts"
import { Supervisor, supervisorLayer } from "./supervisor/supervisor.ts"
import type { StartedSandbox } from "./supervisor/supervisor.ts"

/**
 * The composition root. Every input is explicit and every failure is closed:
 * a missing manifest, posture, listener, or state directory stops the process
 * instead of starting a partially configured kernel.
 */
export class BootError extends Data.TaggedError("BootError")<{
  readonly variable: string
  readonly reason: string
}> {}

/** The deployed ingest todo cap (`docs/design/07` §4.9); no silent default here. */
export const TODO_LIMITS: TodoLimits = {
  maxBytes: 65_536,
  maxWriteBytes: 4_096,
  maxLines: 200,
  maxLineChars: 512,
}

export interface BootConfig {
  readonly manifestPath: string
  readonly posturePath: string
  readonly stateDir: string
  readonly inboxDir: string
  readonly identityPath: string
  readonly epochId: string
  readonly allowedOrigins: ReadonlyArray<OriginId>
  readonly freshnessWindowMs: number
  readonly maxBatchesPerPoll: number
  readonly todoDir: string
  readonly chatListen: ListenTarget
  readonly statusListen: ListenTarget
  readonly csrSocket: string
  readonly searchSocket: string
  readonly relayToken: string
  readonly csrStateDir: string
  readonly searchBudgetPath: string
  readonly pointerDir: string
  readonly chatOrigin: string
  readonly chatUpstream?: string
}

const required = (
  env: (name: string) => string | undefined,
  name: string,
): Effect.Effect<string, BootError> => {
  const value = env(name)?.trim()
  return value === undefined || value === ""
    ? Effect.fail(new BootError({ variable: name, reason: "is required" }))
    : Effect.succeed(value)
}

const positive = (
  env: (name: string) => string | undefined,
  name: string,
  fallback: number,
): Effect.Effect<number, BootError> =>
  Effect.gen(function* () {
    const raw = env(name)?.trim()
    if (raw === undefined || raw === "") return fallback
    const value = Number(raw)
    return Number.isInteger(value) && value > 0
      ? value
      : yield* Effect.fail(new BootError({ variable: name, reason: "must be a positive integer" }))
  })

const absolute = (variable: string, value: string): Effect.Effect<string, BootError> =>
  value.startsWith("/")
    ? Effect.succeed(value)
    : Effect.fail(new BootError({ variable, reason: "must be an absolute path" }))

const optional = (env: (name: string) => string | undefined, name: string): string | undefined => {
  const value = env(name)?.trim()
  return value === undefined || value === "" ? undefined : value
}

/** `host:port`, and the host must be loopback: the guest has no other address. */
export const listenTarget = (
  env: (name: string) => string | undefined,
  name: string,
): Effect.Effect<ListenTarget, BootError> =>
  Effect.gen(function* () {
    const raw = yield* required(env, name)
    const separator = raw.lastIndexOf(":")
    const host = separator === -1 ? "" : raw.slice(0, separator).trim()
    const port = separator === -1 ? Number.NaN : Number(raw.slice(separator + 1).trim())
    if (host !== "127.0.0.1") {
      return yield* Effect.fail(
        new BootError({ variable: name, reason: "must bind 127.0.0.1 (the guest has no NIC)" }),
      )
    }
    if (!Number.isInteger(port) || port < 0 || port > 65_535) {
      return yield* Effect.fail(new BootError({ variable: name, reason: "has an invalid port" }))
    }
    return { host, port }
  })

const readRelayToken = (path: string): Effect.Effect<string, BootError> =>
  Effect.tryPromise({
    try: () => Deno.readTextFile(path),
    catch: () => new BootError({ variable: "KANBEI_RELAY_TOKEN", reason: `cannot read ${path}` }),
  }).pipe(
    Effect.flatMap((text) => {
      const token = text.trim()
      return token === ""
        ? Effect.fail(new BootError({ variable: "KANBEI_RELAY_TOKEN", reason: "is empty" }))
        : Effect.succeed(token)
    }),
  )

/** Every setting comes from `KANBEI_*`; there is no compiled-in default path. */
export const bootConfigFromEnv = (
  env: (name: string) => string | undefined,
): Effect.Effect<BootConfig, BootError> =>
  Effect.gen(function* () {
    const manifestPath = yield* absolute("KANBEI_MANIFEST", yield* required(env, "KANBEI_MANIFEST"))
    const posturePath = yield* absolute("KANBEI_POSTURE", yield* required(env, "KANBEI_POSTURE"))
    const stateDir = yield* absolute("KANBEI_STATE_DIR", yield* required(env, "KANBEI_STATE_DIR"))
    const inboxDir = yield* absolute("KANBEI_INBOX_DIR", yield* required(env, "KANBEI_INBOX_DIR"))
    const identityPath = yield* absolute(
      "KANBEI_IDENTITY",
      yield* required(env, "KANBEI_IDENTITY"),
    )
    const todoDir = yield* absolute("KANBEI_TODO_DIR", yield* required(env, "KANBEI_TODO_DIR"))
    const csrSocket = yield* absolute(
      "KANBEI_CSR_SOCKET",
      yield* required(env, "KANBEI_CSR_SOCKET"),
    )
    const searchSocket = yield* absolute(
      "KANBEI_SEARCH_SOCKET",
      yield* required(env, "KANBEI_SEARCH_SOCKET"),
    )
    const relayTokenPath = yield* absolute(
      "KANBEI_RELAY_TOKEN",
      yield* required(env, "KANBEI_RELAY_TOKEN"),
    )
    const csrStateDir = yield* absolute(
      "KANBEI_CSR_STATE_DIR",
      yield* required(env, "KANBEI_CSR_STATE_DIR"),
    )
    const searchBudgetPath = yield* absolute(
      "KANBEI_SEARCH_BUDGET",
      yield* required(env, "KANBEI_SEARCH_BUDGET"),
    )
    const pointerDir = yield* absolute(
      "KANBEI_POINTERS",
      yield* required(env, "KANBEI_POINTERS"),
    )
    const chatOrigin = yield* required(env, "KANBEI_CHAT_ORIGIN")
    const chatListen = yield* listenTarget(env, "KANBEI_CHAT_LISTEN")
    const statusListen = yield* listenTarget(env, "KANBEI_STATUS_LISTEN")
    const chatUpstream = optional(env, "KANBEI_CHAT_UPSTREAM")
    if (chatUpstream !== undefined && !/^http:\/\/127\.0\.0\.1:\d{1,5}\//u.test(chatUpstream)) {
      return yield* Effect.fail(
        new BootError({
          variable: "KANBEI_CHAT_UPSTREAM",
          reason: "must be a loopback http://127.0.0.1:<port>/ endpoint",
        }),
      )
    }
    const epochId = yield* required(env, "KANBEI_EPOCH")
    const rawOrigins = yield* required(env, "KANBEI_ORIGINS")
    const allowedOrigins = yield* Effect.try({
      try: () =>
        Schema.decodeUnknownSync(Schema.Array(OriginId))(
          rawOrigins.split(",").map((origin) => origin.trim()).filter((origin) => origin !== ""),
        ),
      catch: () => new BootError({ variable: "KANBEI_ORIGINS", reason: "has an unknown origin" }),
    })
    if (allowedOrigins.length === 0) {
      return yield* Effect.fail(
        new BootError({ variable: "KANBEI_ORIGINS", reason: "must not be empty" }),
      )
    }
    return {
      manifestPath,
      posturePath,
      stateDir,
      inboxDir,
      identityPath,
      epochId,
      allowedOrigins,
      freshnessWindowMs: yield* positive(env, "KANBEI_FRESHNESS_WINDOW_MS", 3_600_000),
      maxBatchesPerPoll: yield* positive(env, "KANBEI_MAX_BATCHES_PER_POLL", 20),
      todoDir,
      chatListen,
      statusListen,
      csrSocket,
      searchSocket,
      relayToken: yield* readRelayToken(relayTokenPath),
      csrStateDir,
      searchBudgetPath,
      pointerDir,
      chatOrigin,
      ...(chatUpstream === undefined ? {} : { chatUpstream }),
    }
  })

const readJson = (path: string): Effect.Effect<unknown, BootError> =>
  Effect.tryPromise({
    try: async () => JSON.parse(await Deno.readTextFile(path)),
    catch: (error) =>
      new BootError({
        variable: path,
        reason: error instanceof Error ? error.message : String(error),
      }),
  })

const parseManifest = (value: unknown): Effect.Effect<SandboxManifest, BootError> =>
  Effect.try({
    try: () => Schema.decodeUnknownSync(ManifestSchema, { onExcessProperty: "error" })(value),
    catch: () => new BootError({ variable: "KANBEI_MANIFEST", reason: "is not a manifest" }),
  })

const parsePosture = (value: unknown): Effect.Effect<ObservedPosture, BootError> =>
  Effect.try({
    try: () =>
      Schema.decodeUnknownSync(ObservedPostureSchema, { onExcessProperty: "error" })(value),
    catch: () => new BootError({ variable: "KANBEI_POSTURE", reason: "is not a posture" }),
  })

export const inboxConfigFor = (config: BootConfig): InboxConfig => ({
  identityPath: config.identityPath,
  allowedOrigins: config.allowedOrigins,
  limits: {
    maxBatchBytes: MAX_BATCH_BYTES,
    maxBatchesPerPoll: config.maxBatchesPerPoll,
    // The host writer caps one envelope at 1000 items; a reader below that lies.
    maxEntriesPerBatch: MAX_WIRE_ITEMS,
    maxEntryBytes: 100_000,
    maxTotalBlobBytes: 8 * MAX_BATCH_BYTES,
  },
  freshnessWindowMs: config.freshnessWindowMs,
})

export const kernelLayers = (config: BootConfig) => {
  const blobs = fileBlobSinkLayer(`${config.stateDir}/cas`)
  const cas = contentStoreLayer.pipe(Layer.provide(blobs))
  const ledger = ledgerLayer(config.epochId).pipe(
    Layer.provide(fileLedgerStoreLayer(`${config.stateDir}/ledger.jsonl`)),
  )
  const checkpoints = fileInboxCheckpointLayer(`${config.stateDir}/inbox-checkpoint.json`)
  const quarantine = fileInboxQuarantineLayer(`${config.stateDir}/inbox-quarantine.json`)
  const reader = inboxReaderLayer(inboxConfigFor(config)).pipe(
    Layer.provide(directoryLayer(config.inboxDir)),
    Layer.provide(ageDecryptorLayer({ identityPath: config.identityPath })),
    Layer.provide(checkpoints),
    Layer.provide(quarantine),
    Layer.provide(cas),
    Layer.provide(ledger),
  )
  const alerts = Layer.succeed(StalenessAlertSink)({
    emit: (alert: StalenessAlert) =>
      Effect.logWarning("stale ingest source", {
        origin: alert.origin,
        kind: alert.kind,
        lastSequence: alert.lastSequence,
      }),
  })
  const feed = inboxFreshnessFeedLayer.pipe(Layer.provide(reader))
  const watcher = freshnessWatcherLayer({
    windows: {},
    defaultWindowMs: config.freshnessWindowMs,
    knownOrigins: config.allowedOrigins,
  }).pipe(Layer.provide(feed), Layer.provide(alerts))
  return Layer.mergeAll(reader, cas, ledger, checkpoints, alerts, watcher)
}

export const channelHostConfig = (config: BootConfig): ChannelHostConfig => ({
  chat: {
    upstream: config.chatUpstream !== undefined,
    stubReply: CHAT_STUB_REPLY,
    maxBodyBytes: CHAT_MAX_BODY_BYTES,
    maxMessageChars: CHAT_MAX_MESSAGE_CHARS,
  },
  csr: { commonName: CSR_COMMON_NAME, renewalWindowMs: CSR_RENEWAL_WINDOW_MS },
  search: {
    maxQueryChars: SEARCH_MAX_QUERY_CHARS,
    maxLimit: SEARCH_MAX_RESULTS,
    maxTextChars: SEARCH_MAX_SNIPPET_CHARS,
    queriesPerHour: 60,
    queriesPerDay: 400,
  },
  todo: TODO_LIMITS,
})

/**
 * A ruling session is a live chat bearer session for the owner origin: the
 * consent surface never mints a human session of its own.
 */
const humanSessionsLayer = (
  sessions: Layer.Layer<SessionStore>,
  origin: string,
): Layer.Layer<HumanSessions> =>
  Layer.effect(
    HumanSessions,
    Effect.gen(function* () {
      const store = yield* SessionStore
      return {
        verify: (sessionId, sessionOrigin) =>
          sessionOrigin === origin && sessionId.trim() !== ""
            ? store.validate(sessionId).pipe(
              Effect.map((valid) =>
                valid
                  ? Option.some({ sessionId, origin: sessionOrigin, verifiedAtUs: 0 })
                  : Option.none()
              ),
            )
            : Effect.succeed(Option.none()),
      }
    }),
  ).pipe(Layer.provide(sessions))

const noChatLane: Layer.Layer<ChatUpstream> = Layer.succeed(ChatUpstream)({
  reply: () => Effect.fail(new ChatUpstreamError({ reason: "no chat lane is configured" })),
})

/** Every channel service the kernel exposes, wired to its deployed transport. */
export const channelLayers = (config: BootConfig) => {
  const sessions = sessionStoreLayer()
  const upstream = config.chatUpstream === undefined
    ? noChatLane
    : chatUpstreamLayer(config.chatUpstream)
  const chat = chatHttpLayer(channelHostConfig(config).chat).pipe(
    Layer.provide(sessions),
    Layer.provide(upstream),
  )
  const csr = csrClientLayer(channelHostConfig(config).csr).pipe(
    Layer.provide(vsockCsrTransportLayer({ path: config.csrSocket, token: config.relayToken })),
    Layer.provide(fileCertificateStoreLayer(config.csrStateDir)),
    Layer.provide(
      Layer.succeed(AlertSink)({
        emit: (alert) => Effect.logWarning("certificate alert", alert),
      }),
    ),
  )
  const search = searchClientLayer(channelHostConfig(config).search).pipe(
    Layer.provide(vsockSearchChannelLayer({ path: config.searchSocket, token: config.relayToken })),
    Layer.provide(searchBudgetFileLayer(config.searchBudgetPath)),
  )
  const todo = todoWriterLayer(TODO_LIMITS).pipe(Layer.provide(todoFilesLayer(config.todoDir)))
  const candidates = candidateSupervisorLayer.pipe(
    Layer.provide(pointerFileLayer(config.pointerDir)),
    Layer.provide(humanSessionsLayer(sessions, config.chatOrigin)),
    // No delivery port exists yet; refusing keeps a token from being dropped.
    Layer.provide(refusingConsentDeliveryLayer),
  )
  const host = channelHostLayer(channelHostConfig(config)).pipe(
    Layer.provide(chat),
    Layer.provide(csr),
    Layer.provide(search),
    Layer.provide(todo),
    Layer.provide(candidates),
  )
  return Layer.mergeAll(chat, csr, search, todo, candidates, host)
}

/**
 * Boot the supervisor, verify the declared posture, bind every declared
 * listener, then poll the inbox and probe freshness forever. Control never
 * reaches a channel before the posture check succeeds, and a channel that
 * cannot start refuses the boot.
 */
export const startKernel = (
  config: BootConfig,
): Effect.Effect<StartedSandbox, BootError, Supervisor> =>
  Effect.gen(function* () {
    const manifest = yield* parseManifest(yield* readJson(config.manifestPath))
    const posture = yield* parsePosture(yield* readJson(config.posturePath))
    const supervisor = yield* Supervisor
    const started = yield* supervisor.start(manifest, posture).pipe(
      Effect.mapError((refusal) =>
        new BootError({
          variable: "KANBEI_POSTURE",
          reason: `${refusal.code}: ${refusal.detail}`,
        })
      ),
    )
    yield* Effect.logInfo("kanbei started", {
      sandbox: started.sandboxId,
      channels: started.channels,
    })
    return started
  })

/** Bind the chat and status surfaces; a refused channel refuses the whole boot. */
export const startChannels = (
  config: BootConfig,
): Effect.Effect<RunningChannels, BootError, ChannelHost | HttpServer> =>
  startChannelHost({ chat: config.chatListen, status: config.statusListen }).pipe(
    Effect.mapError((error) =>
      new BootError({
        variable: `KANBEI_${error.channel.toUpperCase()}_START`,
        reason: error.reason,
      })
    ),
  )

export const boot = (
  config: BootConfig,
): Effect.Effect<
  never,
  BootError,
  | Supervisor
  | InboxReader
  | FreshnessWatcher
  | ContentStore
  | Ledger
  | ChannelHost
  | HttpServer
> =>
  Effect.gen(function* () {
    yield* startKernel(config)
    const listeners = yield* startChannels(config)
    yield* Effect.logInfo("kanbei channels listening", {
      chat: listeners.chat.target,
      status: listeners.status.target,
    })
    const inbox = yield* InboxReader
    const freshness = yield* FreshnessWatcher
    return yield* Effect.forever(
      Effect.gen(function* () {
        const poll = yield* inbox.poll.pipe(
          Effect.mapError((error) =>
            new BootError({ variable: "KANBEI_INBOX", reason: error.reason })
          ),
        )
        yield* Effect.logInfo("inbox poll", poll)
        yield* freshness.probe
        yield* Effect.sleep("60 seconds")
      }),
    )
  })

/** Run the composition root exactly once with the supervisor and channel layers. */
export const runBoot = (config: BootConfig): Effect.Effect<never, BootError> => {
  const kernel = kernelLayers(config)
  // The candidate supervisor needs Ledger and ContentStore at build time; the
  // same kernel layer value is memoized, so one ledger instance serves both.
  const channels = channelLayers(config).pipe(Layer.provide(kernel))
  return boot(config).pipe(
    Effect.provide(
      Layer.mergeAll(
        supervisorLayer(),
        kernel,
        channels,
        denoHttpServerLayer,
      ),
    ),
  )
}

if (import.meta.main && Deno.args[0] === "decrypt-smoke") {
  // The installed wrapper owns the flags and the PATH entry for `age`; this
  // subcommand proves the packaged closure can decrypt a real batch.
  const { runDecryptSmoke } = await import("./cli/decrypt-smoke.ts")
  Deno.exit(await runDecryptSmoke(Deno.args.slice(1)))
}

if (import.meta.main) {
  const program = Effect.gen(function* () {
    const config = yield* bootConfigFromEnv((name) => Deno.env.get(name))
    return yield* runBoot(config)
  }).pipe(
    Effect.catch((error: BootError) =>
      Effect.logError(`kanbei: boot refused: ${error.variable}: ${error.reason}`).pipe(
        Effect.as(2 as const),
      )
    ),
  )
  Deno.exit(await Effect.runPromise(program))
}
