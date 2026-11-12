import type { ArmResult, SpikeResult } from "./types.ts"

/** Checks that gate eligibility: every check an arm reports must pass or be n/a. */
const gatingChecksHold = (arm: ArmResult): boolean =>
  arm.checks.every((check) => check.status === "pass" || check.status === "not-applicable")

export interface ExitVerdict {
  readonly code: number
  readonly reasons: ReadonlyArray<string>
}

const failingChecks = (arm: ArmResult): string =>
  arm.checks
    .filter((check) => check.status === "fail" || check.status === "not-run")
    .map((check) => `${check.id}=${check.status}`)
    .join(", ")

/**
 * A run is successful only when DG-4 selected an arm, the evidence envelope was
 * emitted, and every arm measured in this run passed the checks that gate
 * eligibility. A measured arm that cannot host the workload therefore leaves the
 * process non-zero even though the recommendation stands; the envelope records
 * that split in `acceptedResiduals`.
 */
export const exitVerdict = (result: SpikeResult): ExitVerdict => {
  const reasons: ReadonlyArray<string> = [
    ...(result.recommendation.status === "recommend" ? [] : ["DG-4 produced no recommendation"]),
    ...(result.evidence.status === "emitted" ? [] : ["the evidence envelope was not emitted"]),
    ...result.arms.flatMap((arm) =>
      arm.status === "measured" && !gatingChecksHold(arm)
        ? [`measured arm ${arm.arm} failed eligibility-gating checks: ${failingChecks(arm)}`]
        : []
    ),
  ]
  return { code: reasons.length === 0 ? 0 : 1, reasons }
}
