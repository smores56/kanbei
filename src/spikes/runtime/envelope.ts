import { Effect, Schema } from "effect"
import { runEvidence } from "../../evidence/harness.ts"
import { EvidenceEnvelope } from "../../evidence/types.ts"
import type { FixtureReference, Outcome } from "../../evidence/types.ts"
import type { SpikeResultDocument } from "./schema.ts"

/**
 * A spike with no canonical ledger still has to bind its evidence. The head is
 * the empty chain and the sequence range is empty on purpose: nothing in this
 * run writes canonical records, and claiming a real head would be fabrication.
 */
export const appendFixtureReference = (version: string): FixtureReference => ({
  id: "append-v1",
  version,
  canonicalHead: { seq: 0, hash: "sha256:" + "0".repeat(64) },
  sequenceRange: [0, 0],
})

/**
 * The permission and fail-closed probes are a security measurement, so they bind
 * `security-v1` rather than the ledger's `append-v1`. Nothing here writes a
 * canonical record either, so the head is the empty chain for the same reason.
 */
export const securityFixtureReference = (version: string): FixtureReference => ({
  id: "security-v1",
  version,
  canonicalHead: { seq: 0, hash: "sha256:" + "0".repeat(64) },
  sequenceRange: [0, 0],
})

/**
 * The expected denial set is the union of both arms' pre-registered expectations.
 * Node is not expected to deny `net-connect` (it has no network capability), so
 * that probe is not an invariant here.
 */
export const securityInvariantIds = (
  expectations: SpikeResultDocument["preRegistration"]["permissionExpectations"],
): ReadonlyArray<string> => [...new Set([...expectations.deno, ...expectations.node])]

/**
 * An expected denial is a pass; a probe the runtime should have denied but
 * allowed is a fail. This is the probe data's verdict, independent of the
 * runtime decision.
 */
export const securityEnvelopeOutcome = (
  arms: SpikeResultDocument["arms"],
  expectations: SpikeResultDocument["preRegistration"]["permissionExpectations"],
): Outcome => {
  const allDenied = (["deno", "node"] as const).every((arm) =>
    expectations[arm].every((id) =>
      arms[arm].permissions.some((probe) => probe.id === id && probe.denied)
    )
  )
  return allDenied ? "pass" : "fail"
}

export const outcomeOf = (
  decision: SpikeResultDocument["decision"]["value"],
  blocked: ReadonlyArray<string>,
  bindingGaps: ReadonlyArray<string> = [],
): Outcome => {
  if (blocked.length > 0 || bindingGaps.length > 0) return "blocked"
  if (decision === "inconclusive") return "inconclusive"
  return "pass"
}

export const envelopeMetrics = (
  document: Omit<SpikeResultDocument, "envelope">,
): Record<string, number> => {
  const metrics: Record<string, number> = {
    compositeNode: document.composite.node,
    compositeDeno: document.composite.deno,
    compositeMargin: document.composite.margin,
    hardFails: document.hardFails.length,
  }
  for (const measurement of document.measurements) {
    metrics[`node.${measurement.id}`] = measurement.node
    metrics[`deno.${measurement.id}`] = measurement.deno
  }
  return metrics
}

/**
 * The A0 harness classifies a successful body as `pass`; a spike additionally
 * has to classify its own decision (`inconclusive` blocks the gate, `blocked` is
 * a defect). The harness still owns the binding — fixture, environment, artifacts,
 * seeds — and the re-validated envelope keeps the `07 §7.0` contract.
 */
export const sealSpikeEvidence = (
  document: Omit<SpikeResultDocument, "envelope">,
  fixture: FixtureReference,
  outcome: Outcome,
) =>
  runEvidence({
    testId: "NODE-DENO-1",
    testClass: "spike",
    invariantIds: [],
    fixture,
    seeds: [0],
    body: (_snapshot, capture) =>
      Effect.gen(function* () {
        yield* capture.putArtifactText(JSON.stringify(document))
        return document
      }),
    metrics: envelopeMetrics(document),
  }).pipe(
    Effect.map((envelope) => Schema.decodeUnknownSync(EvidenceEnvelope)({ ...envelope, outcome })),
  )

const deniedCount = (
  probes: SpikeResultDocument["arms"]["node"]["permissions"],
): number => probes.filter((probe) => probe.denied).length

const securityMetrics = (
  document: Omit<SpikeResultDocument, "envelope">,
): Record<string, number> => ({
  expectedProbes: securityInvariantIds(document.preRegistration.permissionExpectations).length,
  nodeDeniedProbes: deniedCount(document.arms.node.permissions),
  denoDeniedProbes: deniedCount(document.arms.deno.permissions),
})

/**
 * The security probes bind their own dataset and carry their own outcome; the
 * artifact records the probe data the outcome was computed from.
 */
export const sealSecurityEvidence = (
  document: Omit<SpikeResultDocument, "envelope">,
  fixture: FixtureReference,
  outcome: Outcome,
) =>
  runEvidence({
    testId: "NODE-DENO-1-SECURITY",
    testClass: "spike",
    invariantIds: [...securityInvariantIds(document.preRegistration.permissionExpectations)],
    fixture,
    seeds: [0],
    body: (_snapshot, capture) =>
      Effect.gen(function* () {
        yield* capture.putArtifactText(
          JSON.stringify({
            expectations: document.preRegistration.permissionExpectations,
            deno: document.arms.deno.permissions,
            node: document.arms.node.permissions,
          }),
        )
        return document
      }),
    metrics: securityMetrics(document),
  }).pipe(
    Effect.map((envelope) => Schema.decodeUnknownSync(EvidenceEnvelope)({ ...envelope, outcome })),
  )
