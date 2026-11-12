import { Effect, Schema } from "effect"
import { canonicalize } from "../../evidence/canonical.ts"
import { sha256, sha256Text } from "../../evidence/digest.ts"
import type { IngestBatchEnvelope, IngestEntry } from "./schema.ts"

/**
 * The host writer's envelope (`nix-config modules/lib/ingest/policy.py`):
 * `{v, source, kind, seq, sent_at, items}`. Both producers and this reader use
 * `WIRE_VERSION`; the 1 MiB frame cap is the host's `MAX_BATCH_BYTES`.
 */
export const WIRE_VERSION = 1
export const MAX_BATCH_BYTES = 1 << 20
/** The host writer caps one envelope at 1000 items; refusing below that is a lie. */
export const MAX_WIRE_ITEMS = 1000
/** Reader admission: one source may consume at most this many files per poll. */
export const MAX_WIRE_ITEMS_PER_SOURCE = 50

export const WireKind = Schema.Literals(["batch", "alert"])
export type WireKind = typeof WireKind.Type

const strict = { onExcessProperty: "error" } as const

export const HostBatchEnvelope = Schema.Struct({
  v: Schema.Literal(WIRE_VERSION),
  source: Schema.String,
  kind: WireKind,
  /** The host accepts `seq >= 0`; `0` is a legal first sequence. */
  seq: Schema.optional(Schema.Number.pipe(Schema.check(Schema.isGreaterThanOrEqualTo(0)))),
  sent_at: Schema.optional(Schema.String),
  items: Schema.Array(Schema.Json),
})
export type HostBatchEnvelope = typeof HostBatchEnvelope.Type

export const parseHostEnvelope = (value: unknown): HostBatchEnvelope => {
  const envelope = Schema.decodeUnknownSync(HostBatchEnvelope, strict)(value)
  // The host's own writer cap; a larger envelope is not something it produces.
  if (envelope.items.length > MAX_WIRE_ITEMS) {
    throw new Error(`envelope carries ${envelope.items.length} items > ${MAX_WIRE_ITEMS}`)
  }
  return envelope
}

/** True when the document is the host wire envelope rather than the rich manifest. */
export const isHostEnvelope = (value: unknown): boolean =>
  typeof value === "object" && value !== null && "v" in value && "items" in value &&
  "source" in value

/**
 * The host heartbeat (`nix-config modules/lib/ingest/heartbeat.py`,
 * `policy.validate_heartbeat`): one envelope per 5-minute window carrying every
 * source's staleness verdict, window, and last successful batch time.
 */
export const HostHeartbeatSource = Schema.Struct({
  stale: Schema.Boolean,
  host: Schema.optional(Schema.Boolean),
  window_s: Schema.Number.pipe(Schema.check(Schema.isGreaterThanOrEqualTo(0))),
  last_ok: Schema.optional(Schema.NullOr(Schema.String)),
  age_s: Schema.optional(Schema.NullOr(Schema.Number)),
})
export type HostHeartbeatSource = typeof HostHeartbeatSource.Type

export const HostHeartbeatEnvelope = Schema.Struct({
  v: Schema.Literal(WIRE_VERSION),
  kind: Schema.Literal("heartbeat"),
  generated_at: Schema.String,
  sources: Schema.Record(Schema.String, HostHeartbeatSource),
  host_sources: Schema.Array(Schema.String),
})
export type HostHeartbeatEnvelope = typeof HostHeartbeatEnvelope.Type

export const parseHostHeartbeat = (value: unknown): HostHeartbeatEnvelope =>
  Schema.decodeUnknownSync(HostHeartbeatEnvelope, strict)(value)

/**
 * True when the document is a host heartbeat. A heartbeat shares `v` and `kind`
 * with the batch envelope, so the `sources` map is what distinguishes it.
 */
export const isHeartbeatEnvelope = (value: unknown): boolean =>
  typeof value === "object" && value !== null && "v" in value && "kind" in value &&
  value.kind === "heartbeat" && "sources" in value

const itemKey = (source: string, sequence: number, index: number): string =>
  `${source}/${sequence}/${index}`

const resourceKeyOf = (item: unknown, fallback: string): string => {
  if (typeof item === "object" && item !== null) {
    for (const key of ["resourceKey", "id", "key"]) {
      const candidate = (item as Record<string, unknown>)[key]
      if (typeof candidate === "string" && candidate.trim() !== "") return candidate
    }
  }
  return fallback
}

const encodeItem = (item: unknown): Uint8Array =>
  new TextEncoder().encode(canonicalize(Schema.decodeUnknownSync(Schema.Json)(item)))

/**
 * Convert one host envelope into the rich, hash-addressed batch the kernel
 * already validates. Each item becomes a canonical JSON object keyed by its own
 * digest, so the ledger stores the exact bytes the host sent.
 */
export const hostEnvelopeToBatch = (
  envelope: HostBatchEnvelope,
  name: string,
): Effect.Effect<IngestBatchEnvelope> =>
  Effect.gen(function* () {
    const sequence = envelope.seq ?? 1
    // A missing producer time is not "now": the reader derives `unknown`
    // freshness from an unparseable value rather than presenting the source current.
    const producedAt = envelope.sent_at ?? ""
    const envelopeDigest = yield* sha256Text(
      canonicalize(Schema.decodeUnknownSync(Schema.Json)(envelope)),
    ).pipe(Effect.orDie)
    const encoded = envelope.items.map((item) => encodeItem(item))
    const digests = yield* Effect.forEach(encoded, (bytes) => sha256(bytes), {
      concurrency: 1,
    }).pipe(Effect.orDie)
    const entries: Array<IngestEntry> = envelope.items.map((item, index) => {
      const digest = digests[index] ?? ""
      return {
        resourceKey: resourceKeyOf(item, itemKey(envelope.source, sequence, index)),
        kind: "capture",
        locator: `inbox:${name}#${index}`,
        capturedAt: producedAt,
        mediaType: "application/json",
        body: digest,
        evidence: { wire: "host-batch-v1", kind: envelope.kind },
      }
    })
    const manifestBody = {
      origin: envelope.source as IngestBatchEnvelope["manifest"]["origin"],
      sequence,
      producedAt,
      scan: {
        mode: "event" as const,
        scopeHash: envelopeDigest,
        startedAt: producedAt,
        completedAt: producedAt,
      },
      entries,
      coverage: { complete: true, gaps: 0, suppressed: 0 },
      suppressed: 0,
      encryption: { scheme: "age" as const },
    }
    const batchDigest = yield* sha256Text(
      canonicalize(Schema.decodeUnknownSync(Schema.Json)(manifestBody)),
    ).pipe(Effect.orDie)
    return {
      envelopeVersion: 1,
      manifest: { ...manifestBody, batchDigest },
      blobs: Object.fromEntries(
        encoded.map((bytes, index) => [digests[index] ?? "", bytes.toBase64()]),
      ),
    }
  })
