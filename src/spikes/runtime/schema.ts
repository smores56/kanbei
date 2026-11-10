import { Schema } from "effect"
import { EvidenceEnvelope } from "../../evidence/types.ts"

export const RuntimeName = Schema.Literals(["node", "deno"])
export type RuntimeName = typeof RuntimeName.Type

export const Summary = Schema.Struct({
  count: Schema.Number,
  min: Schema.Number,
  max: Schema.Number,
  mean: Schema.Number,
  p50: Schema.Number,
  p95: Schema.Number,
})
export type Summary = typeof Summary.Type

export const PreRegistration = Schema.Struct({
  protocol: Schema.Literal("S1"),
  decisionId: Schema.Literal("NODE-DENO-1"),
  dataset: Schema.Struct({ id: Schema.String, version: Schema.String }),
  arms: Schema.Struct({ node: Schema.String, deno: Schema.String }),
  sample: Schema.Struct({
    coldStartRuns: Schema.Number,
    warmStartIterations: Schema.Number,
    loopSamplerIntervalMs: Schema.Number,
    workerRuns: Schema.Number,
  }),
  load: Schema.Struct({
    appends: Schema.Number,
    appendChunk: Schema.Number,
    readsPerChunk: Schema.Number,
  }),
  hardFails: Schema.Array(
    Schema.Struct({ id: Schema.String, condition: Schema.String, rationale: Schema.String }),
  ),
  compositeWeights: Schema.Array(
    Schema.Struct({ id: Schema.String, weight: Schema.Number, rationale: Schema.String }),
  ),
  thresholds: Schema.Struct({
    eventLoopP95Ms: Schema.Number,
    appendP95Ms: Schema.Number,
    shutdownMs: Schema.Number,
  }),
  permissionExpectations: Schema.Struct({
    node: Schema.Array(Schema.String),
    deno: Schema.Array(Schema.String),
  }),
  decisionRule: Schema.String,
  nonInferiorityMargin: Schema.Number,
  decisionMargin: Schema.Number,
  tieBreakers: Schema.Array(Schema.String),
  frozenRevision: Schema.String,
})
export type PreRegistration = typeof PreRegistration.Type

export const CorrectnessCheck = Schema.Struct({
  id: Schema.String,
  pass: Schema.Boolean,
  detail: Schema.String,
})
export type CorrectnessCheck = typeof CorrectnessCheck.Type

export const PermissionProbe = Schema.Struct({
  id: Schema.String,
  denied: Schema.Boolean,
  code: Schema.String,
  model: Schema.String,
})
export type PermissionProbe = typeof PermissionProbe.Type

export const ArmMeasurements = Schema.Struct({
  runtime: RuntimeName,
  runtimeVersion: Schema.String,
  sqliteVersion: Schema.String,
  boot: Schema.Struct({ coldReady: Summary, coldStart: Summary, warmStart: Summary }),
  memory: Schema.Struct({
    rssAfterBoot: Summary,
    rssAfterLoadBytes: Schema.Number,
    peakRssBytes: Schema.Number,
  }),
  eventLoop: Summary,
  ledger: Schema.Struct({
    appends: Schema.Number,
    appendsPerSecond: Schema.Number,
    append: Summary,
    read: Summary,
    walMode: Schema.String,
    readYourWrites: Schema.Boolean,
    hashChainValid: Schema.Boolean,
  }),
  correctness: Schema.Array(CorrectnessCheck),
  singleWriter: Schema.Struct({
    concurrentWriterCommitted: Schema.Boolean,
    failClosed: Schema.Boolean,
    recoveryOk: Schema.Boolean,
    busyBlockedEventLoop: Schema.Boolean,
    busyBlockMs: Schema.Number,
  }),
  isolation: Schema.Struct({
    workerStartupMs: Schema.Number,
    roundtrip: Summary,
    crashObserved: Schema.Boolean,
    parentSurvivedCrash: Schema.Boolean,
  }),
})
export type ArmMeasurements = typeof ArmMeasurements.Type

export const ArmResult = Schema.Struct({
  ...ArmMeasurements.fields,
  signals: Schema.Struct({
    handlerInvoked: Schema.Boolean,
    shutdownMs: Schema.Number,
    exitCode: Schema.Number,
  }),
  permissions: Schema.Array(PermissionProbe),
  packaging: Schema.Struct({
    runtimeBinaryBytes: Schema.Number,
    singleBinaryBytes: Schema.Number,
    singleBinaryStatus: Schema.Literals(["measured", "blocked"]),
    singleBinaryNote: Schema.String,
  }),
})
export type ArmResult = typeof ArmResult.Type

export const Measurement = Schema.Struct({
  id: Schema.String,
  label: Schema.String,
  unit: Schema.String,
  direction: Schema.Literals(["lower", "higher"]),
  node: Schema.Number,
  deno: Schema.Number,
  ratio: Schema.Number,
  winner: Schema.Literals(["node", "deno", "tie"]),
})
export type Measurement = typeof Measurement.Type

export const HardFail = Schema.Struct({
  id: Schema.String,
  runtime: RuntimeName,
  detail: Schema.String,
})
export type HardFail = typeof HardFail.Type

export const Decision = Schema.Struct({
  value: Schema.Literals(["node", "deno", "inconclusive"]),
  rule: Schema.String,
  rationale: Schema.Array(Schema.String),
})
export type Decision = typeof Decision.Type

export const SpikeResultDocument = Schema.Struct({
  schemaVersion: Schema.Literal(1),
  protocol: Schema.Literal("S1"),
  decisionId: Schema.Literal("NODE-DENO-1"),
  preRegistration: PreRegistration,
  environment: Schema.Struct({
    hostFingerprint: Schema.String,
    kernel: Schema.String,
    runtime: Schema.String,
    sandboxPosture: Schema.String,
    brokerIdentity: Schema.String,
  }),
  arms: Schema.Struct({ node: ArmResult, deno: ArmResult }),
  measurements: Schema.Array(Measurement),
  hardFails: Schema.Array(HardFail),
  composite: Schema.Struct({ node: Schema.Number, deno: Schema.Number, margin: Schema.Number }),
  decision: Decision,
  rejectedAlternative: Schema.Struct({
    value: Schema.Literals(["node", "deno", "none"]),
    reasons: Schema.Array(Schema.String),
  }),
  residualRisks: Schema.Array(Schema.String),
  blocked: Schema.Array(
    Schema.Struct({ arm: RuntimeName, measurement: Schema.String, reason: Schema.String }),
  ),
  unboundProbes: Schema.Array(Schema.String),
  envelope: EvidenceEnvelope,
  /** Present when the permission/fail-closed probes were measured. Additive to v1. */
  securityEnvelope: Schema.optional(EvidenceEnvelope),
})
export type SpikeResultDocument = typeof SpikeResultDocument.Type
