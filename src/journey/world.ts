import { Clock, Effect, Layer, Option, Ref, Schema } from "effect"
import { ageDecryptorLayer } from "../adapters/age.ts"
import { fileBlobSinkLayer } from "../adapters/blob-sink.ts"
import { fileInboxCheckpointLayer } from "../adapters/inbox-checkpoint.ts"
import { fileInboxQuarantineLayer } from "../adapters/inbox-quarantine.ts"
import { fileLedgerStoreLayer } from "../adapters/ledger-file.ts"
import { directoryLayer } from "../adapters/inbox-directory.ts"
import type { InboxConfig, PollResult } from "../channels/inbox/reader.ts"
import { InboxReader, inboxReaderLayer } from "../channels/inbox/reader.ts"
import type { InboxSnapshot } from "../channels/inbox/reader.ts"
import type { FreshnessReport, StalenessAlert } from "../channels/inbox/freshness.ts"
import {
  FreshnessWatcher,
  freshnessWatcherLayer,
  inboxFreshnessFeedLayer,
  StalenessAlertSink,
} from "../channels/inbox/freshness.ts"
import { BlobSink } from "../kernel/blob-sink.ts"
import { contentStoreLayer } from "../kernel/cas.ts"
import { Ledger, ledgerLayer } from "../kernel/ledger.ts"
import { readCaptures } from "../kernel/observation.ts"
import { OriginId } from "../channels/inbox/schema.ts"
import type { JourneyFixture, JourneySource } from "./schema.ts"

export interface ClockRef {
  now: number
}

export const makeClock = (reference: ClockRef): Clock.Clock => ({
  currentTimeMillisUnsafe: () => reference.now,
  currentTimeMillis: Effect.sync(() => reference.now),
  currentTimeNanosUnsafe: () => BigInt(reference.now) * 1_000_000n,
  currentTimeNanos: Effect.sync(() => BigInt(reference.now) * 1_000_000n),
  monotonicTimeNanosUnsafe: () => BigInt(reference.now) * 1_000_000n,
  monotonicTimeNanos: Effect.sync(() => BigInt(reference.now) * 1_000_000n),
  sleep: () => Effect.void,
})

export interface WorldObservation {
  readonly origin: string
  readonly resourceKey: string
  readonly body: string
}

export interface WorldLedger {
  readonly recordCount: number
  readonly captureCount: number
  readonly headSeq: number
  readonly headHash: string
}

export interface World {
  readonly poll: PollResult
  readonly state: InboxSnapshot
  readonly observations: ReadonlyArray<WorldObservation>
  readonly reportBefore: FreshnessReport
  readonly reportAfter: FreshnessReport
  readonly alerts: ReadonlyArray<StalenessAlert>
  readonly ledger: WorldLedger
}

const limits: InboxConfig["limits"] = {
  maxBatchBytes: 1 << 20,
  maxBatchesPerPoll: 20,
  maxEntriesPerBatch: 8,
  maxEntryBytes: 100_000,
  maxTotalBlobBytes: 500_000,
}

/**
 * Boots the real Inbox reader and freshness watcher over the sealed journey
 * batches on a virtual clock. Every journey step gets its own world so a step
 * cannot pass on state left by another.
 */
