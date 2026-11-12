import { Clock, Context, Data, Effect, Layer, Option, Ref, Result, Schema } from "effect"
import { canonicalize } from "../../evidence/canonical.ts"
import { sha256, sha256Text } from "../../evidence/digest.ts"
import type { ContentDigest } from "../../kernel/blob-sink.ts"
import { ContentStore } from "../../kernel/cas.ts"
import { Ledger } from "../../kernel/ledger.ts"
import { publishCaptureFrom } from "../../kernel/observation.ts"
import type { PublishedContent } from "../../kernel/cas.ts"
import { AgeDecryptor } from "./age.ts"
import { InboxCheckpointStore } from "./checkpoint.ts"
import type { InboxCheckpoint } from "./checkpoint.ts"
import { InboxDirectory } from "./directory.ts"
import { InboxQuarantineStore } from "./quarantine.ts"
import { IngestBatchEnvelope } from "./schema.ts"
import type { IngestBatchEnvelope as BatchEnvelope, OriginId } from "./schema.ts"
import {
  appendBatchReceipt,
  appendHostHeartbeat,
  batchKey,
  itemKey,
  rebuildInboxState,
} from "./receipts.ts"
import { heartbeatState } from "./state.ts"
import type {
  CoverageReceipt,
  GapRecord,
  HeartbeatEntry,
  Observation,
  SourceHeartbeat,
} from "./state.ts"
import {
  hostEnvelopeToBatch,
  isHeartbeatEnvelope,
  isHostEnvelope,
  MAX_WIRE_ITEMS_PER_SOURCE,
  parseHostEnvelope,
  parseHostHeartbeat,
} from "./wire.ts"

export type { CoverageReceipt, GapRecord, Observation, SourceHeartbeat } from "./state.ts"

export interface InboxLimits {
  readonly maxBatchBytes: number
  readonly maxBatchesPerPoll: number
  readonly maxEntriesPerBatch: number
  readonly maxEntryBytes: number
  readonly maxTotalBlobBytes: number
}

export interface InboxConfig {
  readonly identityPath: string
  readonly allowedOrigins: ReadonlyArray<OriginId>
  readonly limits: InboxLimits
  readonly freshnessWindowMs: number
}

export type QuarantineReason =
  | "read_failed"
  | "decrypt_failed"
  | "parse_failed"
  | "source_not_allowed"
  | "invalid_sequence"
  | "batch_too_large"
  | "entry_too_large"
  | "too_many_entries"
  | "digest_mismatch"
  | "credential_detected"
  | "publish_failed"

export interface QuarantineRecord {
  readonly name: string
  readonly reason: QuarantineReason
  readonly detail: string
  readonly ciphertextDigest?: ContentDigest
}

export interface SecurityEvent {
  readonly kind: "credential_detected"
  readonly origin?: OriginId
  readonly name: string
  readonly detail: string
}

export interface InboxSnapshot {
  readonly observations: ReadonlyArray<Observation>
  readonly receipts: ReadonlyArray<CoverageReceipt>
  readonly heartbeats: ReadonlyArray<SourceHeartbeat>
  readonly gaps: ReadonlyArray<GapRecord>
  readonly quarantined: ReadonlyArray<QuarantineRecord>
  readonly securityEvents: ReadonlyArray<SecurityEvent>
  readonly duplicates: number
}

export interface PollResult {
  readonly processed: number
  readonly accepted: number
  readonly quarantined: number
  readonly duplicates: number
}

export class InboxError extends Data.TaggedError("InboxError")<{
  readonly operation: "list" | "checkpoint"
  readonly reason: string
}> {}

export interface InboxReader {
  readonly poll: Effect.Effect<PollResult, InboxError, ContentStore | Ledger>
  readonly state: Effect.Effect<InboxSnapshot>
  readonly sourceState: (
    origin: OriginId,
  ) => Effect.Effect<"current" | "stale" | "unknown">
  readonly heartbeat: (origin: OriginId) => Effect.Effect<Option.Option<SourceHeartbeat>>
}

export const InboxReader = Context.Service<InboxReader>("kanbei/channel/inbox/Reader")

