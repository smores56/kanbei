import { Schema } from "effect"

export const ARM_IDS = ["monty", "typescript", "lua", "bash", "cpython"] as const
export const ArmId = Schema.Literals(ARM_IDS)
export type ArmId = typeof ArmId.Type

export const RuntimeVariantId = Schema.Literals([
  "monty",
  "deno",
  "node",
  "lua5.4",
  "luau",
  "bash",
  "cpython",
])
export type RuntimeVariantId = typeof RuntimeVariantId.Type

export const LATENCY_PROFILES_MS = [10, 100, 1000] as const
export type LatencyProfileMs = (typeof LATENCY_PROFILES_MS)[number]
export const LatencyProfileMs = Schema.Literals([10, 100, 1000])

export const TRIALS_PER_CELL = 30
export const TEMPERATURE = 0

export const TaskCategory = Schema.Literals([
  "fan-out-read",
  "conditional-multi-step",
  "large-output-reduction",
  "file-transformation",
  "error-recovery",
  "approval-gate",
  "fork-branch",
  "streaming-progress",
  "dependency-heavy",
  "adversarial",
])
export type TaskCategory = typeof TaskCategory.Type

export const RubricCriterion = Schema.Struct({
  id: Schema.String,
  criterion: Schema.String,
})
export type RubricCriterion = typeof RubricCriterion.Type

export const TaskSpec = Schema.Struct({
  id: Schema.String,
  ordinal: Schema.Number,
  title: Schema.String,
  category: TaskCategory,
  description: Schema.String,
  hostCallPattern: Schema.String,
  goldOutputJson: Schema.String,
  rubric: Schema.Array(RubricCriterion),
  requiredCapabilities: Schema.Array(Schema.String),
  adversarial: Schema.Boolean,
  secretCanary: Schema.optional(Schema.String),
})
export type TaskSpec = typeof TaskSpec.Type

export const RuntimeResolution = Schema.Union([
  Schema.Struct({
    status: Schema.Literal("available"),
    variant: RuntimeVariantId,
    binaryPath: Schema.String,
    version: Schema.String,
    source: Schema.Literals(["path", "nixpkgs"]),
    env: Schema.Record(Schema.String, Schema.String),
  }),
  Schema.Struct({
    status: Schema.Literal("unavailable"),
    variant: RuntimeVariantId,
    reason: Schema.String,
    missingInput: Schema.String,
    log: Schema.Array(Schema.String),
  }),
])
export type RuntimeResolution = typeof RuntimeResolution.Type

/** One observable attempt at a capability. `allowed` is false when the runtime denied it. */
export const CapabilityProbe = Schema.Struct({
  attempted: Schema.Boolean,
  allowed: Schema.Boolean,
  errorClass: Schema.optional(Schema.String),
  detail: Schema.optional(Schema.String),
})
export type CapabilityProbe = typeof CapabilityProbe.Type

export const FanoutResult = Schema.Struct({
  ok: Schema.Boolean,
  calls: Schema.Number,
  checksum: Schema.Number,
  groups: Schema.Array(Schema.Tuple([Schema.String, Schema.Number])),
  top: Schema.Array(Schema.Tuple([Schema.Number, Schema.Number])),
  callOverheadsMs: Schema.Array(Schema.Number),
  totalMs: Schema.Number,
})
export type FanoutResult = typeof FanoutResult.Type

export const ProbeOutput = Schema.Struct({
  probeVersion: Schema.Literal(1),
  expression: Schema.Struct({ ok: Schema.Boolean, value: Schema.Number }),
  fanout: FanoutResult,
  stream: Schema.Struct({ events: Schema.Number }),
  retry: Schema.Struct({ attempts: Schema.Number, ok: Schema.Boolean }),
  fileWrite: CapabilityProbe,
  envRead: CapabilityProbe,
  processSpawn: CapabilityProbe,
  network: CapabilityProbe,
  secretFileRead: CapabilityProbe,
  dependencyImport: Schema.Struct({
    ok: Schema.Boolean,
    modules: Schema.Array(Schema.Struct({ name: Schema.String, ok: Schema.Boolean })),
  }),
  snapshot: Schema.Struct({
    supported: Schema.Boolean,
    ok: Schema.Boolean,
    detail: Schema.optional(Schema.String),
  }),
  approvalPause: Schema.Struct({
    supported: Schema.Boolean,
    ok: Schema.Boolean,
    detail: Schema.optional(Schema.String),
  }),
})
export type ProbeOutput = typeof ProbeOutput.Type

