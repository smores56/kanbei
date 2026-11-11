import { Clock, Context, Data, Effect, Layer, Option, Ref, Result, Schema } from "effect"
import { canonicalize } from "../evidence/canonical.ts"
import type { Json } from "../evidence/canonical.ts"
import { sha256Labeled } from "../evidence/digest.ts"
import { LedgerStore } from "./ledger-store.ts"

export const AuthorityClass = Schema.Literals([
  "human_authority",
  "delegated_authority",
  "observation",
  "candidate_claim",
  "derived_proposal",
  "effect_receipt",
  "activation_receipt",
  "security_action",
  "coordination",
  "lifecycle",
  "kernel_generation",
])
export type AuthorityClass = typeof AuthorityClass.Type

export const AppenderRole = Schema.Literals(["frozen", "human_gesture", "agent", "plugin"])
export type AppenderRole = typeof AppenderRole.Type

export const LedgerComponent = Schema.Struct({
  type: Schema.String,
  payload: Schema.Json,
})
export type LedgerComponent = typeof LedgerComponent.Type

export const LedgerRecord = Schema.Struct({
  record_id: Schema.String,
  seq: Schema.Number,
  epoch_id: Schema.String,
  recorded_at_us: Schema.Number,
  prev_hash: Schema.String,
  record_hash: Schema.String,
  components: Schema.Array(LedgerComponent),
  idempotency_key: Schema.optional(Schema.String),
})
export type LedgerRecord = typeof LedgerRecord.Type

export interface LedgerHead {
  readonly seq: number
  readonly hash: string
  readonly epochId: string
}

export type LedgerErrorCode =
  | "authority_denied"
  | "component_invalid"
  | "epoch_mismatch"
  | "store_failed"
  | "tampered_row"
  | "hash_mismatch"
  | "sequence_gap"
  | "predecessor_mismatch"

export class LedgerError extends Data.TaggedError("LedgerError")<{
  readonly code: LedgerErrorCode
  readonly detail: string
}> {}

export interface LedgerAppendRequest {
  readonly components: ReadonlyArray<LedgerComponent>
  readonly appender: AppenderRole
  readonly principal: string
  readonly authorityClass?: AuthorityClass
  readonly idempotencyKey?: string
  readonly epochId?: string
}

export interface LedgerAppendResult {
  readonly record: LedgerRecord
  readonly duplicate: boolean
}

/** The frozen append surface: one sequencer, one chain, one authority matrix. */
export interface Ledger {
  readonly append: (request: LedgerAppendRequest) => Effect.Effect<LedgerAppendResult, LedgerError>
  readonly records: Effect.Effect<ReadonlyArray<LedgerRecord>, LedgerError>
  readonly head: Effect.Effect<LedgerHead, LedgerError>
  /** Re-reads and re-verifies every stored row; tampering after boot refuses here. */
  readonly verify: Effect.Effect<LedgerHead, LedgerError>
}

export const Ledger = Context.Service<Ledger>("kanbei/kernel/Ledger")

const AUTHORITY_COMPONENT = "kb.core/authority@1.0.0"

const allowedClasses: Readonly<Record<AppenderRole, ReadonlyArray<AuthorityClass>>> = {
  frozen: [
    "human_authority",
    "delegated_authority",
    "observation",
    "candidate_claim",
    "derived_proposal",
    "effect_receipt",
    "activation_receipt",
    "security_action",
    "coordination",
    "lifecycle",
    "kernel_generation",
  ],
  human_gesture: ["human_authority", "coordination"],
  agent: ["observation", "candidate_claim", "derived_proposal", "coordination"],
  plugin: ["observation", "candidate_claim", "derived_proposal", "coordination"],
}

const componentGrammar =
  /^kb\.[a-z][a-z0-9.-]*\/[A-Za-z0-9][A-Za-z0-9._-]*@[0-9]+\.[0-9]+\.[0-9]+$/u

const authorityComponent = (
  authorityClass: AuthorityClass,
  principal: string,
): LedgerComponent => ({
  type: AUTHORITY_COMPONENT,
  payload: { class: authorityClass, principal },
})

