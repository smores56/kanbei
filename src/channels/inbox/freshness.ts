import { Clock, Context, Data, Effect, Layer, Option, Ref } from "effect"
import type { CoverageReceipt, InboxReader, SourceHeartbeat } from "./reader.ts"
import { InboxReader as InboxReaderTag } from "./reader.ts"
import type { OriginId } from "./schema.ts"
import { heartbeatState } from "./state.ts"

export interface FreshnessConfig {
  /** Per-origin operator windows; an unlisted origin uses `defaultWindowMs`. */
  readonly windows: Readonly<Partial<Record<OriginId, number>>>
  readonly defaultWindowMs: number
  readonly knownOrigins: ReadonlyArray<OriginId>
}

export type FreshnessState = "fresh" | "empty" | "stale" | "unknown"

export interface SourceFreshness {
  readonly origin: OriginId
  readonly state: FreshnessState
  readonly windowMs: number
  readonly ageMs: Option.Option<number>
  readonly lastSequence: Option.Option<number>
  readonly producedAt: Option.Option<string>
  readonly receivedAt: Option.Option<string>
  readonly coverage: Option.Option<SourceHeartbeat["coverage"]>
}

export interface FreshnessReport {
  readonly sources: ReadonlyArray<SourceFreshness>
}

export interface StalenessAlert {
  readonly origin: OriginId
  readonly kind: "expired" | "missing"
  readonly lastSequence: Option.Option<number>
  readonly windowMs: number
  readonly at: string
}

export class StalenessAlertSinkError extends Data.TaggedError("StalenessAlertSinkError")<{
  readonly reason: string
}> {}

export interface StalenessAlertSink {
  readonly emit: (alert: StalenessAlert) => Effect.Effect<void, StalenessAlertSinkError>
}

export const StalenessAlertSink = Context.Service<StalenessAlertSink>(
  "kanbei/channel/inbox/StalenessAlertSink",
)

/** A batch-derived view: the reader's heartbeats plus the entry counts it accepted. */
export interface FreshnessFeed {
  readonly snapshot: Effect.Effect<{
    readonly heartbeats: ReadonlyArray<SourceHeartbeat>
    readonly receipts: ReadonlyArray<CoverageReceipt>
  }>
}

export const FreshnessFeed = Context.Service<FreshnessFeed>("kanbei/channel/inbox/FreshnessFeed")

export const inboxFreshnessFeedLayer: Layer.Layer<FreshnessFeed, never, InboxReader> = Layer.effect(
  FreshnessFeed,
  Effect.gen(function* () {
    const reader = yield* InboxReaderTag
    return {
      snapshot: reader.state.pipe(
        Effect.map((state) => ({ heartbeats: state.heartbeats, receipts: state.receipts })),
      ),
    }
  }),
)

export interface FreshnessWatcher {
  readonly report: Effect.Effect<FreshnessReport>
  readonly probe: Effect.Effect<FreshnessReport>
  readonly currentFor: (
    origin: OriginId,
  ) => Effect.Effect<Option.Option<SourceFreshness>>
  readonly alerts: Effect.Effect<ReadonlyArray<StalenessAlert>>
}

export const FreshnessWatcher = Context.Service<FreshnessWatcher>(
  "kanbei/channel/inbox/FreshnessWatcher",
)

/** A stale or unknown source never counts as current (ZT-18). */
export const isCurrent = (state: FreshnessState): boolean => state === "fresh" || state === "empty"

export const currentSources = (report: FreshnessReport): ReadonlyArray<SourceFreshness> =>
  report.sources.filter((source) => isCurrent(source.state))

const configIssues = (config: FreshnessConfig): ReadonlyArray<string> => {
  const issues: Array<string> = []
  if (!Number.isFinite(config.defaultWindowMs) || config.defaultWindowMs <= 0) {
    issues.push("defaultWindowMs must be positive")
  }
  for (const [origin, window] of Object.entries(config.windows)) {
    if (window === undefined || !Number.isFinite(window) || window <= 0) {
      issues.push(`window for ${origin} must be positive`)
    }
  }
  if (config.knownOrigins.length === 0) issues.push("knownOrigins must not be empty")
  return issues
}

const windowFor = (config: FreshnessConfig, origin: OriginId): number =>
  config.windows[origin] ?? config.defaultWindowMs

interface LatestHeartbeat {
  readonly heartbeat: SourceHeartbeat
}

const latestPerOrigin = (
  heartbeats: ReadonlyArray<SourceHeartbeat>,
): ReadonlyMap<OriginId, LatestHeartbeat> => {
  const latest = new Map<OriginId, LatestHeartbeat>()
  for (const heartbeat of heartbeats) {
    const existing = latest.get(heartbeat.origin)
    // Out-of-order batches never regress the freshness watermark.
    if (existing === undefined || heartbeat.sequence > existing.heartbeat.sequence) {
      latest.set(heartbeat.origin, { heartbeat })
    }
  }
  return latest
}

const entriesByOrigin = (
  receipts: ReadonlyArray<CoverageReceipt>,
): ReadonlyMap<OriginId, number> => {
  const latest = new Map<OriginId, CoverageReceipt>()
  for (const receipt of receipts) {
    const existing = latest.get(receipt.origin)
    if (existing === undefined || receipt.sequence >= existing.sequence) {
      latest.set(receipt.origin, receipt)
    }
  }
  return new Map([...latest].map(([origin, receipt]) => [origin, receipt.entries]))
}