interface InboxState {
  readonly observations: ReadonlyArray<Observation>
  readonly receipts: ReadonlyArray<CoverageReceipt>
  readonly heartbeats: ReadonlyArray<HeartbeatEntry>
  readonly gaps: ReadonlyArray<GapRecord>
  readonly quarantined: ReadonlyArray<QuarantineRecord>
  readonly securityEvents: ReadonlyArray<SecurityEvent>
  readonly seenBatches: ReadonlySet<string>
  readonly seenItems: ReadonlySet<string>
  readonly seenHeartbeats: ReadonlySet<string>
  readonly lastSequence: Readonly<Record<string, number>>
  readonly handled: ReadonlySet<string>
  readonly quarantineNames: ReadonlySet<string>
  readonly duplicates: number
}

const emptyState: InboxState = {
  observations: [],
  receipts: [],
  heartbeats: [],
  gaps: [],
  quarantined: [],
  securityEvents: [],
  seenBatches: new Set(),
  seenItems: new Set(),
  seenHeartbeats: new Set(),
  lastSequence: {},
  handled: new Set(),
  quarantineNames: new Set(),
  duplicates: 0,
}

const stateFromCheckpoint = (checkpoint: InboxCheckpoint): InboxState => ({
  ...emptyState,
  seenBatches: new Set(checkpoint.seenBatches),
  seenItems: new Set(checkpoint.seenItems),
  seenHeartbeats: new Set(checkpoint.seenHeartbeats ?? []),
  lastSequence: { ...checkpoint.lastSequence },
  handled: new Set(checkpoint.handled),
  quarantineNames: new Set(Object.keys(checkpoint.quarantined)),
})

const checkpointFromState = (state: InboxState): InboxCheckpoint => ({
  version: 1,
  handled: [...state.handled].toSorted(),
  quarantined: Object.fromEntries(
    state.quarantined.map((record) => [record.name, record.reason]),
  ),
  seenBatches: [...state.seenBatches].toSorted(),
  seenItems: [...state.seenItems].toSorted(),
  seenHeartbeats: [...state.seenHeartbeats].toSorted(),
  lastSequence: { ...state.lastSequence },
})

interface InvalidBatch {
  readonly reason: QuarantineReason
  readonly detail: string
}

const decodeBase64 = (value: string): Result.Result<Uint8Array, string> => {
  try {
    return Result.succeed(Uint8Array.fromBase64(value))
  } catch (error) {
    return Result.fail(error instanceof Error ? error.message : String(error))
  }
}

const invalid = (detail: string): InvalidBatch => ({ reason: "parse_failed", detail })

/** Structural detail only: a parse error message can quote the plaintext. */
const decodeJson = (plaintext: Uint8Array): Result.Result<unknown, InvalidBatch> => {
  try {
    return Result.succeed(JSON.parse(new TextDecoder().decode(plaintext)))
  } catch {
    return Result.fail(invalid(`plaintext is not JSON ( bytes)`))
  }
}

/**
 * Accept both producers: the host writer's `{v,source,kind,seq,sent_at,items}`
 * envelope (converted to the rich form) and the rich manifest+blobs envelope the
 * journey fixtures ship. Unknown versions and extra fields are refusals.
 */
export const parseBatchJson = (
  value: unknown,
  name: string,
): Effect.Effect<Result.Result<BatchEnvelope, InvalidBatch>> =>
  Effect.gen(function* () {
    if (isHostEnvelope(value)) {
      const host = yield* Effect.result(Effect.try({
        try: () => parseHostEnvelope(value),
        catch: (error) => error,
      }))
      if (Result.isFailure(host)) {
        return Result.fail(invalid("host envelope does not match the wire schema"))
      }
      const converted = yield* Effect.result(hostEnvelopeToBatch(host.success, name))
      return Result.isFailure(converted)
        ? Result.fail(invalid("host envelope could not be converted"))
        : Result.succeed(converted.success)
    }
    return Result.try({
      try: () =>
        Schema.decodeUnknownSync(IngestBatchEnvelope, { onExcessProperty: "error" })(value),
      catch: () => invalid(`manifest does not match the schema ( bytes)`),
    })
  })

export const parseBatch = (
  plaintext: Uint8Array,
  name: string,
): Effect.Effect<Result.Result<BatchEnvelope, InvalidBatch>> =>
  Effect.gen(function* () {
    const json = decodeJson(plaintext)
    if (Result.isFailure(json)) return Result.fail(json.failure)
    return yield* parseBatchJson(json.success, name)
  })

