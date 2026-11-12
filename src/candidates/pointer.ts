import { Context, Data, Schema } from "effect"
import type { Effect, Option } from "effect"

export const PointerTarget = Schema.Struct({
  candidateId: Schema.String,
  stateGeneration: Schema.Number,
})

export const ScopePointer = Schema.Struct({
  scope: Schema.String,
  generation: Schema.Number,
  candidateId: Schema.String,
  candidateDigest: Schema.String,
  stateGeneration: Schema.Number,
  activatedAtUs: Schema.Number,
  activationReceipt: Schema.String,
  prev: Schema.optional(PointerTarget),
})
export type ScopePointer = typeof ScopePointer.Type

export class PointerError extends Data.TaggedError("PointerError")<{
  readonly operation: "read" | "publish"
  readonly scope: string
  readonly reason: string
}> {}

/**
 * The trust-root-owned activation pointer, one per scope. The plugin cannot
 * write it; publication is write-temp, fsync, rename (`07 §4.8.4`).
 */
export interface PointerStore {
  readonly read: (scope: string) => Effect.Effect<Option.Option<ScopePointer>, PointerError>
  readonly publish: (pointer: ScopePointer) => Effect.Effect<void, PointerError>
  readonly clear: (scope: string) => Effect.Effect<void, PointerError>
}

export const PointerStore = Context.Service<PointerStore>("kanbei/candidates/PointerStore")