export const runWorld = async (options: {
  readonly fixture: JourneyFixture
  readonly source: JourneySource
  readonly fixtureDirectory: string
}): Promise<World> => {
  const directory = await Deno.makeTempDir({ prefix: "kanbei-journey-v2-" })
  const batches = options.source.batches.filter((batch) =>
    batch.caseId !== options.fixture.ingest.replayBatch
  )
  const replayed = options.source.batches.find((batch) =>
    batch.caseId === options.fixture.ingest.replayBatch
  )
  const copied = replayed === undefined ? batches : [...batches, replayed]
  try {
    await Promise.all(
      copied.map((batch) =>
        Deno.copyFile(
          `${options.fixtureDirectory}/${batch.file}`,
          `${directory}/${batch.caseId}.age`,
        )
      ),
    )

    const clock: ClockRef = { now: Date.parse(options.fixture.clock.start) }
    const identityPath = `${options.fixtureDirectory}/keys/identity.txt`
    const allowlist = Schema.decodeUnknownSync(Schema.Array(OriginId))([
      ...options.fixture.sources.lifeAdmin,
      ...options.fixture.sources.material,
    ])
    const staleOrigin = Schema.decodeUnknownSync(OriginId)(options.fixture.sources.staleOrigin)
    const unknownOrigin = Schema.decodeUnknownSync(OriginId)(options.fixture.sources.unknownOrigin)
    const knownOrigins = [...allowlist, unknownOrigin]
    const config: InboxConfig = {
      identityPath,
      allowedOrigins: allowlist,
      limits,
      freshnessWindowMs: options.fixture.clock.freshnessWindowMs,
    }

    const program = Effect.gen(function* () {
      const alerts = yield* Ref.make<ReadonlyArray<StalenessAlert>>([])
      const sink = Layer.succeed(StalenessAlertSink)({
        emit: (alert) => Ref.update(alerts, (current) => [...current, alert]),
      })
      const blobs = fileBlobSinkLayer(`${directory}/cas`)
      const cas = contentStoreLayer.pipe(Layer.provide(blobs))
      const ledger = ledgerLayer("journey-v2").pipe(
        Layer.provide(fileLedgerStoreLayer(`${directory}/ledger.jsonl`)),
      )
      const checkpoints = fileInboxCheckpointLayer(`${directory}/checkpoint.json`)
      const quarantine = fileInboxQuarantineLayer(`${directory}/quarantine.json`)
      const reader = inboxReaderLayer(config).pipe(
        Layer.provide(directoryLayer(directory)),
        Layer.provide(ageDecryptorLayer({ identityPath })),
        Layer.provide(checkpoints),
        Layer.provide(quarantine),
        Layer.provide(cas),
        Layer.provide(ledger),
      )
      const feed = inboxFreshnessFeedLayer.pipe(Layer.provide(reader))
      const watcher = freshnessWatcherLayer({
        windows: { [staleOrigin]: options.fixture.clock.staleWindowMs },
        defaultWindowMs: options.fixture.clock.freshnessWindowMs,
        knownOrigins,
      }).pipe(Layer.provide(feed), Layer.provide(sink))
      const layers = Layer.mergeAll(
        reader,
        watcher,
        sink,
        blobs,
        cas,
        ledger,
        checkpoints,
        quarantine,
      )

      return yield* Effect.gen(function* () {
        const inbox = yield* InboxReader
        const freshness = yield* FreshnessWatcher
        const blobSink = yield* BlobSink
        const poll = yield* inbox.poll
        const state = yield* inbox.state
        const journal = yield* Ledger
        const captures = yield* readCaptures()
        const head = yield* journal.head
        const reportBefore = yield* freshness.report
        const observations = yield* Effect.forEach(
          state.observations,
          (observation) =>
            Effect.gen(function* () {
              const stored = yield* blobSink.read(observation.digest)
              return {
                origin: observation.origin,
                resourceKey: observation.resourceKey,
                body: Option.isSome(stored) ? new TextDecoder().decode(stored.value) : "",
              }
            }),
          { concurrency: 1 },
        )
        yield* Effect.sync(() => {
          clock.now += options.fixture.clock.staleAdvanceMs
        })
        const reportAfter = yield* freshness.probe
        return {
          poll,
          state,
          observations,
          reportBefore,
          reportAfter,
          alerts: yield* freshness.alerts,
          ledger: {
            recordCount: (yield* journal.records).length,
            captureCount: captures.length,
            headSeq: head.seq,
            headHash: head.hash,
          },
        }
      }).pipe(
        Effect.provide(layers),
        Effect.provideService(Clock.Clock, makeClock(clock)),
      )
    })

    return await Effect.runPromise(program)
  } finally {
    await Deno.remove(directory, { recursive: true })
  }
}

/** Freshness states keyed by origin, mapped onto the advice vocabulary. */
export const sourceStates = (report: FreshnessReport): ReadonlyMap<string, string> =>
  new Map(
    report.sources.map((source) => [
      source.origin,
      source.state === "fresh" || source.state === "empty" ? "current" : source.state,
    ]),
  )