const componentIssues = (
  request: LedgerAppendRequest,
  components: ReadonlyArray<LedgerComponent>,
): ReadonlyArray<string> => {
  const issues: Array<string> = []
  const types = components.map((component) => component.type)
  if (new Set(types).size !== types.length) issues.push("duplicate component type")
  for (const component of components) {
    if (component.type === AUTHORITY_COMPONENT && request.authorityClass !== undefined) {
      issues.push("the append path materializes the authority component")
    }
    if (!componentGrammar.test(component.type)) {
      issues.push(`invalid component type ${component.type}`)
    }
    if (component.type.startsWith("kb.core/") && request.appender !== "frozen") {
      issues.push(`kb.core is frozen, not ${request.appender}`)
    }
    if (component.type.startsWith("kb.work/consent_display") && request.appender !== "frozen") {
      issues.push("the consent display is rendered only by the frozen surface")
    }
    if (
      (component.type.startsWith("kb.effect/") || component.type.startsWith("kb.activation/")) &&
      request.appender !== "frozen"
    ) {
      issues.push("effect and activation receipts are appended only by frozen components")
    }
    if (
      (component.type.startsWith("kb.memory/promotion") ||
        component.type.startsWith("kb.memory/attestation")) &&
      request.appender !== "human_gesture"
    ) {
      issues.push("promotion and attestation are human only")
    }
    if (
      (component.type.startsWith("kb.security/") || component.type.startsWith("kb.audit/")) &&
      request.appender !== "frozen"
    ) {
      issues.push("security and audit records are frozen only")
    }
  }
  if (request.principal.trim() === "") issues.push("principal is required")
  return issues
}

const authorityIssues = (request: LedgerAppendRequest): ReadonlyArray<string> => {
  if (request.authorityClass === undefined) return []
  return allowedClasses[request.appender].includes(request.authorityClass)
    ? []
    : [`${request.appender} cannot append ${request.authorityClass}`]
}

const compareTypes = (left: string, right: string): number =>
  left < right ? -1 : left > right ? 1 : 0

export const genesisHash = (epochId: string): Effect.Effect<string> =>
  sha256Labeled("kanbei.epoch.genesis.v1", [epochId, new Uint8Array(32)]).pipe(Effect.orDie)

interface RecordPreimage {
  readonly record_id: string
  readonly seq: number
  readonly epoch_id: string
  readonly recorded_at_us: number
  readonly prev_hash: string
  readonly components: ReadonlyArray<LedgerComponent>
}

const toJson = (value: RecordPreimage | Readonly<Record<string, unknown>>): Json =>
  Schema.decodeUnknownSync(Schema.Json)(value)

export const hashPreimage = (
  preimage: RecordPreimage,
): Effect.Effect<string> =>
  sha256Labeled("kanbei.record.v1", [canonicalize(toJson(preimage))]).pipe(Effect.orDie)

export const recordIdFor = (
  epochId: string,
  seq: number,
  components: ReadonlyArray<LedgerComponent>,
): Effect.Effect<string> =>
  sha256Labeled(
    "kanbei.recordid.v1",
    [canonicalize(toJson({ epoch_id: epochId, seq, components }))],
  ).pipe(Effect.orDie)

const parseLine = (line: string, index: number): Result.Result<LedgerRecord, LedgerError> => {
  try {
    const decoded = Schema.decodeUnknownSync(LedgerRecord, { onExcessProperty: "error" })(
      JSON.parse(line),
    )
    return Result.succeed(decoded)
  } catch (error) {
    return Result.fail(
      new LedgerError({
        code: "tampered_row",
        detail: `row ${index + 1}: ${error instanceof Error ? error.message : String(error)}`,
      }),
    )
  }
}

export const parseRows = (
  lines: ReadonlyArray<string>,
): Result.Result<ReadonlyArray<LedgerRecord>, LedgerError> => {
  const records: Array<LedgerRecord> = []
  for (const [index, line] of lines.entries()) {
    const parsed = parseLine(line, index)
    if (Result.isFailure(parsed)) return Result.fail(parsed.failure)
    records.push(parsed.success)
  }
  return Result.succeed(records)
}

