import { Schema } from "effect"

export const BudgetWindow = Schema.Literals([
  "rolling_1h",
  "calendar_day",
  "calendar_month",
])

export const NixGenerationId = Schema.String

export const HardCeilings = Schema.Struct({
  generation: NixGenerationId,
  window: BudgetWindow,
  spendUsd: Schema.Number,
  tokensIn: Schema.Number,
  tokensOut: Schema.Number,
  modelRequestsPerWindow: Schema.Number,
  brokerRequestsPerWindow: Schema.Number,
  concurrentLoops: Schema.Number,
})
export type HardCeilings = typeof HardCeilings.Type

export const LoopBudget = Schema.Struct({
  tokens: Schema.Number,
  spend_usd: Schema.Number,
  wall_clock: Schema.Duration,
  model_calls: Schema.Number,
  effect_calls: Schema.Number,
  decision_requests: Schema.Number,
  decisions_consumed: Schema.Number,
  max_override_multiplier: Schema.Number,
})
export type LoopBudget = typeof LoopBudget.Type

export const BudgetOverride = Schema.Struct({
  id: Schema.String,
  loop_id: Schema.String,
  rationale: Schema.String,
  delta: Schema.Struct({
    tokens: Schema.optional(Schema.Number),
    spend_usd: Schema.optional(Schema.Number),
    wall_clock: Schema.optional(Schema.Duration),
    effect_calls: Schema.optional(Schema.Number),
  }),
  expires_at_us: Schema.Number,
  author: Schema.String,
  authority_class: Schema.Literal("candidate_claim"),
})
export type BudgetOverride = typeof BudgetOverride.Type

export const OperatorEnvelope = Schema.Struct({
  hardCeilings: HardCeilings,
  defaultLoopBudget: LoopBudget,
})
export type OperatorEnvelope = typeof OperatorEnvelope.Type