const CREDENTIAL_KEY =
  /(^|\.)(authorization|api[_-]?key|access[_-]?token|refresh[_-]?token|session[_-]?token|secret|password|credential)$/iu
const CREDENTIAL_VALUE =
  /(?:Bearer\s+[A-Za-z0-9._~+/-]{8,}|sk-[A-Za-z0-9]{12,}|ghp_[A-Za-z0-9]{20,}|AKIA[0-9A-Z]{16})/u

const scanCredentials = (value: unknown, path: string): ReadonlyArray<string> => {
  if (value === null || typeof value !== "object") {
    if (typeof value === "string" && CREDENTIAL_VALUE.test(value)) return [path]
    return []
  }
  if (Array.isArray(value)) {
    return value.flatMap((item, index) => scanCredentials(item, `${path}[${index}]`))
  }
  return Object.entries(value).flatMap(([key, item]) => {
    const next = path === "" ? key : `${path}.${key}`
    return CREDENTIAL_KEY.test(key) ? [next] : scanCredentials(item, next)
  })
}

const scanText = (text: string, path: string): ReadonlyArray<string> => {
  try {
    return scanCredentials(JSON.parse(text), path)
  } catch {
    return CREDENTIAL_VALUE.test(text) ? [path] : []
  }
}

const sequenceDelta = (
  last: number | undefined,
  sequence: number,
  origin: OriginId,
): { readonly gaps: ReadonlyArray<GapRecord>; readonly next: number } => {
  if (last === undefined) {
    return sequence > 1
      ? {
        gaps: [{
          origin,
          kind: "gap",
          missing: sequence - 1,
          sequence,
          detail: `first accepted sequence is ${sequence}`,
        }],
        next: sequence,
      }
      : { gaps: [], next: sequence }
  }
  if (sequence <= last) {
    return {
      gaps: [{
        origin,
        kind: "out_of_order",
        missing: 0,
        sequence,
        detail: `sequence ${sequence} arrived after ${last}`,
      }],
      next: last,
    }
  }
  if (sequence > last + 1) {
    return {
      gaps: [{
        origin,
        kind: "gap",
        missing: sequence - last - 1,
        sequence,
        detail: `missing ${last + 1}..${sequence - 1}`,
      }],
      next: sequence,
    }
  }
  return { gaps: [], next: sequence }
}

const iso = (millis: number): string => new Date(millis).toISOString()

/** A restart merges the checkpoint's watermark with the ledger's receipts. */
const mergeLastSequence = (
  left: Readonly<Record<string, number>>,
  right: Readonly<Record<string, number>>,
): Readonly<Record<string, number>> => {
  const merged: Record<string, number> = { ...left }
  for (const [origin, sequence] of Object.entries(right)) {
    merged[origin] = Math.max(merged[origin] ?? 0, sequence)
  }
  return merged
}

const heartbeatAt = (entry: HeartbeatEntry, now: number, window: number): SourceHeartbeat => ({
  ...entry,
  // Freshness is the producer's clock: a late delivery of an old batch is stale,
  // and an unparseable producer time is unknown rather than current.
  state: heartbeatState(entry, now, window),
})

/**
 * Reader admission per poll. Names are only a scheduling hint here; the file
 * content still decides the origin, so a hostile name can only starve itself.
 */
const selectPollNames = <A extends { readonly name: string }>(
  files: ReadonlyArray<A>,
  origins: ReadonlyArray<OriginId>,
  maxBatches: number,
): ReadonlyArray<A> => {
  const counts = new Map<string, number>()
  const selected: Array<A> = []
  for (const file of files.toSorted((left, right) => left.name.localeCompare(right.name))) {
    if (selected.length >= maxBatches) break
    const origin = origins.find((candidate) => file.name.startsWith(`${candidate}-`))
    if (origin !== undefined) {
      const used = counts.get(origin) ?? 0
      if (used >= MAX_WIRE_ITEMS_PER_SOURCE) continue
      counts.set(origin, used + 1)
    }
    selected.push(file)
  }
  return selected
}

const configIssues = (config: InboxConfig): ReadonlyArray<string> => {
  const issues: Array<string> = []
  const limits = config.limits
  for (const [name, value] of Object.entries(limits)) {
    if (!Number.isFinite(value) || value <= 0) issues.push(`${name} must be positive`)
  }
  if (config.identityPath.trim() === "") issues.push("identityPath is required")
  if (config.freshnessWindowMs < 0) issues.push("freshnessWindowMs must not be negative")
  if (config.allowedOrigins.length === 0) issues.push("allowedOrigins must not be empty")
  return issues
}

