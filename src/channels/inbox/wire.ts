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
export const MAX_WIRE_ITEMS = 1000

export const WireKind = Schema.Literals(["batch", "alert"])
export type WireKind = typeof WireKind.Type

const strict = { onExcessProperty: "error" } as const

export const HostBatchEnvelope = Schema.Struct({
  v: Schema.Literal(WIRE_VERSION),
  source: Schema.String,
  kind: WireKind,
  seq: Schema.optional(Schema.Number),
  sent_at: Schema.optional(Schema.String),
  items: Schema.Array(Schema.Json),
})
export type HostBatchEnvelope = typeof HostBatchEnvelope.Type

export const parseHostEnvelope = (value: unknown): HostBatchEnvelope =>
  Schema.decodeUnknownSync(HostBatchEnvelope, strict)(value)

/** True when the document is the host wire envelope rather than the rich manifest. */
export const isHostEnvelope = (value: unknown): boolean =>
  typeof value === "object" && value !== null && "v" in value && "items" in value &&
  "source" in value

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
    const producedAt = envelope.sent_at ?? new Date(0).toISOString()
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