const buildSource = (
  config: FreshnessConfig,
  origin: OriginId,
  heartbeat: SourceHeartbeat | undefined,
  entries: number | undefined,
  now: number,
): SourceFreshness => {
  if (heartbeat === undefined) {
    return {
      origin,
      state: "unknown",
      windowMs: windowFor(config, origin),
      ageMs: Option.none(),
      lastSequence: Option.none(),
      producedAt: Option.none(),
      receivedAt: Option.none(),
      coverage: Option.none(),
    }
  }
  const windowMs = windowFor(config, origin)
  // Producer time is the freshness watermark. A host heartbeat states its own
  // verdict; an unparseable time is unknown instead of falling back to delivery
  // time, which would present a broken producer as current.
  const verdict = heartbeatState(heartbeat, now, windowMs)
  const basisText = heartbeat.kind === "host"
    ? heartbeat.lastOk ?? heartbeat.producedAt
    : heartbeat.producedAt
  const basis = Date.parse(basisText)
  const age = verdict === "unknown" || !Number.isFinite(basis)
    ? undefined
    : Math.max(0, now - basis)
  const state: FreshnessState = verdict === "unknown"
    ? "unknown"
    : verdict === "stale"
    ? "stale"
    : entries === 0
    ? "empty"
    : "fresh"
  return {
    origin,
    state,
    windowMs,
    ageMs: age === undefined ? Option.none() : Option.some(age),
    lastSequence: Option.some(heartbeat.sequence),
    producedAt: Option.some(heartbeat.producedAt),
    receivedAt: Option.some(heartbeat.receivedAt),
    coverage: Option.some(heartbeat.coverage),
  }
}

const buildReport = (
  config: FreshnessConfig,
  snapshot: {
    readonly heartbeats: ReadonlyArray<SourceHeartbeat>
    readonly receipts: ReadonlyArray<CoverageReceipt>
  },
  now: number,
): FreshnessReport => {
  const latest = latestPerOrigin(snapshot.heartbeats)
  const entries = entriesByOrigin(snapshot.receipts)
  const origins = new Set<OriginId>([
    ...config.knownOrigins,
    ...latest.keys(),
    ...snapshot.receipts.map((receipt) => receipt.origin),
  ])
  const sources = [...origins].toSorted().map((origin) =>
    buildSource(config, origin, latest.get(origin)?.heartbeat, entries.get(origin), now)
  )
  return { sources }
}

const alertOf = (source: SourceFreshness, at: number): StalenessAlert | undefined => {
  if (source.state === "stale") {
    return {
      origin: source.origin,
      kind: "expired",
      lastSequence: source.lastSequence,
      windowMs: source.windowMs,
      at: new Date(at).toISOString(),
    }
  }
  if (source.state === "unknown") {
    return {
      origin: source.origin,
      kind: "missing",
      lastSequence: Option.none(),
      windowMs: source.windowMs,
      at: new Date(at).toISOString(),
    }
  }
  return undefined
}

export const freshnessWatcherLayer = (
  config: FreshnessConfig,
): Layer.Layer<FreshnessWatcher, never, FreshnessFeed | StalenessAlertSink> =>
  Layer.effect(
    FreshnessWatcher,
    Effect.gen(function* () {
      const issues = configIssues(config)
      if (issues.length > 0) {
        return yield* Effect.die(new Error(`invalid freshness config: ${issues.join("; ")}`))
      }
      const feed = yield* FreshnessFeed
      const sink = yield* StalenessAlertSink
      const alerted = yield* Ref.make<ReadonlySet<OriginId>>(new Set())
      const history = yield* Ref.make<ReadonlyArray<StalenessAlert>>([])

      const read = (): Effect.Effect<FreshnessReport> =>
        Effect.gen(function* () {
          const now = yield* Clock.currentTimeMillis
          return buildReport(config, yield* feed.snapshot, now)
        })

      const probe: Effect.Effect<FreshnessReport> = Effect.gen(function* () {
        const now = yield* Clock.currentTimeMillis
        const report = buildReport(config, yield* feed.snapshot, now)
        const active = yield* Ref.get(alerted)
        const next = new Set<OriginId>()
        const fired: Array<StalenessAlert> = []
        for (const source of report.sources) {
          const alert = alertOf(source, now)
          if (alert === undefined) continue
          next.add(source.origin)
          if (!active.has(source.origin)) fired.push(alert)
        }
        yield* Ref.set(alerted, next)
        if (fired.length > 0) {
          yield* Ref.update(history, (current) => [...current, ...fired])
          yield* Effect.forEach(fired, (alert) => Effect.orDie(sink.emit(alert)), {
            concurrency: 1,
          })
        }
        return report
      })

      return {
        report: read(),
        probe,
        currentFor: (origin) =>
          read().pipe(
            Effect.map((report) =>
              Option.fromUndefinedOr(
                report.sources.find((source) =>
                  source.origin === origin && isCurrent(source.state)
                ),
              )
            ),
          ),
        alerts: Ref.get(history),
      }
    }),
  )

export const makeFreshnessConfig = (
  overrides: Partial<FreshnessConfig> = {},
): FreshnessConfig => ({
  windows: {},
  defaultWindowMs: 24 * 60 * 60 * 1000,
  knownOrigins: ["tuta-mail"],
  ...overrides,
})

export type { FreshnessConfig as Config }

export interface FreshnessStatusSource {
  readonly origin: OriginId
  readonly state: FreshnessState
  readonly lastSequence?: number
}

/** The status route's source rows: stale and unknown are labelled, never dropped. */
export const statusSources = (
  report: FreshnessReport,
): ReadonlyArray<FreshnessStatusSource> =>
  report.sources.map((source) =>
    Option.isSome(source.lastSequence)
      ? { origin: source.origin, state: source.state, lastSequence: source.lastSequence.value }
      : { origin: source.origin, state: source.state }
  )
