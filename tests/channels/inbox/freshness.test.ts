import { assertEquals } from "@std/assert"
import { Clock, Effect, Layer, Option, Ref, Schema } from "effect"
import type {
  FreshnessConfig,
  FreshnessReport,
  StalenessAlert,
} from "../../../src/channels/inbox/freshness.ts"
import {
  FreshnessFeed,
  FreshnessWatcher,
  freshnessWatcherLayer,
  inboxFreshnessFeedLayer,
  StalenessAlertSink,
  statusSources,
} from "../../../src/channels/inbox/freshness.ts"
import type {
  CoverageReceipt,
  InboxReader,
  InboxSnapshot,
  SourceHeartbeat,
} from "../../../src/channels/inbox/reader.ts"
import { InboxReader as InboxReaderTag } from "../../../src/channels/inbox/reader.ts"

const NOW = Date.parse("2026-11-11T00:00:00.000Z")
const SECOND = 1_000
const MINUTE = 60 * SECOND
const DAY = 24 * 60 * 60 * 1000

export const clockAt = (millis: number): Clock.Clock => ({
  currentTimeMillisUnsafe: () => millis,
  currentTimeMillis: Effect.succeed(millis),
  currentTimeNanosUnsafe: () => BigInt(millis) * 1_000_000n,
  currentTimeNanos: Effect.succeed(BigInt(millis) * 1_000_000n),
  monotonicTimeNanosUnsafe: () => 0n,
  monotonicTimeNanos: Effect.succeed(0n),
  sleep: () => Effect.void,
})

const digest = "sha256:" + "a".repeat(64)

const coverage = { complete: true, gaps: 0, suppressed: 0 }

const heartbeat = (
  origin: SourceHeartbeat["origin"],
  sequence: number,
  receivedAt: number,
  receivedCoverage = coverage,
): SourceHeartbeat => ({
  origin,
  sequence,
  // The fixture heartbeat is produced when it is delivered; the producer clock
  // is the freshness basis.
  producedAt: new Date(receivedAt).toISOString(),
  receivedAt: new Date(receivedAt).toISOString(),
  coverage: receivedCoverage,
  state: "current",
})

const receipt = (
  origin: CoverageReceipt["origin"],
  sequence: number,
  entries: number,
): CoverageReceipt => ({
  origin,
  batchDigest: digest,
  sequence,
  receivedAt: new Date(NOW).toISOString(),
  entries,
  publishedBytes: entries * 10,
  coverage,
})

interface Harness {
  readonly feed: Ref.Ref<{
    heartbeats: ReadonlyArray<SourceHeartbeat>
    receipts: ReadonlyArray<CoverageReceipt>
  }>
  readonly alerts: Ref.Ref<ReadonlyArray<StalenessAlert>>
  readonly layer: Layer.Layer<FreshnessWatcher>
}

const harness = (
  config: FreshnessConfig,
  snapshot: {
    heartbeats: ReadonlyArray<SourceHeartbeat>
    receipts: ReadonlyArray<CoverageReceipt>
  },
): Effect.Effect<Harness> =>
  Effect.gen(function* () {
    const feed = yield* Ref.make(snapshot)
    const alerts = yield* Ref.make<ReadonlyArray<StalenessAlert>>([])
    const feedLayer = Layer.succeed(FreshnessFeed)({ snapshot: Ref.get(feed) })
    const sinkLayer = Layer.succeed(StalenessAlertSink)({
      emit: (alert) => Ref.update(alerts, (current) => [...current, alert]),
    })
    return {
      feed,
      alerts,
      layer: freshnessWatcherLayer(config).pipe(
        Layer.provide(Layer.mergeAll(feedLayer, sinkLayer)),
      ),
    }
  })

const config = (overrides: Partial<FreshnessConfig> = {}): FreshnessConfig => ({
  windows: {},
  defaultWindowMs: MINUTE,
  knownOrigins: ["tuta-mail"],
  ...overrides,
})