export const inboxReaderLayer = (
  config: InboxConfig,
): Layer.Layer<
  InboxReader,
  never,
  | InboxDirectory
  | AgeDecryptor
  | InboxCheckpointStore
  | InboxQuarantineStore
  | ContentStore
  | Ledger
> =>
  Layer.effect(
    InboxReader,
    Effect.gen(function* () {
      const issues = configIssues(config)
      if (issues.length > 0) {
        return yield* Effect.die(new Error(`invalid inbox config: ${issues.join("; ")}`))
      }
      const directory = yield* InboxDirectory
      const decryptor = yield* AgeDecryptor
      const quarantines = yield* InboxQuarantineStore
      const checkpoints = yield* InboxCheckpointStore
      const checkpoint = yield* checkpoints.load.pipe(Effect.orDie)
      const recorded = yield* quarantines.load.pipe(Effect.orDie)
      // Receipts, heartbeats, gaps, and captures are canonical ledger records, so
      // a restarted reader rebuilds them instead of forgetting every source.
      const cas = yield* ContentStore
      const ledger = yield* Ledger
      const rebuilt = yield* rebuildInboxState().pipe(
        Effect.provideService(ContentStore, cas),
        Effect.provideService(Ledger, ledger),
      )
      const state = yield* Ref.make<InboxState>({
        ...stateFromCheckpoint(checkpoint),
        observations: rebuilt.observations,
        receipts: rebuilt.receipts,
        heartbeats: rebuilt.heartbeats,
        gaps: rebuilt.gaps,
        seenBatches: new Set([...checkpoint.seenBatches, ...rebuilt.seenBatches]),
        seenItems: new Set([...checkpoint.seenItems, ...rebuilt.seenItems]),
        lastSequence: mergeLastSequence(checkpoint.lastSequence, rebuilt.lastSequence),
        quarantined: recorded.map((entry) => ({
          name: entry.name,
          reason: entry.reason,
          detail: entry.detail,
          ...(entry.ciphertextDigest === undefined
            ? {}
            : { ciphertextDigest: entry.ciphertextDigest }),
        })),
        // A stale checkpoint must not re-admit a name the sidecar already buried.
        quarantineNames: new Set(recorded.map((entry) => entry.name)),
        handled: new Set([...checkpoint.handled, ...recorded.map((entry) => entry.name)]),
      })

      const quarantine = (
        name: string,
        reason: QuarantineReason,
        detail: string,
        ciphertextDigest?: ContentDigest,
      ): Effect.Effect<void> =>
        Effect.gen(function* () {
          const entry = {
            name,
            reason,
            detail,
            ...(ciphertextDigest === undefined ? {} : { ciphertextDigest }),
          }
          yield* Ref.update(state, (current) => ({
            ...current,
            quarantined: [...current.quarantined, entry],
            handled: new Set([...current.handled, name]),
            quarantineNames: new Set([...current.quarantineNames, name]),
          }))
          // Durable beside the checkpoint: a restarted reader must skip it too.
          yield* quarantines.record(entry).pipe(Effect.orDie)
        })

      /**
       * The host's own freshness envelope. It is admitted only for allowlisted
       * sources, appended to the ledger before the reader reports on it, and
       * deduped by generation time plus content digest, so a copied heartbeat
       * file cannot refresh a source.
       */
      const processHeartbeat = (
        name: string,
        value: unknown,
        ciphertextDigest: ContentDigest,
      ): Effect.Effect<"accepted" | "quarantined" | "duplicate", never, Ledger> =>
        Effect.gen(function* () {
          const parsed = yield* Effect.result(Effect.try({
            try: () => parseHostHeartbeat(value),
            catch: () => invalid("heartbeat does not match the host schema"),
          }))
          if (Result.isFailure(parsed)) {
            yield* quarantine(name, parsed.failure.reason, parsed.failure.detail, ciphertextDigest)
            return "quarantined" as const
          }
          const envelope = parsed.success
          const known = new Set<string>(config.allowedOrigins)
          const unknown = Object.keys(envelope.sources)
            .filter((source) => !known.has(source))
            .toSorted()
          if (unknown.length > 0) {
            yield* quarantine(
              name,
              "source_not_allowed",
              `heartbeat sources ${unknown.join(",")}`,
              ciphertextDigest,
            )
            return "quarantined" as const
          }
          const unlistedHost = envelope.host_sources.filter((source) =>
            !(source in envelope.sources)
          )
          if (unlistedHost.length > 0) {
            yield* quarantine(
              name,
              "parse_failed",
              `host sources outside the status map: ${unlistedHost.join(",")}`,
              ciphertextDigest,
            )
            return "quarantined" as const
          }
          const digest = yield* sha256Text(
            canonicalize(Schema.decodeUnknownSync(Schema.Json)(envelope)),
          ).pipe(Effect.orDie)
          const dedupe = `${envelope.generated_at}|${digest}`
          const current = yield* Ref.get(state)
          const duplicate = (): Effect.Effect<"duplicate"> =>
            Ref.update(state, (previous) => ({
              ...previous,
              duplicates: previous.duplicates + 1,
              handled: new Set([...previous.handled, name]),
            })).pipe(Effect.as("duplicate" as const))
          if (current.seenHeartbeats.has(dedupe)) return yield* duplicate()

          const now = yield* Clock.currentTimeMillis
          const heartbeats: ReadonlyArray<HeartbeatEntry> = [...config.allowedOrigins]
            .filter((origin) => origin in envelope.sources)
            .toSorted()
            .flatMap((origin) => {
              const status = envelope.sources[origin]
              if (status === undefined) return []
              return [{
                origin,
                kind: "host" as const,
                sequence: 0,
                // The source's basis is its last successful batch; a source with
                // none has no producer time and stays unknown.
                producedAt: status.last_ok ?? "",
                generatedAt: envelope.generated_at,
                receivedAt: iso(now),
                // The host envelope carries freshness, not scan coverage; it never
                // claims a complete scan.
                coverage: { complete: false, gaps: 0, suppressed: 0 },
                windowMs: status.window_s * 1000,
                hostStale: status.stale,
                ...(typeof status.last_ok === "string" ? { lastOk: status.last_ok } : {}),
              }]
            })
          const appended = yield* appendHostHeartbeat({
            generatedAt: envelope.generated_at,
            digest,
            heartbeats,
          }).pipe(Effect.result)
          if (Result.isFailure(appended)) {
            yield* quarantine(
              name,
              "publish_failed",
              "the heartbeat could not be appended",
              ciphertextDigest,
            )
            return "quarantined" as const
          }
          if (appended.success) return yield* duplicate()
          const origins = new Set(heartbeats.map((entry) => entry.origin))
          yield* Ref.update(state, (previous) => ({
            ...previous,
            heartbeats: [
              ...previous.heartbeats.filter((entry) => !origins.has(entry.origin)),
              ...heartbeats,
            ],
            seenHeartbeats: new Set([...previous.seenHeartbeats, dedupe]),
            handled: new Set([...previous.handled, name]),
          }))
          return "accepted" as const
        })

      const processFile = (
        name: string,
        size: number,
      ): Effect.Effect<"accepted" | "quarantined" | "duplicate", never, ContentStore | Ledger> =>
        Effect.gen(function* () {
          if (size > config.limits.maxBatchBytes + 65_536) {
            yield* quarantine(name, "batch_too_large", `ciphertext ${size} bytes`)
            return "quarantined" as const
          }
          const read = yield* Effect.result(directory.read(name))
          if (Result.isFailure(read)) {
            yield* quarantine(name, "read_failed", read.failure.reason)
            return "quarantined" as const
          }
          const ciphertext = read.success
          const ciphertextDigest = yield* sha256(ciphertext).pipe(Effect.orDie)
          const decrypted = yield* Effect.result(decryptor.decrypt(ciphertext))
          if (Result.isFailure(decrypted)) {
            yield* quarantine(name, "decrypt_failed", decrypted.failure.reason, ciphertextDigest)
            return "quarantined" as const
          }
          const plaintext = decrypted.success
          if (plaintext.length > config.limits.maxBatchBytes) {
            yield* quarantine(name, "batch_too_large", `plaintext ${plaintext.length} bytes`)
            return "quarantined" as const
          }
          const decoded = decodeJson(plaintext)
          if (Result.isFailure(decoded)) {
            yield* quarantine(
              name,
              decoded.failure.reason,
              decoded.failure.detail,
              ciphertextDigest,
            )
            return "quarantined" as const
          }
          if (isHeartbeatEnvelope(decoded.success)) {
            return yield* processHeartbeat(name, decoded.success, ciphertextDigest)
          }
          const parsed = yield* parseBatchJson(decoded.success, name)
          if (Result.isFailure(parsed)) {
            yield* quarantine(name, parsed.failure.reason, parsed.failure.detail, ciphertextDigest)
            return "quarantined" as const
          }
          const envelope = parsed.success
          const manifest = envelope.manifest
          if (!config.allowedOrigins.includes(manifest.origin)) {
            yield* quarantine(
              name,
              "source_not_allowed",
              `origin ${manifest.origin}`,
              ciphertextDigest,
            )
            return "quarantined" as const
          }
          // The host accepts `seq = 0`; anything negative or fractional is not a sequence.
          if (!Number.isInteger(manifest.sequence) || manifest.sequence < 0) {
            yield* quarantine(name, "invalid_sequence", `sequence ${manifest.sequence}`)
            return "quarantined" as const
          }

          const current = yield* Ref.get(state)
          const key = batchKey(manifest.origin, manifest.batchDigest)
          if (current.seenBatches.has(key)) {
            yield* Ref.update(state, (previous) => ({
              ...previous,
              duplicates: previous.duplicates + 1,
              handled: new Set([...previous.handled, name]),
            }))
            return "duplicate" as const
          }

          const { batchDigest, ...manifestWithoutDigest } = manifest
          const manifestJson = Schema.decodeUnknownSync(Schema.Json)(manifestWithoutDigest)
          const recomputed = yield* sha256Text(canonicalize(manifestJson)).pipe(Effect.orDie)
          if (recomputed !== batchDigest) {
            yield* quarantine(
              name,
              "digest_mismatch",
              `manifest digest ${batchDigest} != ${recomputed}`,
              ciphertextDigest,
            )
            return "quarantined" as const
          }
          if (manifest.entries.length > config.limits.maxEntriesPerBatch) {
            yield* quarantine(
              name,
              "too_many_entries",
              `${manifest.entries.length} entries`,
              ciphertextDigest,
            )
            return "quarantined" as const
          }

          const blobs = new Map<ContentDigest, Uint8Array>()
          for (const [digest, encoded] of Object.entries(envelope.blobs)) {
            const bytes = decodeBase64(encoded)
            if (Result.isFailure(bytes)) {
              yield* quarantine(name, "parse_failed", `blob ${digest} is not base64`)
              return "quarantined" as const
            }
            const actual = yield* sha256(bytes.success).pipe(Effect.orDie)
            if (actual !== digest) {
              yield* quarantine(name, "parse_failed", `blob ${digest} hashes to ${actual}`)
              return "quarantined" as const
            }
            if (bytes.success.length > config.limits.maxEntryBytes) {
              yield* quarantine(
                name,
                "entry_too_large",
                `blob ${digest} is ${bytes.success.length}`,
              )
              return "quarantined" as const
            }
            blobs.set(digest, bytes.success)
          }
          const referenced = new Set(manifest.entries.map((entry) => entry.body))
          const unreferenced = [...blobs.keys()].filter((digest) => !referenced.has(digest))
          if (unreferenced.length > 0) {
            yield* quarantine(name, "parse_failed", `unreferenced blobs: ${unreferenced.join(",")}`)
            return "quarantined" as const
          }
          const missingBodies = [...referenced].filter((digest) => !blobs.has(digest))
          if (missingBodies.length > 0) {
            yield* quarantine(name, "parse_failed", `missing blobs: ${missingBodies.join(",")}`)
            return "quarantined" as const
          }
          const totalBytes = [...blobs.values()].reduce((total, bytes) => total + bytes.length, 0)
          if (totalBytes > config.limits.maxTotalBlobBytes) {
            yield* quarantine(name, "batch_too_large", `blobs total ${totalBytes} bytes`)
            return "quarantined" as const
          }

          const findings = [
            ...scanCredentials(manifest, ""),
            ...[...blobs.entries()].flatMap(([digest, bytes]) =>
              scanText(new TextDecoder().decode(bytes), `blobs.${digest}`)
            ),
          ]
          if (findings.length > 0) {
            yield* Ref.update(state, (previous) => ({
              ...previous,
              securityEvents: [
                ...previous.securityEvents,
                {
                  kind: "credential_detected" as const,
                  origin: manifest.origin,
                  name,
                  detail: findings.slice(0, 20).join(","),
                },
              ],
            }))
            yield* quarantine(
              name,
              "credential_detected",
              findings.slice(0, 20).join(","),
              ciphertextDigest,
            )
            return "quarantined" as const
          }

          // `07 §6.3.3`: bodies to the CAS, then one coverage receipt, and only
          // then observations. A crash before the receipt leaves reclaimable
          // bytes and no observation; the file is re-read and converges because
          // every append carries an idempotency key.
          const bodies = new Map<ContentDigest, Uint8Array>()
          for (const entry of manifest.entries) {
            const bytes = blobs.get(entry.body)
            if (bytes !== undefined && !bodies.has(entry.body)) bodies.set(entry.body, bytes)
          }
          const published = new Map<ContentDigest, PublishedContent>()
          const stored = yield* Effect.forEach(
            [...bodies.entries()],
            ([digest, bytes]) => {
              const entry = manifest.entries.find((candidate) => candidate.body === digest)
              return cas.publish(bytes, entry?.mediaType ?? "application/octet-stream").pipe(
                Effect.map((content) => published.set(digest, content)),
              )
            },
            { concurrency: 1 },
          ).pipe(Effect.result)
          if (Result.isFailure(stored)) {
            yield* quarantine(
              name,
              "publish_failed",
              "content-addressed bytes could not be published",
              ciphertextDigest,
            )
            return "quarantined" as const
          }

          const now = yield* Clock.currentTimeMillis
          const delta = sequenceDelta(
            current.lastSequence[manifest.origin],
            manifest.sequence,
            manifest.origin,
          )
          const suppression: ReadonlyArray<GapRecord> = manifest.suppressed > 0
            ? [{
              origin: manifest.origin,
              kind: "suppression",
              missing: manifest.suppressed,
              sequence: manifest.sequence,
              detail: `${manifest.suppressed} entries suppressed by the source`,
            }]
            : []
          const batchGaps = [...delta.gaps, ...suppression]
          const seenItems = new Set(current.seenItems)
          const created = manifest.entries.flatMap((entry) => {
            // Distinct resource keys with identical bytes are distinct observations.
            const item = itemKey(manifest.origin, entry.resourceKey, entry.body)
            if (seenItems.has(item)) return []
            seenItems.add(item)
            const bytes = blobs.get(entry.body)
            if (bytes === undefined) return []
            return [{
              origin: manifest.origin,
              sequence: manifest.sequence,
              batchDigest: manifest.batchDigest,
              resourceKey: entry.resourceKey,
              kind: entry.kind,
              locator: entry.locator,
              capturedAt: entry.capturedAt,
              digest: entry.body,
              bytes: bytes.length,
              ...(entry.mediaType === undefined ? {} : { mediaType: entry.mediaType }),
            }]
          })
          const duplicateItems = manifest.entries.length - created.length
          const receipt: CoverageReceipt = {
            origin: manifest.origin,
            batchDigest: manifest.batchDigest,
            sequence: manifest.sequence,
            receivedAt: iso(now),
            entries: created.length,
            publishedBytes: totalBytes,
            coverage: manifest.coverage,
          }
          const heartbeat: HeartbeatEntry = {
            origin: manifest.origin,
            kind: "batch",
            sequence: manifest.sequence,
            producedAt: manifest.producedAt,
            receivedAt: iso(now),
            coverage: {
              complete: manifest.coverage.complete,
              gaps: manifest.coverage.gaps +
                batchGaps.filter((gap) => gap.kind === "gap").length,
              suppressed: manifest.suppressed,
            },
          }
          const receiptRecord = yield* appendBatchReceipt({
            receipt,
            heartbeat,
            gaps: batchGaps,
          }).pipe(Effect.result)
          if (Result.isFailure(receiptRecord)) {
            yield* quarantine(
              name,
              "publish_failed",
              "the coverage receipt could not be appended",
              ciphertextDigest,
            )
            return "quarantined" as const
          }
          const captures = yield* Effect.forEach(
            created,
            (observation) => {
              const content = published.get(observation.digest)
              if (content === undefined) {
                return Effect.fail(`observation ${observation.resourceKey} has no published body`)
              }
              return publishCaptureFrom(content, {
                origin: observation.origin,
                sequence: observation.sequence,
                resourceKey: observation.resourceKey,
                locator: observation.locator,
                capturedAt: observation.capturedAt,
                mediaType: observation.mediaType ?? "application/octet-stream",
                bytes: bodies.get(observation.digest) ?? new Uint8Array(),
                coverage: manifest.coverage,
                idempotencyKey:
                  `inbox:${manifest.origin}:${manifest.sequence}:${observation.resourceKey}:${observation.digest}`,
              }).pipe(Effect.mapError((error) => error.detail), Effect.asVoid)
            },
            { concurrency: 1 },
          ).pipe(Effect.result)
          if (Result.isFailure(captures)) {
            yield* quarantine(
              name,
              "publish_failed",
              "an observation could not be appended",
              ciphertextDigest,
            )
            return "quarantined" as const
          }
          const receiptIsNew = !receiptRecord.success
          yield* Ref.update(state, (previous) => ({
            ...previous,
            observations: [...previous.observations, ...created],
            ...(receiptIsNew
              ? {
                receipts: [...previous.receipts, receipt],
                gaps: [...previous.gaps, ...batchGaps],
                heartbeats: [
                  ...previous.heartbeats.filter((entry) => entry.origin !== manifest.origin),
                  heartbeat,
                ],
              }
              : {}),
            seenBatches: new Set([...previous.seenBatches, key]),
            seenItems,
            handled: new Set([...previous.handled, name]),
            duplicates: previous.duplicates + duplicateItems,
            lastSequence: { ...previous.lastSequence, [manifest.origin]: delta.next },
          }))
          return "accepted" as const
        })

      const poll: InboxReader["poll"] = Effect.gen(function* () {
        const listing = yield* directory.list.pipe(
          Effect.mapError((error) => new InboxError({ operation: "list", reason: error.reason })),
        )
        const current = yield* Ref.get(state)
        // Select the next unprocessed files, not the first N names: handled and
        // quarantined files stay on disk but never occupy the poll window again.
        const names = selectPollNames(
          listing
            .filter((file) => file.name.endsWith(".age"))
            .filter((file) =>
              !current.handled.has(file.name) && !current.quarantineNames.has(file.name)
            ),
          config.allowedOrigins,
          config.limits.maxBatchesPerPoll,
        )
        const outcomes = yield* Effect.forEach(
          names,
          (file) => processFile(file.name, file.size),
          { concurrency: 1 },
        )
        const next = yield* Ref.get(state)
        yield* checkpoints.save(checkpointFromState(next)).pipe(
          Effect.mapError((error) =>
            new InboxError({ operation: "checkpoint", reason: error.reason })
          ),
        )
        return {
          processed: outcomes.length,
          accepted: outcomes.filter((outcome) => outcome === "accepted").length,
          quarantined: outcomes.filter((outcome) => outcome === "quarantined").length,
          duplicates: outcomes.filter((outcome) => outcome === "duplicate").length,
        }
      })

      const snapshot = (now: number): Effect.Effect<InboxSnapshot> =>
        Ref.get(state).pipe(
          Effect.map((value) => ({
            observations: value.observations,
            receipts: value.receipts,
            heartbeats: value.heartbeats.map((entry) =>
              heartbeatAt(entry, now, config.freshnessWindowMs)
            ),
            gaps: value.gaps,
            quarantined: value.quarantined,
            securityEvents: value.securityEvents,
            duplicates: value.duplicates,
          })),
        )

      return {
        poll,
        state: Effect.gen(function* () {
          const now = yield* Clock.currentTimeMillis
          return yield* snapshot(now)
        }),
        sourceState: (origin) =>
          Effect.gen(function* () {
            const now = yield* Clock.currentTimeMillis
            const value = yield* Ref.get(state)
            const entry = value.heartbeats.find((heartbeat) => heartbeat.origin === origin)
            if (entry === undefined) return "unknown" as const
            return heartbeatAt(entry, now, config.freshnessWindowMs).state
          }),
        heartbeat: (origin) =>
          Effect.gen(function* () {
            const now = yield* Clock.currentTimeMillis
            const value = yield* Ref.get(state)
            const entry = value.heartbeats.find((heartbeat) => heartbeat.origin === origin)
            return entry === undefined
              ? Option.none()
              : Option.some(heartbeatAt(entry, now, config.freshnessWindowMs))
          }),
      }
    }),
  )
