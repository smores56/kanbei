import { Effect, Schema } from "effect"
import type { Ledger, LedgerComponent, LedgerError, LedgerRecord } from "../kernel/ledger.ts"
import { Ledger as LedgerTag } from "../kernel/ledger.ts"

export const MEMORY_CLAIM_COMPONENT = "kb.memory/claim@1.0.0"
export const MEMORY_RETRACTION_COMPONENT = "kb.memory/retraction@1.0.0"

export interface ClaimInput {
  readonly claimId: string
  readonly subject: string
  readonly value: string
  readonly confidence: number
  readonly source: string
}

export interface RetractionInput {
  readonly targetClaimId: string
  readonly reason: string
}

export type MemoryState = "active" | "contested" | "suspect"

export interface MemoryClaim {
  readonly claimId: string
  readonly subject: string
  readonly value: string
  readonly state: MemoryState
}

const ClaimPayload = Schema.Struct({
  claimId: Schema.String,
  subject: Schema.String,
  value: Schema.String,
  confidence: Schema.Number,
  source: Schema.String,
})

const RetractionPayload = Schema.Struct({
  targetClaimId: Schema.String,
  reason: Schema.String,
})

const claimComponent = (input: ClaimInput): LedgerComponent => ({
  type: MEMORY_CLAIM_COMPONENT,
  payload: { ...input },
})

const retractionComponent = (input: RetractionInput): LedgerComponent => ({
  type: MEMORY_RETRACTION_COMPONENT,
  payload: { ...input },
})

/** Claims and retractions are agent observations; promotions stay human-only. */
export const appendMemoryEvents = (
  claims: ReadonlyArray<ClaimInput>,
  retractions: ReadonlyArray<RetractionInput>,
): Effect.Effect<void, LedgerError, Ledger> =>
  Effect.gen(function* () {
    const ledger = yield* LedgerTag
    yield* Effect.forEach(
      claims,
      (claim) =>
        ledger.append({
          components: [claimComponent(claim)],
          appender: "agent",
          principal: "journey-v2",
          authorityClass: "candidate_claim",
          idempotencyKey: `journey-v2:claim:${claim.claimId}`,
        }),
      { concurrency: 1 },
    )
    yield* Effect.forEach(
      retractions,
      (retraction) =>
        ledger.append({
          components: [retractionComponent(retraction)],
          appender: "agent",
          principal: "journey-v2",
          authorityClass: "observation",
          idempotencyKey: `journey-v2:retraction:${retraction.targetClaimId}`,
        }),
      { concurrency: 1 },
    )
  })

interface RawClaim {
  readonly claimId: string
  readonly subject: string
  readonly value: string
}

const decode = <A>(schema: Schema.ConstraintDecoder<A>, value: unknown): A | undefined => {
  try {
    return Schema.decodeUnknownSync(schema)(value)
  } catch {
    return undefined
  }
}

/**
 * A read-time projection: a retracted claim is `suspect` (never current), and a
 * subject with two different values is `contested` while alternatives stay
 * visible. Nothing here rewrites canonical records.
 */
export const readMemory = (
  records: ReadonlyArray<LedgerRecord>,
): ReadonlyArray<MemoryClaim> => {
  const claims = new Map<string, RawClaim>()
  const retracted = new Set<string>()
  for (const record of records) {
    for (const component of record.components) {
      if (component.type === MEMORY_CLAIM_COMPONENT) {
        const payload = decode(ClaimPayload, component.payload)
        if (payload !== undefined) {
          claims.set(payload.claimId, {
            claimId: payload.claimId,
            subject: payload.subject,
            value: payload.value,
          })
        }
      }
      if (component.type === MEMORY_RETRACTION_COMPONENT) {
        const payload = decode(RetractionPayload, component.payload)
        if (payload !== undefined) retracted.add(payload.targetClaimId)
      }
    }
  }
  const active = [...claims.values()].filter((claim) => !retracted.has(claim.claimId))
  return [...claims.values()]
    .map((claim): MemoryClaim => {
      if (retracted.has(claim.claimId)) {
        return {
          claimId: claim.claimId,
          subject: claim.subject,
          value: claim.value,
          state: "suspect",
        }
      }
      const contested = active.some((other) =>
        other.claimId !== claim.claimId &&
        other.subject === claim.subject &&
        other.value !== claim.value
      )
      return {
        claimId: claim.claimId,
        subject: claim.subject,
        value: claim.value,
        state: contested ? "contested" : "active",
      }
    })
    .toSorted((left, right) => left.claimId.localeCompare(right.claimId))
}
