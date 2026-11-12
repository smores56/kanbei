import { BUDGET, DECISION_RULE, TARGET_CONCURRENCY } from "./protocol.ts"
import { CHECK_IDS } from "./types.ts"
import type {
  ArmResult,
  CheckStatus,
  ContainmentCheck,
  IneligibleArm,
  MatrixRow,
  Recommendation,
} from "./types.ts"

const mebibyte = 1024 * 1024

const checkOf = (checks: ReadonlyArray<ContainmentCheck>, id: string): CheckStatus =>
  checks.find((check) => check.id === id)?.status ?? "not-run"

/** Reasons are ordered so the first failing gate is the recorded one. */
export const eligibilityFailure = (arm: ArmResult): string | undefined => {
  if (arm.status !== "measured") {
    return `status ${arm.status}${arm.blockedReason === undefined ? "" : `: ${arm.blockedReason}`}`
  }
  const failed = arm.checks.filter((check) => check.status === "fail")
  if (failed.length > 0) {
    return `containment failed: ${failed.map((check) => check.id).join(", ")}`
  }
  const notRun = arm.checks.filter((check) => check.status === "not-run")
  if (notRun.length > 0) {
    return `containment not run: ${notRun.map((check) => check.id).join(", ")}`
  }
  if (checkOf(arm.checks, "hosts-agent-workload") !== "pass") {
    return "cannot host the reference agent workload"
  }
  const metrics = arm.metrics
  if (metrics === undefined) return "no metrics recorded"

  const target = metrics.density.find((point) => point.agents === TARGET_CONCURRENCY)
  if (target === undefined) return `density not measured at ${TARGET_CONCURRENCY} agents`
  if (target.outcome !== "ok" || target.failures > 0) {
    return `density failed at ${TARGET_CONCURRENCY} agents (${target.failures} failures)`
  }
  const rssMib = metrics.rssPerAgentBytes / mebibyte
  if (rssMib > BUDGET.perAgentRssMiB) {
    return `per-agent RSS ${rssMib.toFixed(1)} MiB exceeds ${BUDGET.perAgentRssMiB} MiB`
  }
  if (metrics.coldStartMs.p95Ms > BUDGET.coldStartP95Ms) {
    return `p95 cold start ${
      metrics.coldStartMs.p95Ms.toFixed(0)
    } ms exceeds ${BUDGET.coldStartP95Ms} ms`
  }
  if (metrics.ipcRoundTripMs.p95Ms > BUDGET.ipcP95Ms) {
    return `p95 IPC ${metrics.ipcRoundTripMs.p95Ms.toFixed(1)} ms exceeds ${BUDGET.ipcP95Ms} ms`
  }
  if (arm.operability.requiresRoot) return "requires root"
  if (arm.operability.hostDependencies.length > BUDGET.hostDependencies) {
    return `${arm.operability.hostDependencies.length} host dependencies exceed ${BUDGET.hostDependencies}`
  }
  if (arm.operability.supervisorLoc > BUDGET.supervisorLoc) {
    return `${arm.operability.supervisorLoc} implementation lines exceed ${BUDGET.supervisorLoc}`
  }
  return undefined
}

const rank = (left: ArmResult, right: ArmResult): number => {
  const leftMetrics = left.metrics
  const rightMetrics = right.metrics
  if (leftMetrics === undefined || rightMetrics === undefined) return 0
  const coldStart = leftMetrics.coldStartMs.p95Ms - rightMetrics.coldStartMs.p95Ms
  if (coldStart !== 0) return coldStart
  const rss = leftMetrics.rssPerAgentBytes - rightMetrics.rssPerAgentBytes
  if (rss !== 0) return rss
  return leftMetrics.ipcRoundTripMs.p95Ms - rightMetrics.ipcRoundTripMs.p95Ms
}

const rationaleFor = (selected: ArmResult, eligibleCount: number): string =>
  eligibleCount === 1
    ? `${selected.arm} is the only arm that satisfies every pre-registered gate`
    : `${selected.arm} has the lowest p95 cold start among ${eligibleCount} eligible arms`

export const recommend = (arms: ReadonlyArray<ArmResult>): Recommendation => {
  const ineligible: ReadonlyArray<IneligibleArm> = arms.flatMap((arm) => {
    const reason = eligibilityFailure(arm)
    return reason === undefined ? [] : [{ arm: arm.arm, reason }]
  })
  const eligible = arms.filter((arm) => eligibilityFailure(arm) === undefined)
  const ranked = [...eligible].toSorted(rank)
  const selected = ranked[0]
  const rationale = ineligible.map((entry) => `${entry.arm}: ${entry.reason}`)

  return selected === undefined
    ? {
      decision: "DG-4",
      status: "no-selection",
      rule: DECISION_RULE,
      targetConcurrency: TARGET_CONCURRENCY,
      eligible: [],
      ineligible: [...ineligible],
      rationale: [
        "no arm satisfies the pre-registered gates; DG-4 stays open",
        ...ineligible.map((entry) => `${entry.arm}: ${entry.reason}`),
      ],
    }
    : {
      decision: "DG-4",
      status: "recommend",
      recommended: selected.arm,
      rule: DECISION_RULE,
      targetConcurrency: TARGET_CONCURRENCY,
      eligible: eligible.map((arm) => arm.arm),
      ineligible: [...ineligible],
      rationale: [rationaleFor(selected, eligible.length), ...rationale],
    }
}

export const buildMatrix = (arms: ReadonlyArray<ArmResult>): ReadonlyArray<MatrixRow> =>
  CHECK_IDS.map((check) => ({
    check,
    cells: Object.fromEntries(arms.map((arm) => [arm.arm, checkOf(arm.checks, check)])),
  }))
