import type { ArmAssessment, ProbeResult, Recommendation } from "./types.ts"

export type ArmKey = "pure" | "dedicated"

const SECRET_CATEGORIES: ReadonlySet<string> = new Set([
  "secret-reachability",
  "same-uid-process",
  "broker-identity",
])

const enforcementFor = (probe: ProbeResult, arm: ArmKey) =>
  probe.enforcement === undefined ? "not-applicable" : probe.enforcement[arm]

/**
 * A probe informs an arm when it was run under that arm or under a condition both
 * arms share. A probe run only under the deployment UID never proves a claim about
 * the dedicated-UID arm.
 */
const informsArm = (probe: ProbeResult, arm: ArmKey): boolean => {
  if (probe.arm === "both") return true
  return arm === "pure" ? probe.arm === "pure-user-space" : probe.arm === "dedicated-uid"
}

/** A probe applies to an arm when it ran under that arm or under a shared condition. */
const appliesToArm = (probe: ProbeResult, arm: ArmKey): boolean =>
  probe.arm === (arm === "pure" ? "pure-user-space" : "dedicated-uid") ||
  (arm === "pure" && probe.arm === "both")

/** A measurement counts for an arm only when it exercised that arm and held. */
const measuredForArm = (probe: ProbeResult, arm: ArmKey): boolean =>
  probe.status === "pass" && appliesToArm(probe, arm)

/**
 * Derives an arm's enforcement profile mechanically from measured probes.
 * A property is only listed as kernel-enforced when a probe measured it holding for
 * that arm; an unmeasured kernel claim is unverified, never enforced.
 */
export const assessArm = (
  probes: ReadonlyArray<ProbeResult>,
  arm: ArmKey,
): ArmAssessment => {
  const relevant = probes.filter((probe) => informsArm(probe, arm))
  const kernelEnforced = relevant
    .filter((probe) => enforcementFor(probe, arm) === "kernel" && measuredForArm(probe, arm))
    .map((probe) => probe.id)
  const conventional = relevant
    .filter((probe) => enforcementFor(probe, arm) === "conventional" && probe.status === "pass")
    .map((probe) => probe.id)
  const violations = relevant
    .filter((probe) =>
      enforcementFor(probe, arm) === "conventional" && probe.status === "fail" ||
      enforcementFor(probe, arm) === "kernel" && probe.status === "fail" && appliesToArm(probe, arm)
    )
    .map((probe) => probe.id)
  const unverified = relevant
    .filter((probe) =>
      enforcementFor(probe, arm) === "kernel" &&
      !(appliesToArm(probe, arm) && (probe.status === "pass" || probe.status === "fail"))
    )
    .map((probe) => probe.id)

  const secretViolations = relevant
    .filter(
      (probe) =>
        SECRET_CATEGORIES.has(probe.category) &&
        enforcementFor(probe, arm) === "conventional" &&
        probe.status === "fail",
    )
    .map((probe) => probe.id)
  const secretUnverified = relevant
    .filter(
      (probe) =>
        SECRET_CATEGORIES.has(probe.category) &&
        enforcementFor(probe, arm) === "kernel" &&
        !measuredForArm(probe, arm),
    )
    .map((probe) => probe.id)

  const secretIsolation = secretViolations.length > 0
    ? "none" as const
    : secretUnverified.length > 0
    ? "unverified" as const
    : "kernel-enforced" as const

  const note = secretViolations.length > 0
    ? `measured secret-isolation failures: ${secretViolations.join(", ")}`
    : secretUnverified.length > 0
    ? `secret isolation was not exercised for this arm: ${secretUnverified.join(", ")}`
    : "secret isolation was measured as kernel-enforced"

  return {
    arm: arm === "pure" ? "pure-user-space" : "dedicated-uid",
    kernelEnforced,
    conventional: [...conventional, ...violations],
    unverified,
    secretIsolation,
    note,
  }
}

export const buildRecommendation = (
  arms: ReadonlyArray<ArmAssessment>,
): Recommendation => {
  const pure = arms.find((arm) => arm.arm === "pure-user-space")
  const dedicated = arms.find((arm) => arm.arm === "dedicated-uid")
  const pureConventional = pure?.conventional ?? []
  const dedicatedUnverified = dedicated?.unverified ?? []

  return {
    variant: "D",
    summary:
      "Hybrid: core, agents, and the UI stay under the deployment user; the broker and the sandbox supervisor run as dedicated system users in system units.",
    rationale: [
      `Pure user-space cannot protect broker secrets: same-UID code read an open credential descriptor and 0600 files, and signalled sibling processes (conventional: ${
        pureConventional.join(", ")
      }).`,
      `The user manager is controllable by the deployment UID, so hard ceilings, restart discipline, and the watchdog are conventional rather than kernel-enforced for a pure user-space arm (SD-10, LM-01).`,
      "Kernel-enforced pieces that remain available to the user-space core are Landlock filesystem and TCP rules, Yama-scoped memory denial, cgroup ceilings inside a unit, and loopback-only binding.",
      `Dedicated-UID enforcement claims could not be exercised from an unprivileged run (${
        dedicatedUnverified.length > 0 ? dedicatedUnverified.join(", ") : "none"
      }), so they are recorded as unverified rather than assumed.`,
    ],
    requiredConditions: [
      "Create kanbei-broker and kanbei-sandbox system users and run both as system units; the deployment user must not be able to signal, trace, or reconfigure them.",
      "Keep broker credentials outside ~/.local/state/kanbei and outside any path writable by the deployment user; the broker UID alone must be able to read them.",
      "Bind the UI to 127.0.0.1 and expose it only through the existing tunnel/Access convention; a wildcard bind was measured as externally reachable (NW-02).",
      "Apply Landlock filesystem and TCP rules in the sandbox supervisor before exec, and keep kernel.yama.ptrace_scope >= 1; both were measured as effective only when present.",
      "Verify effective systemd hardening at startup and refuse to start when a directive is accepted but ineffective (SD-04).",
      "Set LimitCORE=0 on broker units: core dumps are piped to the system coredump handler on this host (SC-05).",
      "Remove the deployment user from the docker group: membership is root-equivalent and would erase any UID separation on this host (SC-04).",
      "Use system units for the watchdog and alerting; a user unit is journal-only and dies with the session unless linger is enabled (RC-03).",
      "Kill the whole control group on supervisor termination so orphaned sandboxes cannot survive (RC-02).",
    ],
    rejectedAlternatives: [
      "A/B pure user-space (home-manager or NixOS-plane user unit): measured secret isolation is none; same-UID code reads broker-held credentials and rewrites ceilings.",
      "C all-dedicated system units: strongest identity separation but makes mutable code and activation human-root-only, which contradicts the user-controlled mutable layer of D-2.",
    ],
    residuals: [
      "Dedicated-UID enforcement is unverified on this host because no privileged operation was permitted; the decision relies on the kernel UID model plus review.",
      "If a future host allows the deployment user to join docker or equivalent groups, all UID separation is void.",
      "Yama ptrace scope, Landlock ABI, and cgroup delegation are host configuration; a host rebuild can silently weaken them unless startup verification checks effective values.",
      "UDP egress, io_uring, and SCM_CREDENTIALS behaviour remain outside the measured enforcement.",
    ],
  }
}
