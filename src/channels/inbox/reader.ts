import { Clock, Context, Data, Effect, Layer, Option, Ref, Result, Schema } from "effect"
import { canonicalize } from "../../evidence/canonical.ts"
import { sha256, sha256Text } from "../../evidence/digest.ts"
import type { ContentDigest } from "../../kernel/blob-sink.ts"
import { BlobSink } from "../../kernel/blob-sink.ts"
import { AgeDecryptor } from "./age.ts"
import { InboxDirectory } from "./directory.ts"
import { IngestBatchEnvelope } from "./schema.ts"
import type { IngestBatchEnvelope as BatchEnvelope, OriginId, ScanCoverage } from "./schema.ts"

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

export interface Observation {
  readonly origin: OriginId
  readonly sequence: number
  readonly batchDigest: ContentDigest
  readonly resourceKey: string
  readonly kind: string
  readonly locator: string
  readonly capturedAt: string
  readonly digest: ContentDigest
  readonly bytes: number
  readonly mediaType?: string
}

export interface CoverageReceipt {
  readonly origin: OriginId
  readonly batchDigest: ContentDigest
  readonly sequence: number
  readonly receivedAt: string
  readonly entries: number
  readonly publishedBytes: number
  readonly coverage: ScanCoverage
}

export interface GapRecord {
  readonly origin: OriginId
  readonly kind: "gap" | "out_of_order" | "suppression"
  readonly missing: number
  readonly sequence: number
  readonly detail: string
}

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

export interface SourceHeartbeat {
  readonly origin: OriginId
  readonly sequence: number
  readonly producedAt: string
  readonly receivedAt: string
  readonly coverage: ScanCoverage
  readonly state: "current" | "stale" | "unknown"
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
  readonly operation: "list"
  readonly reason: string
}> {}

export interface InboxReader {
  readonly poll: Effect.Effect<PollResult, InboxError>
  readonly state: Effect.Effect<InboxSnapshot>
  readonly sourceState: (
    origin: OriginId,
  ) => Effect.Effect<"current" | "stale" | "unknown">
  readonly heartbeat: (origin: OriginId) => Effect.Effect<Option.Option<SourceHeartbeat>>
}

export const InboxReader = Context.Service<InboxReader>("kanbei/channel/inbox/Reader")

interface HeartbeatEntry {
  readonly origin: OriginId
  readonly sequence: number
  readonly producedAt: string
  readonly receivedAt: string
  readonly coverage: ScanCoverage
}

interface InboxState {
  readonly observations: ReadonlyArray<Observation>
  readonly receipts: ReadonlyArray<CoverageReceipt>
  readonly heartbeats: ReadonlyArray<HeartbeatEntry>
  readonly gaps: ReadonlyArray<GapRecord>
  readonly quarantined: ReadonlyArray<QuarantineRecord>
  readonly securityEvents: ReadonlyArray<SecurityEvent>
  readonly seenBatches: ReadonlySet<string>
  readonly seenItems: ReadonlySet<string>
  readonly lastSequence: Readonly<Record<string, number>>
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
  lastSequence: {},
  duplicates: 0,
}

interface InvalidBatch {
  readonly reason: QuarantineReason
  readonly detail: string
}

const batchKey = (origin: OriginId, digest: ContentDigest): string => `${origin}|${digest}`
const itemKey = (origin: OriginId, batch: ContentDigest, digest: ContentDigest): string =>
  `${origin}|${batch}|${digest}`

const decodeBase64 = (value: string): Result.Result<Uint8Array, string> => {
  try {
    return Result.succeed(Uint8Array.fromBase64(value))
  } catch (error) {
    return Result.fail(error instanceof Error ? error.message : String(error))
  }
}

/** Strict: unknown fields, an unknown version, or a bad manifest is a parse failure. */
export const parseEnvelope = (
  plaintext: Uint8Array,
): Result.Result<BatchEnvelope, InvalidBatch> => {
  try {
    const json = JSON.parse(new TextDecoder().decode(plaintext))
    const decoded = Schema.decodeUnknownSync(IngestBatchEnvelope, {
      onExcessProperty: "error",
    })(json)
    return Result.succeed(decoded)
  } catch (error) {
    return Result.fail({
      reason: "parse_failed",
      detail: error instanceof Error ? error.message.slice(0, 300) : String(error),
    })
  }
}

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