const onWatcher = <A>(
  h: Harness,
  now: number,
  effect: Effect.Effect<A, never, FreshnessWatcher>,
): Promise<A> =>
  Effect.runPromise(
    effect.pipe(
      Effect.provide(h.layer),
      Effect.provideService(Clock.Clock, clockAt(now)),
      Effect.orDie,
    ),
  )

const report = (h: Harness, now = NOW): Promise<FreshnessReport> =>
  onWatcher(
    h,
    now,
    Effect.gen(function* () {
      const watcher = yield* FreshnessWatcher
      return yield* watcher.report
    }),
  )

const probe = (h: Harness, now = NOW): Promise<FreshnessReport> =>
  onWatcher(
    h,
    now,
    Effect.gen(function* () {
      const watcher = yield* FreshnessWatcher
      return yield* watcher.probe
    }),
  )

const alertsOf = (h: Harness) => Effect.runPromise(Ref.get(h.alerts))

Deno.test("a source exactly at its window is fresh and one millisecond past is stale", async () => {
  const atBoundary = await Effect.runPromise(
    harness(config({ defaultWindowMs: MINUTE }), {
      heartbeats: [heartbeat("tuta-mail", 4, NOW - MINUTE)],
      receipts: [receipt("tuta-mail", 4, 2)],
    }),
  )
  assertEquals((await report(atBoundary)).sources[0]?.state, "fresh")

  const past = await Effect.runPromise(
    harness(config({ defaultWindowMs: MINUTE - 1 }), {
      heartbeats: [heartbeat("tuta-mail", 4, NOW - MINUTE)],
      receipts: [receipt("tuta-mail", 4, 2)],
    }),
  )
  assertEquals((await report(past)).sources[0]?.state, "stale")
})

Deno.test("a source with no heartbeat is unknown, not current, and alerts as missing", async () => {
  const h = await Effect.runPromise(harness(config(), { heartbeats: [], receipts: [] }))
  const outcome = await onWatcher(
    h,
    NOW,
    Effect.gen(function* () {
      const watcher = yield* FreshnessWatcher
      const freshness = yield* watcher.report
      yield* watcher.probe
      yield* watcher.probe
      return { freshness, alerts: yield* watcher.alerts }
    }),
  )
  assertEquals(outcome.freshness.sources.map((source) => source.state), ["unknown"])
  assertEquals(Option.isNone(outcome.freshness.sources[0]?.lastSequence ?? Option.none()), true)
  assertEquals(outcome.alerts.length, 1)
  assertEquals(outcome.alerts[0]?.kind, "missing")
  assertEquals(outcome.alerts[0]?.windowMs, MINUTE)
})

Deno.test("a stale source reports its last sequence and alerts once per transition", async () => {
  const h = await Effect.runPromise(
    harness(config(), {
      heartbeats: [heartbeat("tuta-mail", 7, NOW - 2 * MINUTE)],
      receipts: [receipt("tuta-mail", 7, 3)],
    }),
  )
  const outcome = await onWatcher(
    h,
    NOW,
    Effect.gen(function* () {
      const watcher = yield* FreshnessWatcher
      const stale = yield* watcher.report
      yield* watcher.probe
      yield* watcher.probe
      yield* Ref.set(h.feed, {
        heartbeats: [heartbeat("tuta-mail", 8, NOW)],
        receipts: [receipt("tuta-mail", 8, 1)],
      })
      yield* watcher.probe
      const fresh = yield* watcher.report
      yield* Ref.set(h.feed, {
        heartbeats: [heartbeat("tuta-mail", 8, NOW - 2 * MINUTE)],
        receipts: [receipt("tuta-mail", 8, 1)],
      })
      yield* watcher.probe
      return { stale, fresh, alerts: yield* watcher.alerts }
    }),
  )
  assertEquals(outcome.stale.sources[0]?.state, "stale")
  assertEquals(Option.getOrThrow(outcome.stale.sources[0]?.lastSequence ?? Option.none()), 7)
  assertEquals(outcome.fresh.sources[0]?.state, "fresh")
  assertEquals(outcome.alerts.map((alert) => alert.kind), ["expired", "expired"])
})
Deno.test("out-of-order batches never regress the sequence watermark", async () => {
  const h = await Effect.runPromise(
    harness(config(), {
      heartbeats: [
        heartbeat("tuta-mail", 9, NOW),
        heartbeat("tuta-mail", 4, NOW - 10 * MINUTE),
      ],
      receipts: [receipt("tuta-mail", 9, 2), receipt("tuta-mail", 4, 1)],
    }),
  )
  const freshness = await report(h)
  assertEquals(freshness.sources[0]?.state, "fresh")
  assertEquals(Option.getOrThrow(freshness.sources[0]?.lastSequence ?? Option.none()), 9)
})

