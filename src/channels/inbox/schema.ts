import { Schema } from "effect"

export const OriginId = Schema.Literals([
  "tuta-mail",
  "tuta-calendar",
  "finance",
  "brave",
  "immich",
  "phone",
  "cluster-digest",
  "git",
])
export type OriginId = typeof OriginId.Type

const Digest = Schema.String.pipe(Schema.check(Schema.isPattern(/^sha256:[0-9a-f]{64}$/u)))

export const IngestVerdict = Schema.Literals([
  "capture",
  "missing",
  "git.ref",
  "git.commit",
  "git.rewrite",
])

export const SourceTime = Schema.Struct({
  value: Schema.String,
  precision: Schema.Literals(["instant", "day", "estimate"]),
})

export const IngestEntry = Schema.Struct({
  resourceKey: Schema.String,
  kind: IngestVerdict,
  locator: Schema.String,
  capturedAt: Schema.String,
  sourceTime: Schema.optional(SourceTime),
  mediaType: Schema.optional(Schema.String),
  body: Digest,
  native: Schema.optional(Digest),
  evidence: Schema.Json,
})
export type IngestEntry = typeof IngestEntry.Type

export const ScanCoverage = Schema.Struct({
  complete: Schema.Boolean,
  gaps: Schema.Number,
  suppressed: Schema.Number,
})
export type ScanCoverage = typeof ScanCoverage.Type

export const ScanDescriptor = Schema.Struct({
  mode: Schema.Literals(["event", "full", "targeted"]),
  scopeHash: Schema.String,
  startedAt: Schema.String,
  completedAt: Schema.String,
})

export const IngestBatchManifest = Schema.Struct({
  origin: OriginId,
  sequence: Schema.Number,
  producedAt: Schema.String,
  scan: ScanDescriptor,
  entries: Schema.Array(IngestEntry),
  coverage: ScanCoverage,
  suppressed: Schema.Number,
  encryption: Schema.Struct({
    scheme: Schema.Literal("age"),
    /** The host writer does not put its recipient on the wire envelope. */
    recipient: Schema.optional(Schema.String),
  }),
  batchDigest: Digest,
})
export type IngestBatchManifest = typeof IngestBatchManifest.Type

/** The decrypted batch: one strict envelope, addressed blobs, no extra fields. */
export const IngestBatchEnvelope = Schema.Struct({
  envelopeVersion: Schema.Literal(1),
  manifest: IngestBatchManifest,
  blobs: Schema.Record(Schema.String, Schema.String),
})
export type IngestBatchEnvelope = typeof IngestBatchEnvelope.Type
