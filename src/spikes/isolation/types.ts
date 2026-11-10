import { Schema } from "effect"
import { EvidenceEnvelope } from "../../evidence/types.ts"

export const ArmId = Schema.Literals(["process-cgroup", "wasm-instance", "microvm"])
export type ArmId = typeof ArmId.Type

export const ARM_IDS: ReadonlyArray<ArmId> = ["process-cgroup", "wasm-instance", "microvm"]

export const ArmStatus = Schema.Literals(["measured", "blocked", "inconclusive"])
export type ArmStatus = typeof ArmStatus.Type

export const CheckId = Schema.Literals([
  "crash-isolation",
  "hang-containment",
  "oom-containment",
  "fork-bomb-containment",
  "cleanup-after-kill",
  "orphan-reaping",
  "ipc-round-trip",
  "resource-ceilings",
  "hosts-agent-workload",
])
export type CheckId = typeof CheckId.Type

export const CHECK_IDS: ReadonlyArray<CheckId> = [
  "crash-isolation",
  "hang-containment",
  "oom-containment",
  "fork-bomb-containment",
  "cleanup-after-kill",
  "orphan-reaping",
  "ipc-round-trip",
  "resource-ceilings",
  "hosts-agent-workload",
]

export const CheckStatus = Schema.Literals(["pass", "fail", "not-applicable", "not-run"])
export type CheckStatus = typeof CheckStatus.Type

export const ContainmentCheck = Schema.Struct({
  id: CheckId,
  status: CheckStatus,
  detail: Schema.String,
})
export type ContainmentCheck = typeof ContainmentCheck.Type

export const LatencySummary = Schema.Struct({
  p50Ms: Schema.Number,
  p95Ms: Schema.Number,
  maxMs: Schema.Number,
  samples: Schema.Number,
})
export type LatencySummary = typeof LatencySummary.Type

export const DensityOutcome = Schema.Literals(["ok", "partial", "skipped"])
export type DensityOutcome = typeof DensityOutcome.Type

export const DensityPoint = Schema.Struct({
  agents: Schema.Number,
  outcome: DensityOutcome,
  readyMs: Schema.Number,
  totalRssBytes: Schema.Number,
  perAgentRssBytes: Schema.Number,
  failures: Schema.Number,
})
export type DensityPoint = typeof DensityPoint.Type

export const ArmMetrics = Schema.Struct({
  coldStartMs: LatencySummary,
  cpuWorkMs: Schema.Number,
  cpuWorkIterations: Schema.Number,
  ipcRoundTripMs: LatencySummary,
  rssPerAgentBytes: Schema.Number,
  density: Schema.Array(DensityPoint),
})
export type ArmMetrics = typeof ArmMetrics.Type

export const Operability = Schema.Struct({
  hostDependencies: Schema.Array(Schema.String),
  requiresRoot: Schema.Boolean,
  supervisorLoc: Schema.Number,
  packaging: Schema.String,
  debuggability: Schema.String,
  limitations: Schema.Array(Schema.String),
})
export type Operability = typeof Operability.Type

export const ArmResult = Schema.Struct({
  arm: ArmId,
  status: ArmStatus,
  blockedReason: Schema.optional(Schema.String),
  metrics: Schema.optional(ArmMetrics),
  checks: Schema.Array(ContainmentCheck),
  operability: Operability,
  notes: Schema.Array(Schema.String),
})
export type ArmResult = typeof ArmResult.Type

export const HostCapabilities = Schema.Struct({
  kernel: Schema.String,
  runtime: Schema.String,
  cpus: Schema.Number,
  totalMemoryBytes: Schema.Number,
  cgroupVersion: Schema.Literals(["v2", "none"]),
  cgroupControllers: Schema.Array(Schema.String),
  userNamespaces: Schema.Literals(["available", "unavailable"]),
  landlockAbi: Schema.Number,
  cgroupNsdelegate: Schema.Boolean,
  delegatedParent: Schema.Literals(["owned-by-uid", "not-owned"]),
  kvmDevice: Schema.Literals(["accessible", "present-not-accessible", "absent"]),
  microvmRuntimes: Schema.Array(Schema.String),
  generalVmm: Schema.Array(Schema.String),
  wasmEngine: Schema.String,
  privilegedOperations: Schema.Literal(false),
})
export type HostCapabilities = typeof HostCapabilities.Type

export const SupervisorInfo = Schema.Struct({
  kind: Schema.Literal("rootless-userns-chroot-landlock"),
  uid: Schema.Number,
  landlockAbi: Schema.Number,
  cgroupRoot: Schema.String,
  delegatedBy: Schema.String,
  dependencies: Schema.Tuple([Schema.String, Schema.String]),
})
export type SupervisorInfo = typeof SupervisorInfo.Type

export const MatrixRow = Schema.Struct({
  check: CheckId,
  cells: Schema.Record(Schema.String, CheckStatus),
})
export type MatrixRow = typeof MatrixRow.Type

export const IneligibleArm = Schema.Struct({
  arm: ArmId,
  reason: Schema.String,
})
export type IneligibleArm = typeof IneligibleArm.Type

export const Recommendation = Schema.Struct({
  decision: Schema.Literal("DG-4"),
  status: Schema.Literals(["recommend", "no-selection"]),
  recommended: Schema.optional(ArmId),
  rule: Schema.String,
  targetConcurrency: Schema.Number,
  eligible: Schema.Array(Schema.String),
  ineligible: Schema.Array(IneligibleArm),
  rationale: Schema.Array(Schema.String),
})
export type Recommendation = typeof Recommendation.Type

export const EvidenceBinding = Schema.Union([
  Schema.Struct({ status: Schema.Literal("emitted"), envelope: EvidenceEnvelope }),
  Schema.Struct({ status: Schema.Literal("unavailable"), reason: Schema.String }),
])
export type EvidenceBinding = typeof EvidenceBinding.Type

export const SecurityStatement = Schema.Struct({
  propertyClaimed: Schema.Literal(false),
  statement: Schema.String,
  excluded: Schema.Array(Schema.String),
})
export type SecurityStatement = typeof SecurityStatement.Type

export const SpikeResult = Schema.Struct({
  schemaVersion: Schema.Literal(2),
  spikeId: Schema.Literal("S3"),
  validationId: Schema.Literal("ISO-1"),
  dataset: Schema.Literal("security-v1"),
  generatedAt: Schema.String,
  host: HostCapabilities,
  supervisor: SupervisorInfo,
  targetConcurrency: Schema.Number,
  security: SecurityStatement,
  arms: Schema.Array(ArmResult),
  matrix: Schema.Array(MatrixRow),
  recommendation: Recommendation,
  residuals: Schema.Array(Schema.String),
  evidence: EvidenceBinding,
})
export type SpikeResult = typeof SpikeResult.Type
