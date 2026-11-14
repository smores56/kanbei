import { Effect, Schema } from "effect"
import { ContentStore } from "../../kernel/cas.ts"
import { Ledger } from "../../kernel/ledger.ts"
import type { LedgerRecord } from "../../kernel/ledger.ts"
import { CaptureObservation, OBSERVATION_COMPONENT } from "../../kernel/observation.ts"
import { CoverageReceiptSchema, GapRecordSchema, HeartbeatEntrySchema } from "./state.ts"
import type { CoverageReceipt, GapRecord, HeartbeatEntry, Observation } from "./state.ts"
import { OriginId } from "./schema.ts"

/**
 * The durable half of the inbox: one coverage receipt per accepted batch, the
 * batch's heartbeat, and its gaps, plus the host freshness heartbeats. They are
 * canonical ledger records, so a restart rebuilds the reader's state instead of
 * forgetting it (`07 §6.3.3`: coverage before observations).
 */
export const COVERAGE_COMPONENT = "kb.inbox/coverage_receipt@1.0.0"
export const HOST_HEARTBEAT_COMPONENT = "kb.inbox/host_heartbeat@1.0.0"

export const BatchReceiptPayload = Schema.Struct({
  receipt: CoverageReceiptSchema,
  heartbeat: HeartbeatEntrySchema,
  gaps: Schema.Array(GapRecordSchema),
})
export type BatchReceiptPayload = typeof BatchReceiptPayload.Type

export const HostHeartbeatPayload = Schema.Struct({
  generatedAt: Schema.String,
  digest: Schema.String,
  heartbeats: Schema.Array(HeartbeatEntrySchema),
})
export type HostHeartbeatPayload = typeof HostHeartbeatPayload.Type

export const batchKey = (origin: OriginId, digest: string): string => `${origin}|${digest}`

/**
 * One item is one (origin, resource key, body). Two resources that happen to
 * carry identical bytes are two observations, not one.
 */
export const itemKey = (origin: OriginId, resourceKey: string, digest: string): string =>
  `${origin}|${resourceKey}|${digest}`

const toJson = (value: unknown): Schema.Json => Schema.decodeUnknownSync(Schema.Json)(value)

/** True when the receipt was already durable (a replay), false when appended now. */
export const appendBatchReceipt = (
  payload: BatchReceiptPayload,
): Effect.Effect<boolean, never, Ledger> =>
  Ledger.pipe(
    Effect.flatMap((ledger) =>
      ledger.append({
        components: [{ type: COVERAGE_COMPONENT, payload: toJson(payload) }],
        appender: "frozen",
        principal: "ingest",
        authorityClass: "observation",
        idempotencyKey:
          `inbox-receipt:${payload.receipt.origin}:${payload.receipt.sequence}:${payload.receipt.batchDigest}`,
      })
    ),
    Effect.map((result) => result.duplicate),
    // A record written by an older build carries a wall-clock `receivedAt`, so an
    // otherwise identical replay has the same key and a different payload. That is
    // a duplicate for this caller, not a reason to kill the kernel.
    Effect.catch((error) =>
      error.code === "idempotency_conflict"
        ? Effect.logWarning(
          "inbox receipt already stored under a legacy payload; treating as duplicate",
          {
            origin: payload.receipt.origin,
            sequence: payload.receipt.sequence,
            batchDigest: payload.receipt.batchDigest,
          },
        ).pipe(Effect.as(true))
        : Effect.fail(error)
    ),
    Effect.orDie,
  )

export const appendHostHeartbeat = (
  payload: HostHeartbeatPayload,
): Effect.Effect<boolean, never, Ledger> =>
  Ledger.pipe(
    Effect.flatMap((ledger) =>
      ledger.append({
        components: [{ type: HOST_HEARTBEAT_COMPONENT, payload: toJson(payload) }],
        appender: "frozen",
        principal: "ingest",
        authorityClass: "observation",
        idempotencyKey: `inbox-heartbeat:${payload.generatedAt}:${payload.digest}`,
      })
    ),
    Effect.map((result) => result.duplicate),
    // Same legacy-payload case as the receipt: the key is content-derived, so a
    // stored record with a wall-clock `receivedAt` is the same heartbeat.
    Effect.catch((error) =>
      error.code === "idempotency_conflict"
        ? Effect.logWarning(
          "inbox heartbeat already stored under a legacy payload; treating as duplicate",
          {
            generatedAt: payload.generatedAt,
            digest: payload.digest,
          },
        ).pipe(Effect.as(true))
        : Effect.fail(error)
    ),
    Effect.orDie,
  )

