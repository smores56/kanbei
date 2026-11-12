import { Schema } from "effect"

export const DecisionGate = Schema.Literals([
  "DG-1",
  "DG-2",
  "DG-3",
  "DG-4",
  "DG-5",
  "DG-6",
  "DG-7",
  "DG-8",
])
export type DecisionGate = typeof DecisionGate.Type

export const GateStatus = Schema.Literals(["open", "closed", "blocked"])
export type GateStatus = typeof GateStatus.Type

export const GateRecord = Schema.Struct({
  gate: DecisionGate,
  decision: Schema.String,
  owner: Schema.Literal("human"),
  producingTask: Schema.String,
  blocks: Schema.Array(Schema.String),
  status: GateStatus,
  recommended: Schema.optional(Schema.String),
  recommendationRationale: Schema.Array(Schema.String),
  conditions: Schema.Array(Schema.String),
  blockers: Schema.Array(Schema.String),
  selected: Schema.optional(Schema.String),
  rejectedAlternatives: Schema.Array(Schema.String),
  evidenceEnvelopes: Schema.Array(Schema.String),
  evidenceArtifacts: Schema.Array(Schema.String),
  residuals: Schema.Array(Schema.String),
  signedBy: Schema.optional(Schema.String),
  signedAt: Schema.optional(Schema.String),
})
export type GateRecord = typeof GateRecord.Type

export const openGate = (
  gate: DecisionGate,
  decision: string,
  producingTask: string,
): GateRecord => ({
  gate,
  decision,
  owner: "human",
  producingTask,
  blocks: [],
  status: "open",
  recommendationRationale: [],
  conditions: [],
  blockers: [],
  rejectedAlternatives: [],
  evidenceEnvelopes: [],
  evidenceArtifacts: [],
  residuals: [],
})