export const StartupMeasurement = Schema.Struct({
  runs: Schema.Number,
  p50Ms: Schema.Number,
  p95Ms: Schema.Number,
  minMs: Schema.Number,
  maxMs: Schema.Number,
})
export type StartupMeasurement = typeof StartupMeasurement.Type

export const RuntimeMeasurement = Schema.Struct({
  arm: ArmId,
  variant: RuntimeVariantId,
  resolution: RuntimeResolution,
  startup: Schema.optional(StartupMeasurement),
  probeWallMs: Schema.optional(Schema.Number),
  peakRssBytes: Schema.optional(Schema.Number),
  rssSampled: Schema.optional(Schema.Boolean),
  probe: Schema.optional(ProbeOutput),
  probeFailure: Schema.optional(Schema.String),
  unavailableReason: Schema.optional(Schema.String),
})
export type RuntimeMeasurement = typeof RuntimeMeasurement.Type

export const HostCallMeasurement = Schema.Struct({
  variant: RuntimeVariantId,
  latencyMs: LatencyProfileMs,
  calls: Schema.Number,
  wallMs: Schema.Number,
  p50OverheadMs: Schema.Number,
  p95OverheadMs: Schema.Number,
  fanoutOk: Schema.Boolean,
  latencySimulation: Schema.Literals(["sleep", "busy-wait", "external-sleep", "host-sleep"]),
})
export type HostCallMeasurement = typeof HostCallMeasurement.Type

export const ProgressiveDisclosureRow = Schema.Struct({
  tools: Schema.Number,
  fullSchemaChars: Schema.Number,
  progressiveChars: Schema.Number,
  savingsRatio: Schema.Number,
  tokenMeasurement: Schema.Literal("blocked"),
  blockedBy: Schema.Array(Schema.String),
})
export type ProgressiveDisclosureRow = typeof ProgressiveDisclosureRow.Type

export const SecurityProbe = Schema.Literals([
  "file-write",
  "env-read",
  "process-spawn",
  "network-socket",
  "secret-file-read",
])

export const SecurityRow = Schema.Struct({
  variant: RuntimeVariantId,
  probe: SecurityProbe,
  allowed: Schema.NullOr(Schema.Boolean),
  errorClass: Schema.optional(Schema.String),
  note: Schema.String,
})
export type SecurityRow = typeof SecurityRow.Type

export const SnapshotRow = Schema.Struct({
  variant: RuntimeVariantId,
  snapshotSupported: Schema.Boolean,
  snapshotOk: Schema.Boolean,
  approvalPauseSupported: Schema.Boolean,
  approvalPauseOk: Schema.Boolean,
  trust: Schema.String,
  detail: Schema.optional(Schema.String),
})
export type SnapshotRow = typeof SnapshotRow.Type

export const ModelSlot = Schema.Struct({
  provider: Schema.String,
  tier: Schema.Literals(["primary", "smaller"]),
  modelId: Schema.NullOr(Schema.String),
  credentialEnv: Schema.String,
  credentialPresent: Schema.Boolean,
})
export type ModelSlot = typeof ModelSlot.Type

export const MissingInput = Schema.Struct({
  id: Schema.String,
  description: Schema.String,
  exact: Schema.String,
})
export type MissingInput = typeof MissingInput.Type

export const ModelCell = Schema.Struct({
  arm: ArmId,
  taskId: Schema.String,
  latencyMs: LatencyProfileMs,
  provider: Schema.String,
  tier: Schema.Literals(["primary", "smaller"]),
  trials: Schema.Number,
  plannedTrials: Schema.Number,
  status: Schema.Literals(["blocked", "ready"]),
  blockedBy: Schema.Array(Schema.String),
  metrics: Schema.Record(Schema.String, Schema.Number),
})
export type ModelCell = typeof ModelCell.Type

export const FailureClass = Schema.Struct({
  id: Schema.String,
  description: Schema.String,
  hardFail: Schema.Boolean,
})
export type FailureClass = typeof FailureClass.Type

export const DecisionRecord = Schema.Struct({
  gate: Schema.Literal("DG-1"),
  status: Schema.Literals(["open", "closed"]),
  recommendation: Schema.Literals(["none", "select-arm"]),
  rationale: Schema.Array(Schema.String),
  supportedFindings: Schema.Array(Schema.String),
  unsupportedFindings: Schema.Array(Schema.String),
})
export type DecisionRecord = typeof DecisionRecord.Type
