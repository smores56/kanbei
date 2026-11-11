import { Effect, Schema } from "effect"
import { ContentStore } from "./cas.ts"
import type { CasError } from "./cas.ts"
import { Ledger } from "./ledger.ts"
import type { LedgerError, LedgerRecord } from "./ledger.ts"

export const OBSERVATION_COMPONENT = "kb.observation/capture@1.0.0"

export const CaptureCoverage = Schema.Struct({
  complete: Schema.Boolean,
  gaps: Schema.Number,
  suppressed: Schema.Number,
})
export type CaptureCoverage = typeof CaptureCoverage.Type

/** The canonical shape an ingested capture observation carries. */
export const CaptureObservation = Schema.Struct({
  origin: Schema.String,
  sequence: Schema.Number,
  resourceKey: Schema.String,
  locator: Schema.String,
  capturedAt: Schema.String,
  mediaType: Schema.String,
  body: Schema.String,
  manifest: Schema.String,
  coverage: CaptureCoverage,
})
export type CaptureObservation = typeof CaptureObservation.Type

export interface CaptureInput {
  readonly origin: string
  readonly sequence: number
  readonly resourceKey: string
  readonly locator: string
  readonly capturedAt: string
  readonly mediaType: string
  readonly bytes: Uint8Array
  readonly coverage: CaptureCoverage
  readonly idempotencyKey?: string
}

export interface PublishedCapture {
  readonly record: LedgerRecord
  readonly observation: CaptureObservation
}

/**
 * Publish order (`I-08`): bytes, then the manifest, then the ledger append that
 * references both. A crash before the append leaves only reclaimable content.
 */
export const publishCapture = (
  input: CaptureInput,
): Effect.Effect<PublishedCapture, CasError | LedgerError, ContentStore | Ledger> =>
  Effect.gen(function* () {
    const cas = yield* ContentStore
    const ledger = yield* Ledger
    const published = yield* cas.publish(input.bytes, input.mediaType)
    const observation: CaptureObservation = {
      origin: input.origin,
      sequence: input.sequence,
      resourceKey: input.resourceKey,
      locator: input.locator,
      capturedAt: input.capturedAt,
      mediaType: input.mediaType,
      body: published.manifest.digest,
      manifest: published.manifestDigest,
      coverage: input.coverage,
    }
    const appended = yield* ledger.append({
      components: [{
        type: OBSERVATION_COMPONENT,
        payload: Schema.decodeUnknownSync(Schema.Json)(observation),
      }],
      appender: "frozen",
      principal: "ingest",
      authorityClass: "observation",
      ...(input.idempotencyKey === undefined ? {} : { idempotencyKey: input.idempotencyKey }),
    })
    return { record: appended.record, observation }
  })

const decodeObservation = (record: LedgerRecord): Effect.Effect<CaptureObservation> =>
  Effect.sync(() => {
    const component = record.components.find((entry) => entry.type === OBSERVATION_COMPONENT)
    if (component === undefined) throw new Error(`record ${record.seq} has no capture component`)
    return Schema.decodeUnknownSync(CaptureObservation)(component.payload)
  })

/** Reads the projected captures in recording order; each names its CAS manifest. */
export const readCaptures = (): Effect.Effect<
  ReadonlyArray<PublishedCapture>,
  LedgerError,
  Ledger
> =>
  Effect.gen(function* () {
    const ledger = yield* Ledger
    const records = yield* ledger.records
    const captures = yield* Effect.forEach(
      records.filter((record) =>
        record.components.some((component) => component.type === OBSERVATION_COMPONENT)
      ),
      (record) =>
        decodeObservation(record).pipe(Effect.map((observation) => ({ record, observation }))),
      { concurrency: 1 },
    )
    return captures
  })
