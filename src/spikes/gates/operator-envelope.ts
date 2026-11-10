import { Schema } from "effect"

export const BudgetWindow = Schema.Literals([
  "rolling_1h",
  "calendar_day",
  "calendar_month",
])

export const HardCeilings = Schema.Struct({
  generation: Schema.String,
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
  spendUsd: Schema.Number,
  wallClockMs: Schema.Number,
  modelCalls: Schema.Number,
  effectCalls: Schema.Number,
  decisionRequests: Schema.Number,
  decisionsConsumed: Schema.Number,
  maxOverrideMultiplier: Schema.Number,
})
export type LoopBudget = typeof LoopBudget.Type

export const OperatorEnvelope = Schema.Struct({
  hardCeilings: HardCeilings,
  defaultLoopBudget: LoopBudget,
})
export type OperatorEnvelope = typeof OperatorEnvelope.Type
