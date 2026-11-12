import { Effect, Schema } from "effect"
import { runEvidence } from "../../evidence/harness.ts"
import { EvidenceEnvelope } from "../../evidence/types.ts"
import type { FixtureReference, Outcome } from "../../evidence/types.ts"
import type { SpikeResultBody } from "./types.ts"

/**
 * The same-UID probes measure what another process of the deployment UID can do.
 * Their `fail` status is the finding, not a defect, so it is recorded as-is.
 */
export const SAME_UID_INVARIANTS: ReadonlyArray<string> = [
  "same-uid-process-inspection",
  "same-uid-signal",
  "secret-fd-reachability",
]

/**
 * No canonical record is written by this spike, so the head is the empty chain
 * and the sequence range is empty on purpose.
 */
export const securityFixtureReference = (version: string): FixtureReference => ({
  id: "security-v1",
  version,
  canonicalHead: { seq: 0, hash: "sha256:" + "0".repeat(64) },
  sequenceRange: [0, 0],
})

const envelopeMetrics = (result: SpikeResultBody): Record<string, number> => ({
  probes: result.probes.length,
  failures: result.probes.filter((probe) => probe.status === "fail").length,
  blocked: result.probes.filter((probe) => probe.status === "blocked").length,
})

export const sealDeploymentEvidence = (
  result: SpikeResultBody,
  fixture: FixtureReference,
  outcome: Outcome,
) =>
  runEvidence({
    testId: "DEP-1",
    testClass: "spike",
    invariantIds: [...SAME_UID_INVARIANTS],
    fixture,
    seeds: [0],
    body: (_snapshot, capture) =>
      Effect.gen(function* () {
        yield* capture.putArtifactText(JSON.stringify(result.probes))
        return result
      }),
    metrics: envelopeMetrics(result),
  }).pipe(
    Effect.map((envelope) => Schema.decodeUnknownSync(EvidenceEnvelope)({ ...envelope, outcome })),
  )
