import { Data, Effect, Layer, Schema } from "effect"
import { fileBlobSinkLayer } from "./adapters/blob-sink.ts"
import { fileInboxCheckpointLayer } from "./adapters/inbox-checkpoint.ts"
import { directoryLayer } from "./adapters/inbox-directory.ts"
import { fileLedgerStoreLayer } from "./adapters/ledger-file.ts"
import { ageDecryptorLayer } from "./adapters/age.ts"
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
import { MAX_BATCH_BYTES } from "./channels/inbox/wire.ts"
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
 * a missing manifest, posture, or state directory stops the process instead of
 * starting a partially configured kernel.
 */
export class BootError extends Data.TaggedError("BootError")<{
  readonly variable: string
  readonly reason: string
}> {}

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
    maxEntriesPerBatch: 100,
    maxEntryBytes: 100_000,
    maxTotalBlobBytes: 8 * MAX_BATCH_BYTES,
  },
  freshnessWindowMs: config.freshnessWindowMs,
})

const kernelLayers = (config: BootConfig) => {
  const blobs = fileBlobSinkLayer(`${config.stateDir}/cas`)
  const cas = contentStoreLayer.pipe(Layer.provide(blobs))
  const ledger = ledgerLayer(config.epochId).pipe(
    Layer.provide(fileLedgerStoreLayer(`${config.stateDir}/ledger.jsonl`)),
  )
  const checkpoints = fileInboxCheckpointLayer(`${config.stateDir}/inbox-checkpoint.json`)
  const reader = inboxReaderLayer(inboxConfigFor(config)).pipe(
    Layer.provide(directoryLayer(config.inboxDir)),
    Layer.provide(ageDecryptorLayer({ identityPath: config.identityPath })),
    Layer.provide(checkpoints),
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

/**
 * Boot the supervisor, verify the declared posture, then poll the inbox and
 * probe freshness forever. Control never reaches a channel before the posture
 * check succeeds.
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

export const boot = (
  config: BootConfig,
): Effect.Effect<
  never,
  BootError,
  Supervisor | InboxReader | FreshnessWatcher | ContentStore | Ledger
> =>
  Effect.gen(function* () {
    yield* startKernel(config)
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
export const runBoot = (config: BootConfig): Effect.Effect<never, BootError> =>
  boot(config).pipe(
    Effect.provide(Layer.mergeAll(supervisorLayer(), kernelLayers(config))),
  )

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
