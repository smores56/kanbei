import { Effect, Schema } from "effect"
import { canonicalize } from "../evidence/canonical.ts"
import { sha256Text } from "../evidence/digest.ts"
import type { LedgerRecord } from "./ledger.ts"
import { OBSERVATION_COMPONENT } from "./observation.ts"
import { CaptureObservation } from "./observation.ts"

export const ProjectedCapture = Schema.Struct({
  recordId: Schema.String,
  origin: Schema.String,
  sequence: Schema.Number,
  resourceKey: Schema.String,
  body: Schema.String,
  manifest: Schema.String,
})
export type ProjectedCapture = typeof ProjectedCapture.Type

export const OperationalProjection = Schema.Struct({
  lastSeq: Schema.Number,
  headHash: Schema.String,
  recordCount: Schema.Number,
  authorityCounts: Schema.Record(Schema.String, Schema.Number),
  captures: Schema.Array(ProjectedCapture),
  coverage: Schema.Struct({
    watermarkSeq: Schema.Number,
    pending: Schema.Number,
    state: Schema.Literals(["current", "building"]),
  }),
})
export type OperationalProjection = typeof OperationalProjection.Type

const authorityClassOf = (record: LedgerRecord): string => {
  const component = record.components.find((entry) => entry.type === "kb.core/authority@1.0.0")
  if (component === undefined) return "unclaimed"
  try {
    const decoded = Schema.decodeUnknownSync(
      Schema.Struct({ class: Schema.String, principal: Schema.String }),
    )(component.payload)
    return decoded.class
  } catch {
    return "invalid"
  }
}

const captureOf = (record: LedgerRecord): ProjectedCapture | undefined => {
  const component = record.components.find((entry) => entry.type === OBSERVATION_COMPONENT)
  if (component === undefined) return undefined
  try {
    const observation = Schema.decodeUnknownSync(CaptureObservation)(component.payload)
    return {
      recordId: record.record_id,
      origin: observation.origin,
      sequence: observation.sequence,
      resourceKey: observation.resourceKey,
      body: observation.body,
      manifest: observation.manifest,
    }
  } catch {
    return undefined
  }
}

/**
 * A deterministic projection over the committed prefix. Rebuilding twice from
 * the same rows must produce the same state (`R-01`), and a watermark below the
 * head reports `building` with the exact pending count (`I-16`).
 */
export const buildProjection = (
  records: ReadonlyArray<LedgerRecord>,
  watermarkSeq: number,
): OperationalProjection => {
  const authorityCounts = Object.fromEntries(
    [...Map.groupBy(records, authorityClassOf).entries()].map((
      [key, group],
    ) => [key, group.length]),
  )
  const last = records.at(-1)
  const pending = Math.max(0, records.length - watermarkSeq)
  return {
    lastSeq: last?.seq ?? 0,
    headHash: last?.record_hash ?? "",
    recordCount: records.length,
    authorityCounts: Object.fromEntries(
      Object.entries(authorityCounts).toSorted(([left], [right]) => left < right ? -1 : 1),
    ),
    captures: records.flatMap((record) => {
      const capture = captureOf(record)
      return capture === undefined ? [] : [capture]
    }),
    coverage: {
      watermarkSeq: Math.min(watermarkSeq, records.length),
      pending,
      state: pending === 0 ? "current" : "building",
    },
  }
}

export const projectionDigest = (projection: OperationalProjection): Effect.Effect<string> =>
  sha256Text(canonicalize(Schema.decodeUnknownSync(Schema.Json)(projection))).pipe(Effect.orDie)
