import { Schema } from "effect"
import { OriginId, ScanCoverage } from "./schema.ts"

/**
 * The durable inbox state shapes. The reader keeps these in memory for fast
 * reads, but the coverage receipts, batch heartbeats, host heartbeats, gaps, and
 * captures are appended to the canonical ledger, so a restart rebuilds them
 * instead of starting empty.
 */
export const ObservationSchema = Schema.Struct({
  origin: OriginId,
  sequence: Schema.Number,
  batchDigest: Schema.String,
  resourceKey: Schema.String,
  kind: Schema.String,
  locator: Schema.String,
  capturedAt: Schema.String,
  digest: Schema.String,
  bytes: Schema.Number,
  mediaType: Schema.optional(Schema.String),
})
export type Observation = typeof ObservationSchema.Type

export const CoverageReceiptSchema = Schema.Struct({
  origin: OriginId,
  batchDigest: Schema.String,
  sequence: Schema.Number,
  receivedAt: Schema.String,
  entries: Schema.Number,
  publishedBytes: Schema.Number,
  coverage: ScanCoverage,
})
export type CoverageReceipt = typeof CoverageReceiptSchema.Type

export const GapRecordSchema = Schema.Struct({
  origin: OriginId,
  kind: Schema.Literals(["gap", "out_of_order", "suppression"]),
  missing: Schema.Number,
  sequence: Schema.Number,
  detail: Schema.String,
})
export type GapRecord = typeof GapRecordSchema.Type

/**
 * A heartbeat is either the reader's per-batch record (`batch`) or a host
 * freshness envelope (`host`). The host carries its own window and staleness
 * verdict; the reader never turns either into a silent default.
 */
export const HeartbeatEntrySchema = Schema.Struct({
  origin: OriginId,
  kind: Schema.Literals(["batch", "host"]),
  sequence: Schema.Number,
  /** Producer time: the batch `producedAt`, or the host's `last_ok`. */
  producedAt: Schema.String,
  /** The host envelope's `generated_at`; unused for batch heartbeats. */
  generatedAt: Schema.optional(Schema.String),
  receivedAt: Schema.String,
  coverage: ScanCoverage,
  /** The host's per-source window, when the host stated one. */
  windowMs: Schema.optional(Schema.Number),
  /** The host's own staleness verdict at generation time. */
  hostStale: Schema.optional(Schema.Boolean),
  /** The host's last successful batch time, when it reported one. */
  lastOk: Schema.optional(Schema.String),
})
export type HeartbeatEntry = typeof HeartbeatEntrySchema.Type

export const SourceHeartbeatSchema = Schema.Struct({
  origin: OriginId,
  sequence: Schema.Number,
  producedAt: Schema.String,
  receivedAt: Schema.String,
  coverage: ScanCoverage,
  state: Schema.Literals(["current", "stale", "unknown"]),
  kind: Schema.optional(Schema.Literals(["batch", "host"])),
  generatedAt: Schema.optional(Schema.String),
  windowMs: Schema.optional(Schema.Number),
  hostStale: Schema.optional(Schema.Boolean),
  lastOk: Schema.optional(Schema.String),
})
export type SourceHeartbeat = typeof SourceHeartbeatSchema.Type

/**
 * The single freshness derivation: producer time against the source window. A
 * host verdict of stale is final; an unparseable producer time is `unknown`,
 * never `current` (ZT-18).
 */
export const heartbeatState = (
  entry: {
    readonly producedAt: string
    readonly generatedAt?: string | undefined
    readonly windowMs?: number | undefined
    readonly hostStale?: boolean | undefined
  },
  now: number,
  defaultWindowMs: number,
): "current" | "stale" | "unknown" => {
  const produced = Date.parse(entry.producedAt)
  // A source with no producer time has no freshness basis, and a garbage one
  // cannot be compared: both are unknown, never current.
  if (!Number.isFinite(produced)) return "unknown"
  if (entry.hostStale === true) return "stale"
  const window = entry.windowMs ?? defaultWindowMs
  if (now - produced > window) return "stale"
  // A host heartbeat older than its own window is stale even if its verdict was
  // current when it was written.
  const generated = entry.generatedAt === undefined ? undefined : Date.parse(entry.generatedAt)
  if (generated !== undefined && Number.isFinite(generated) && now - generated > window) {
    return "stale"
  }
  return "current"
}