Deno.test("a current empty scan is empty and stays current, not stale", async () => {
  const h = await Effect.runPromise(
    harness(config(), {
      heartbeats: [heartbeat("tuta-mail", 5, NOW)],
      receipts: [receipt("tuta-mail", 5, 0)],
    }),
  )
  const freshness = await report(h)
  assertEquals(freshness.sources[0]?.state, "empty")
  const current = await onWatcher(
    h,
    NOW,
    Effect.gen(function* () {
      const watcher = yield* FreshnessWatcher
      return yield* watcher.currentFor("tuta-mail")
    }),
  )
  assertEquals(Option.isSome(current), true)
  await probe(h)
  assertEquals((await alertsOf(h)).length, 0)
})

Deno.test("produced time and coverage ride with the freshness report", async () => {
  const h = await Effect.runPromise(
    harness(config(), {
      heartbeats: [heartbeat("tuta-mail", 3, NOW, { complete: false, gaps: 2, suppressed: 1 })],
      receipts: [receipt("tuta-mail", 3, 1)],
    }),
  )
  const source = (await report(h)).sources[0]
  assertEquals(Option.isSome(source?.producedAt ?? Option.none()), true)
  assertEquals(Option.getOrThrow(source?.coverage ?? Option.none()), {
    complete: false,
    gaps: 2,
    suppressed: 1,
  })
  assertEquals(Option.getOrThrow(source?.ageMs ?? Option.none()), 0)
})

Deno.test("a stale or unknown source cannot pass the projection's current check", async () => {
  const h = await Effect.runPromise(
    harness(config({ knownOrigins: ["tuta-mail", "finance"] }), {
      heartbeats: [heartbeat("tuta-mail", 1, NOW - DAY)],
      receipts: [receipt("tuta-mail", 1, 1)],
    }),
  )
  const current = await onWatcher(
    h,
    NOW,
    Effect.gen(function* () {
      const watcher = yield* FreshnessWatcher
      const stale = yield* watcher.currentFor("tuta-mail")
      const unknown = yield* watcher.currentFor("finance")
      return { stale, unknown }
    }),
  )
  assertEquals(Option.isNone(current.stale), true)
  assertEquals(Option.isNone(current.unknown), true)
  const sources = (await report(h)).sources
  assertEquals(
    sources.filter((source) => source.state !== "stale" && source.state !== "unknown"),
    [],
  )
})

Deno.test("the inbox feed adapter reads heartbeats and receipts from the reader state", async () => {
  const snapshot: InboxSnapshot = {
    observations: [],
    receipts: [receipt("tuta-mail", 2, 4)],
    heartbeats: [heartbeat("tuta-mail", 2, NOW)],
    gaps: [],
    quarantined: [],
    securityEvents: [],
    duplicates: 0,
  }
  const reader: InboxReader = {
    poll: Effect.succeed({ processed: 0, accepted: 0, quarantined: 0, duplicates: 0 }),
    state: Effect.succeed(snapshot),
    sourceState: () => Effect.succeed("current"),
    heartbeat: () => Effect.succeed(Option.none()),
  }
  const feedLayer = inboxFreshnessFeedLayer.pipe(
    Layer.provide(Layer.succeed(InboxReaderTag)(reader)),
  )
  const captured = await Effect.runPromise(
    Effect.gen(function* () {
      const feed = yield* FreshnessFeed
      return yield* feed.snapshot
    }).pipe(Effect.provide(feedLayer), Effect.orDie),
  )
  assertEquals(captured.heartbeats.map((entry) => entry.sequence), [2])
  assertEquals(captured.receipts.map((entry) => entry.entries), [4])
})

