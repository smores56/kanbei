import { Effect, Schema } from "effect"
import { CapabilityDenied } from "./errors.ts"

/** What the deployment claims it can enforce. A claim the host cannot back is denied. */
export const DeclaredPosture = Schema.Struct({
  secretIsolation: Schema.Literals(["same-uid", "dedicated-uid"]),
  filesystemIsolation: Schema.Literals(["landlock", "namespaces", "none"]),
  hardCeilings: Schema.Literals(["cgroup", "none"]),
})
export type DeclaredPosture = typeof DeclaredPosture.Type

export const ObservedCapabilities = Schema.Struct({
  landlockAbi: Schema.Number,
  ptraceScope: Schema.Number,
  userNamespacesEnabled: Schema.Boolean,
  cgroupDelegated: Schema.Boolean,
  auditSinkWritable: Schema.Boolean,
})
export type ObservedCapabilities = typeof ObservedCapabilities.Type

export const PostureDecision = Schema.Struct({
  decision: Schema.Literals(["allowed", "denied"]),
  reasons: Schema.Array(Schema.String),
  acceptedResiduals: Schema.Array(Schema.String),
})
export type PostureDecision = typeof PostureDecision.Type

/**
 * Pre-registered fail-closed rules. A deployment that declares an enforcement it
 * cannot obtain must refuse to start; degraded modes are never selected silently.
 */
export const decidePosture = (
  declared: DeclaredPosture,
  observed: ObservedCapabilities,
): PostureDecision => {
  const reasons: string[] = []

  if (declared.filesystemIsolation === "landlock" && observed.landlockAbi < 1) {
    reasons.push("declared Landlock filesystem isolation but the kernel exposes no Landlock ABI")
  }
  if (declared.filesystemIsolation === "namespaces" && !observed.userNamespacesEnabled) {
    reasons.push("declared namespace isolation but user namespaces are unavailable")
  }
  if (declared.hardCeilings === "cgroup" && !observed.cgroupDelegated) {
    reasons.push("declared cgroup ceilings but no cgroup controllers are delegated")
  }
  if (!observed.auditSinkWritable) {
    reasons.push("audit sink is not writable; audited effects must be denied")
  }
  if (declared.secretIsolation === "dedicated-uid" && observed.ptraceScope < 1) {
    reasons.push("declared a dedicated broker UID but same-UID ptrace is unrestricted")
  }

  const acceptedResiduals: string[] = []
  if (declared.secretIsolation === "same-uid") {
    acceptedResiduals.push(
      "broker secrets are readable by any process of the deployment UID (see PR-05, SC-01)",
    )
  }
  if (observed.ptraceScope >= 1) {
    acceptedResiduals.push("memory-inspection denial depends on kernel.yama.ptrace_scope >= 1")
  }
  acceptedResiduals.push("UDP egress and io_uring are not covered by the measured enforcement")

  return {
    decision: reasons.length === 0 ? "allowed" : "denied",
    reasons,
    acceptedResiduals,
  }
}

/** An effect gated on the audit sink; a sink that cannot record makes the effect denied. */
export const guardedEffect = <A, E, R>(
  auditSinkWritable: boolean,
  effect: Effect.Effect<A, E, R>,
): Effect.Effect<A, E | CapabilityDenied, R> =>
  auditSinkWritable ? effect : Effect.fail(
    new CapabilityDenied({
      capability: "audit-sink",
      reason: "audit sink is not writable; the effect is denied rather than unrecorded",
    }),
  )
