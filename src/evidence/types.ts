import { Schema } from "effect"

export const TestClass = Schema.Literals([
  "unit",
  "property",
  "conformance",
  "chaos",
  "redteam",
  "spike",
  "bench",
])
export type TestClass = typeof TestClass.Type

export const Outcome = Schema.Literals(["pass", "fail", "inconclusive", "blocked"])
export type Outcome = typeof Outcome.Type

export const EnvironmentFingerprint = Schema.Struct({
  hostFingerprint: Schema.String,
  kernel: Schema.String,
  runtime: Schema.String,
  sandboxPosture: Schema.String,
  brokerIdentity: Schema.String,
})
export type EnvironmentFingerprint = typeof EnvironmentFingerprint.Type

export const CanonicalHead = Schema.Struct({
  seq: Schema.Number,
  hash: Schema.String,
})
export type CanonicalHead = typeof CanonicalHead.Type

export const EvidenceEnvelope = Schema.Struct({
  testId: Schema.String,
  invariantIds: Schema.Array(Schema.String),
  fixtureVersion: Schema.String,
  canonicalHead: CanonicalHead,
  sequenceRange: Schema.Tuple([Schema.Number, Schema.Number]),
  environment: EnvironmentFingerprint,
  seeds: Schema.Array(Schema.Number),
  modelRecordings: Schema.Array(Schema.String),
  artifacts: Schema.Array(Schema.String),
  outcome: Outcome,
  failureClass: Schema.optional(Schema.String),
  metrics: Schema.optional(Schema.Record(Schema.String, Schema.Number)),
  acceptedResiduals: Schema.optional(Schema.Array(Schema.String)),
})
export type EvidenceEnvelope = typeof EvidenceEnvelope.Type

export interface FixtureReference {
  readonly id: string
  readonly version: string
  readonly canonicalHead: CanonicalHead
  readonly sequenceRange: readonly [number, number]
}

export const fixtureVersion = (reference: FixtureReference): string =>
  `${reference.id}@${reference.version}`