Deno.test("the status rows label stale and unknown sources, never as fresh", async () => {
  const h = await Effect.runPromise(
    harness(config({ knownOrigins: ["tuta-mail", "finance"] }), {
      heartbeats: [heartbeat("tuta-mail", 2, NOW - DAY)],
      receipts: [receipt("tuta-mail", 2, 1)],
    }),
  )
  const rows = statusSources(await report(h))
  assertEquals(rows, [
    { origin: "finance", state: "unknown" },
    { origin: "tuta-mail", state: "stale", lastSequence: 2 },
  ])
})

Deno.test("the freshness fixture reports its one-window-old source as stale", async () => {
  const Fixture = Schema.Struct({
    now: Schema.String,
    windowMs: Schema.Number,
    knownOrigins: Schema.Array(Schema.Literals([
      "tuta-mail",
      "tuta-calendar",
      "finance",
      "brave",
      "immich",
      "phone",
      "cluster-digest",
      "git",
    ])),
    heartbeats: Schema.Array(
      Schema.Struct({
        origin: Schema.Literals([
          "tuta-mail",
          "tuta-calendar",
          "finance",
          "brave",
          "immich",
          "phone",
          "cluster-digest",
          "git",
        ]),
        sequence: Schema.Number,
        producedAt: Schema.String,
        receivedAt: Schema.String,
        coverage: Schema.Struct({
          complete: Schema.Boolean,
          gaps: Schema.Number,
          suppressed: Schema.Number,
        }),
        state: Schema.Literals(["current", "stale", "unknown"]),
      }),
    ),
    receipts: Schema.Array(
      Schema.Struct({
        origin: Schema.Literals([
          "tuta-mail",
          "tuta-calendar",
          "finance",
          "brave",
          "immich",
          "phone",
          "cluster-digest",
          "git",
        ]),
        batchDigest: Schema.String,
        sequence: Schema.Number,
        receivedAt: Schema.String,
        entries: Schema.Number,
        publishedBytes: Schema.Number,
        coverage: Schema.Struct({
          complete: Schema.Boolean,
          gaps: Schema.Number,
          suppressed: Schema.Number,
        }),
      }),
    ),
  })
  const fixture = Schema.decodeUnknownSync(Fixture)(
    JSON.parse(Deno.readTextFileSync("fixtures/freshness-v1/heartbeats.json")),
  )
  const h = await Effect.runPromise(
    harness(
      config({ knownOrigins: fixture.knownOrigins, defaultWindowMs: fixture.windowMs }),
      { heartbeats: fixture.heartbeats, receipts: fixture.receipts },
    ),
  )
  const freshness = await report(h, Date.parse(fixture.now))
  const stale = freshness.sources.find((source) => source.origin === "tuta-mail")
  assertEquals(stale?.state, "stale")
  assertEquals(Option.getOrThrow(stale?.lastSequence ?? Option.none()), 12)
})

Deno.test("an unparseable producer time is unknown, not current, and alerts", async () => {
  const h = await Effect.runPromise(
    harness(config(), {
      heartbeats: [{ ...heartbeat("tuta-mail", 4, NOW), producedAt: "not-a-timestamp" }],
      receipts: [receipt("tuta-mail", 4, 3)],
    }),
  )
  const outcome = await onWatcher(
    h,
    NOW,
    Effect.gen(function* () {
      const watcher = yield* FreshnessWatcher
      const freshness = yield* watcher.probe
      return { freshness, alerts: yield* watcher.alerts }
    }),
  )
  const source = outcome.freshness.sources[0]
  assertEquals(source?.state, "unknown")
  assertEquals(Option.isNone(source?.ageMs ?? Option.none()), true)
  assertEquals(outcome.alerts.map((alert) => alert.kind), ["missing"])
})
