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

export const outcomeOf = (
  decision: SpikeResultDocument["decision"]["value"],
  blocked: ReadonlyArray<string>,
): Outcome => {
  if (blocked.length > 0) return "blocked"
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