/** Recomputes every hash and predecessor; any mismatch marks the ledger corrupt. */
export const verifyChain = (
  epochId: string,
  records: ReadonlyArray<LedgerRecord>,
  genesis: string,
): Effect.Effect<Result.Result<LedgerHead, LedgerError>> =>
  Effect.gen(function* () {
    let previous = genesis
    const seenHashes = new Set<string>()
    for (const [index, record] of records.entries()) {
      const expectedSeq = index + 1
      if (record.seq !== expectedSeq) {
        return Result.fail(
          new LedgerError({
            code: "sequence_gap",
            detail: `row ${index + 1} has seq ${record.seq}`,
          }),
        )
      }
      if (record.epoch_id !== epochId) {
        return Result.fail(
          new LedgerError({
            code: "epoch_mismatch",
            detail: `row ${record.seq} has another epoch`,
          }),
        )
      }
      if (record.prev_hash !== previous) {
        return Result.fail(
          new LedgerError({
            code: "predecessor_mismatch",
            detail: `row ${record.seq} does not link to its predecessor`,
          }),
        )
      }
      if (seenHashes.has(record.record_hash)) {
        return Result.fail(
          new LedgerError({ code: "hash_mismatch", detail: `row ${record.seq} repeats a hash` }),
        )
      }
      const { record_hash: storedHash, idempotency_key: _key, ...preimage } = record
      const recomputed = yield* hashPreimage(preimage)
      if (recomputed !== storedHash) {
        return Result.fail(
          new LedgerError({
            code: "hash_mismatch",
            detail: `row ${record.seq} hashes to ${recomputed}`,
          }),
        )
      }
      seenHashes.add(storedHash)
      previous = storedHash
    }
    return Result.succeed({
      seq: records.length,
      hash: previous,
      epochId,
    })
  })

interface LedgerState {
  readonly records: ReadonlyArray<LedgerRecord>
  readonly head: LedgerHead
  readonly integrity: Option.Option<LedgerError>
  readonly idempotency: Readonly<Record<string, LedgerRecord>>
  readonly recordedAtUs: number
}

const idempotencyIndex = (
  records: ReadonlyArray<LedgerRecord>,
): Readonly<Record<string, LedgerRecord>> =>
  Object.fromEntries(
    records.flatMap((record) =>
      record.idempotency_key === undefined ? [] : [[record.idempotency_key, record] as const]
    ),
  )

const emptyState = (head: LedgerHead): LedgerState => ({
  records: [],
  head,
  integrity: Option.none(),
  idempotency: {},
  recordedAtUs: 0,
})

const loadState = (
  epochId: string,
): Effect.Effect<LedgerState, never, LedgerStore> =>
  Effect.gen(function* () {
    const store = yield* LedgerStore
    const genesis = yield* genesisHash(epochId)
    const loaded = yield* Effect.result(store.read)
    if (Result.isFailure(loaded)) {
      return {
        ...emptyState({ seq: 0, hash: genesis, epochId }),
        integrity: Option.some(
          new LedgerError({ code: "store_failed", detail: loaded.failure.reason }),
        ),
      }
    }
    const parsed = parseRows(loaded.success)
    if (Result.isFailure(parsed)) {
      return {
        ...emptyState({ seq: 0, hash: genesis, epochId }),
        integrity: Option.some(parsed.failure),
      }
    }
    const verified = yield* verifyChain(epochId, parsed.success, genesis)
    if (Result.isFailure(verified)) {
      return {
        ...emptyState({ seq: 0, hash: genesis, epochId }),
        integrity: Option.some(verified.failure),
      }
    }
    const last = parsed.success.at(-1)
    return {
      records: parsed.success,
      head: verified.success,
      integrity: Option.none(),
      idempotency: idempotencyIndex(parsed.success),
      recordedAtUs: last === undefined ? 0 : last.recorded_at_us,
    }
  })

const refuseIntegrity = (
  state: LedgerState,
): Effect.Effect<ReadonlyArray<LedgerRecord>, LedgerError> =>
  Option.match(state.integrity, {
    onNone: () => Effect.succeed(state.records),
    onSome: (error) => Effect.fail(error),
  })

