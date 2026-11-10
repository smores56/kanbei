import { Schema } from "effect"

export const ProbeStatus = Schema.Literals(["pass", "fail", "blocked", "inconclusive"])
export type ProbeStatus = typeof ProbeStatus.Type

/**
 * Which deployment model a probe informs. `both` means the measurement applies to
 * either model because it is a property of this host's kernel or this UID.
 */
export const ProbeArm = Schema.Literals(["pure-user-space", "dedicated-uid", "both"])
export type ProbeArm = typeof ProbeArm.Type

export const ProbeCategory = Schema.Literals([
  "namespaces",
  "same-uid-process",
  "landlock",
  "systemd",
  "resource-limits",
  "network",
  "secret-reachability",
  "recovery",
  "broker-identity",
  "fail-closed",
])
export type ProbeCategory = typeof ProbeCategory.Type

/**
 * How a property is enforced for an arm. `kernel` means the kernel denies the
 * action regardless of what same-privilege code asks for; `conventional` means a
 * cooperating same-privilege process could decline to cooperate.
 */
export const Enforcement = Schema.Literals([
  "kernel",
  "conventional",
  "not-applicable",
])
export type Enforcement = typeof Enforcement.Type

export const ArmEnforcement = Schema.Struct({
  pure: Enforcement,
  dedicated: Enforcement,
})
export type ArmEnforcement = typeof ArmEnforcement.Type

export const ProbeResult = Schema.Struct({
  id: Schema.String,
  category: ProbeCategory,
  arm: ProbeArm,
  method: Schema.String,
  status: ProbeStatus,
  /** What was measured, in one sentence, with values. */
  observed: Schema.String,
  /** Raw command or syscall evidence backing `observed`. */
  evidence: Schema.String,
  enforcement: Schema.optional(ArmEnforcement),
  prerequisites: Schema.optional(Schema.Array(Schema.String)),
  residual: Schema.optional(Schema.String),
})
export type ProbeResult = typeof ProbeResult.Type

export const HostProfile = Schema.Struct({
  hostname: Schema.String,
  kernel: Schema.String,
  user: Schema.String,
  uid: Schema.Number,
  groups: Schema.Array(Schema.String),
  systemd: Schema.String,
  cgroupVersion: Schema.String,
  yamaPtraceScope: Schema.String,
  landlockAbi: Schema.String,
  maxUserNamespaces: Schema.String,
  linger: Schema.String,
  delegatedControllers: Schema.Array(Schema.String),
  ambientCapabilities: Schema.String,
})
export type HostProfile = typeof HostProfile.Type

export const ArmAssessment = Schema.Struct({
  arm: Schema.Literals(["pure-user-space", "dedicated-uid"]),
  /** Properties the kernel enforces for this arm, each backed by a passing probe. */
  kernelEnforced: Schema.Array(Schema.String),
  /** Properties that hold only while same-privilege code cooperates. */
  conventional: Schema.Array(Schema.String),
  /** Properties this arm should enforce but the host could not verify. */
  unverified: Schema.Array(Schema.String),
  secretIsolation: Schema.Literals(["kernel-enforced", "none", "unverified"]),
  note: Schema.String,
})
export type ArmAssessment = typeof ArmAssessment.Type

export const Recommendation = Schema.Struct({
  variant: Schema.Literals(["A", "B", "C", "D"]),
  summary: Schema.String,
  rationale: Schema.Array(Schema.String),
  requiredConditions: Schema.Array(Schema.String),
  rejectedAlternatives: Schema.Array(Schema.String),
  residuals: Schema.Array(Schema.String),
})
export type Recommendation = typeof Recommendation.Type

export const DecisionGateInput = Schema.Struct({
  gate: Schema.Literal("DG-3"),
  status: Schema.Literal("input-ready-human-signature-required"),
  blocks: Schema.Array(Schema.String),
  evidenceProbes: Schema.Array(Schema.String),
  statement: Schema.String,
})
export type DecisionGateInput = typeof DecisionGateInput.Type

export const SpikeResult = Schema.Struct({
  schemaVersion: Schema.Literal(1),
  protocol: Schema.Literal("DEP-1"),
  spike: Schema.Literal("S2"),
  dataset: Schema.Literal("security-v1"),
  host: HostProfile,
  probes: Schema.Array(ProbeResult),
  arms: Schema.Array(ArmAssessment),
  recommendation: Recommendation,
  decisionGate: DecisionGateInput,
  residuals: Schema.Array(Schema.String),
  blockedCapabilities: Schema.Array(Schema.String),
})
export type SpikeResult = typeof SpikeResult.Type