export interface RebuiltInboxState {
  readonly observations: ReadonlyArray<Observation>
  readonly receipts: ReadonlyArray<CoverageReceipt>
  readonly heartbeats: ReadonlyArray<HeartbeatEntry>
  readonly gaps: ReadonlyArray<GapRecord>
  readonly seenBatches: ReadonlySet<string>
  readonly seenItems: ReadonlySet<string>
  readonly lastSequence: Readonly<Record<string, number>>
}

const emptyRebuilt: RebuiltInboxState = {
  observations: [],
  receipts: [],
  heartbeats: [],
  gaps: [],
  seenBatches: new Set(),
  seenItems: new Set(),
  lastSequence: {},
}

const componentOf = (
  record: LedgerRecord,
  type: string,
): Schema.Json | undefined =>
  record.components.find((component) => component.type === type)?.payload

/**
 * Rebuild the source view from the ledger. Observation sizes come from the CAS
 * manifest, so nothing is guessed: a missing manifest is a corrupt store and
 * refuses the boot rather than presenting a zero-byte capture.
 */
export const rebuildInboxState = (): Effect.Effect<
  RebuiltInboxState,
  never,
  Ledger | ContentStore
> =>
  Effect.gen(function* () {
    const ledger = yield* Ledger
    const cas = yield* ContentStore
    const records = yield* ledger.records.pipe(Effect.orDie)
    const observations: Array<Observation> = []
    const receipts: Array<CoverageReceipt> = []
    const heartbeats = new Map<OriginId, HeartbeatEntry>()
    const gaps: Array<GapRecord> = []
    const seenBatches = new Set<string>()
    const seenItems = new Set<string>()
    const lastSequence: Record<string, number> = {}

    for (const record of records) {
      const capture = componentOf(record, OBSERVATION_COMPONENT)
      if (capture !== undefined) {
        const observation = Schema.decodeUnknownSync(CaptureObservation)(capture)
        const origin = Schema.decodeUnknownSync(OriginId)(observation.origin)
        const manifest = yield* cas.readManifest(observation.manifest).pipe(Effect.orDie)
        observations.push({
          origin,
          sequence: observation.sequence,
          batchDigest: "",
          resourceKey: observation.resourceKey,
          kind: "capture",
          locator: observation.locator,
          capturedAt: observation.capturedAt,
          digest: observation.body,
          bytes: manifest.size,
          mediaType: observation.mediaType,
        })
        seenItems.add(itemKey(origin, observation.resourceKey, observation.body))
      }
      const batchReceipt = componentOf(record, COVERAGE_COMPONENT)
      if (batchReceipt !== undefined) {
        const payload = Schema.decodeUnknownSync(BatchReceiptPayload)(batchReceipt)
        receipts.push(payload.receipt)
        heartbeats.set(payload.heartbeat.origin, payload.heartbeat)
        gaps.push(...payload.gaps)
        seenBatches.add(batchKey(payload.receipt.origin, payload.receipt.batchDigest))
        lastSequence[payload.receipt.origin] = Math.max(
          lastSequence[payload.receipt.origin] ?? 0,
          payload.receipt.sequence,
        )
      }
      const hostHeartbeat = componentOf(record, HOST_HEARTBEAT_COMPONENT)
      if (hostHeartbeat !== undefined) {
        const payload = Schema.decodeUnknownSync(HostHeartbeatPayload)(hostHeartbeat)
        for (const entry of payload.heartbeats) heartbeats.set(entry.origin, entry)
      }
    }

    const digestBySequence = new Map(
      receipts.map((receipt) => [`${receipt.origin}|${receipt.sequence}`, receipt.batchDigest]),
    )
    return {
      ...emptyRebuilt,
      observations: observations.map((observation) =>
        Object.assign({}, observation, {
          batchDigest: digestBySequence.get(`${observation.origin}|${observation.sequence}`) ?? "",
        })
      ),
      receipts,
      heartbeats: [...heartbeats.values()].toSorted((left, right) =>
        left.origin < right.origin ? -1 : left.origin > right.origin ? 1 : 0
      ),
      gaps,
      seenBatches,
      seenItems,
      lastSequence,
    }
  })