export const ledgerLayer = (epochId: string): Layer.Layer<Ledger, never, LedgerStore> =>
  Layer.effect(
    Ledger,
    Effect.gen(function* () {
      const store = yield* LedgerStore
      const genesis = yield* genesisHash(epochId)
      const stateRef = yield* Ref.make(yield* loadState(epochId))

      const append: Ledger["append"] = (request) =>
        Effect.gen(function* () {
          const state = yield* Ref.get(stateRef)
          if (Option.isSome(state.integrity)) return yield* Effect.fail(state.integrity.value)
          if (request.epochId !== undefined && request.epochId !== epochId) {
            return yield* Effect.fail(
              new LedgerError({ code: "epoch_mismatch", detail: request.epochId }),
            )
          }
          const supplied = request.components.map((component) => ({
            type: component.type,
            payload: Schema.decodeUnknownSync(Schema.Json)(component.payload),
          }))
          const issues = [...componentIssues(request, supplied), ...authorityIssues(request)]
          if (issues.length > 0) {
            return yield* Effect.fail(
              new LedgerError({ code: "authority_denied", detail: issues.join("; ") }),
            )
          }
          if (request.idempotencyKey !== undefined) {
            const duplicate = state.idempotency[request.idempotencyKey]
            if (duplicate !== undefined) return { record: duplicate, duplicate: true }
          }
          const components = [
            ...supplied,
            ...(request.authorityClass === undefined
              ? []
              : [authorityComponent(request.authorityClass, request.principal)]),
          ].toSorted((left, right) => compareTypes(left.type, right.type))
          const nowMillis = yield* Clock.currentTimeMillis
          const recordedAtUs = Math.max(state.recordedAtUs, Math.floor(nowMillis * 1000))
          const seq = state.head.seq + 1
          const record_id = yield* recordIdFor(epochId, seq, components)
          const prev_hash = state.head.hash
          const record_hash = yield* hashPreimage({
            record_id,
            seq,
            epoch_id: epochId,
            recorded_at_us: recordedAtUs,
            prev_hash,
            components,
          })
          const record: LedgerRecord = {
            record_id,
            seq,
            epoch_id: epochId,
            recorded_at_us: recordedAtUs,
            prev_hash,
            record_hash,
            components,
            ...(request.idempotencyKey === undefined
              ? {}
              : { idempotency_key: request.idempotencyKey }),
          }
          const stored = yield* Effect.result(store.append(JSON.stringify(record)))
          if (Result.isFailure(stored)) {
            return yield* Effect.fail(
              new LedgerError({ code: "store_failed", detail: stored.failure.reason }),
            )
          }
          yield* Ref.set(stateRef, {
            records: [...state.records, record],
            head: { seq, hash: record_hash, epochId },
            integrity: Option.none(),
            idempotency: request.idempotencyKey === undefined
              ? state.idempotency
              : { ...state.idempotency, [request.idempotencyKey]: record },
            recordedAtUs,
          })
          return { record, duplicate: false }
        })

      const verify: Ledger["verify"] = Effect.gen(function* () {
        const loaded = yield* Effect.result(store.read)
        if (Result.isFailure(loaded)) {
          return yield* Effect.fail(
            new LedgerError({ code: "store_failed", detail: loaded.failure.reason }),
          )
        }
        const parsed = parseRows(loaded.success)
        if (Result.isFailure(parsed)) return yield* Effect.fail(parsed.failure)
        const verified = yield* verifyChain(epochId, parsed.success, genesis)
        if (Result.isFailure(verified)) return yield* Effect.fail(verified.failure)
        return verified.success
      })

      return {
        append,
        records: Ref.get(stateRef).pipe(Effect.flatMap(refuseIntegrity)),
        head: Ref.get(stateRef).pipe(
          Effect.flatMap((state) =>
            Option.match(state.integrity, {
              onNone: () => Effect.succeed(state.head),
              onSome: (error) => Effect.fail(error),
            })
          ),
        ),
        verify,
      }
    }),
  )

export const authorityFor = (record: LedgerRecord): Option.Option<Json> => {
  const found = record.components.find((component) => component.type === AUTHORITY_COMPONENT)
  return found === undefined ? Option.none() : Option.some(found.payload)
}