const heartbeatAt = (entry: HeartbeatEntry, now: number, window: number): SourceHeartbeat => ({
  ...entry,
  state: now - Date.parse(entry.receivedAt) > window ? "stale" : "current",
})

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
): Layer.Layer<InboxReader, never, InboxDirectory | AgeDecryptor | BlobSink> =>
  Layer.effect(
    InboxReader,
    Effect.gen(function* () {
      const issues = configIssues(config)
      if (issues.length > 0) {
        return yield* Effect.die(new Error(`invalid inbox config: ${issues.join("; ")}`))
      }
      const directory = yield* InboxDirectory
      const decryptor = yield* AgeDecryptor
      const sink = yield* BlobSink
      const state = yield* Ref.make<InboxState>(emptyState)

      const quarantine = (
        name: string,
        reason: QuarantineReason,
        detail: string,
        ciphertextDigest?: ContentDigest,
      ): Effect.Effect<void> =>
        Ref.update(state, (current) => ({
          ...current,
          quarantined: [
            ...current.quarantined,
            {
              name,
              reason,
              detail,
              ...(ciphertextDigest === undefined ? {} : { ciphertextDigest }),
            },
          ],
        }))

      const processFile = (
        name: string,
        size: number,
      ): Effect.Effect<"accepted" | "quarantined" | "duplicate", never> =>
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
          const parsed = parseEnvelope(plaintext)
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
          if (!Number.isInteger(manifest.sequence) || manifest.sequence < 1) {
            yield* quarantine(name, "invalid_sequence", `sequence ${manifest.sequence}`)
            return "quarantined" as const
          }

          const current = yield* Ref.get(state)
          const key = batchKey(manifest.origin, manifest.batchDigest)
          if (current.seenBatches.has(key)) {
            yield* Ref.update(state, (value) => ({ ...value, duplicates: value.duplicates + 1 }))
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
            yield* Ref.update(state, (value) => ({
              ...value,
              securityEvents: [
                ...value.securityEvents,
                {
                  kind: "credential_detected" as const,
                  origin: manifest.origin,
                  name,
                  detail: findings.join(","),
                },
              ],
            }))
            yield* quarantine(name, "credential_detected", findings.join(","), ciphertextDigest)
            return "quarantined" as const
          }

          const published = yield* Effect.forEach(
            [...blobs.entries()],
            ([digest, bytes]) =>
              sink.put(bytes).pipe(Effect.map((value) => [digest, value] as const)),
            { concurrency: 1 },
          ).pipe(Effect.result)
          if (Result.isFailure(published)) {
            yield* quarantine(name, "publish_failed", published.failure.reason, ciphertextDigest)
            return "quarantined" as const
          }
          const mismatched = published.success.filter(([digest, stored]) => digest !== stored)
          if (mismatched.length > 0) {
            yield* quarantine(name, "digest_mismatch", "blob sink returned a different digest")
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
            const item = itemKey(manifest.origin, manifest.batchDigest, entry.body)
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
          yield* Ref.update(state, (value) => ({
            ...value,
            observations: [...value.observations, ...created],
            receipts: [...value.receipts, receipt],
            heartbeats: [
              ...value.heartbeats.filter((entry) => entry.origin !== manifest.origin),
              heartbeat,
            ],
            gaps: [...value.gaps, ...batchGaps],
            seenBatches: new Set([...value.seenBatches, key]),
            seenItems,
            duplicates: value.duplicates + duplicateItems,
            lastSequence: { ...value.lastSequence, [manifest.origin]: delta.next },
          }))
          return "accepted" as const
        })

      const poll: InboxReader["poll"] = Effect.gen(function* () {
        const listing = yield* directory.list.pipe(
          Effect.mapError((error) => new InboxError({ operation: "list", reason: error.reason })),
        )
        const names = listing
          .filter((file) => file.name.endsWith(".age"))
          .toSorted((left, right) => left.name.localeCompare(right.name))
          .slice(0, config.limits.maxBatchesPerPoll)
        const outcomes = yield* Effect.forEach(
          names,
          (file) => processFile(file.name, file.size),
          { concurrency: 1 },
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
