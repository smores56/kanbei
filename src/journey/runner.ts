import { DenoServices } from "@effect/platform-deno"
import { Effect, Layer, Schema } from "effect"
import { fileArtifactStoreLayer } from "../evidence/artifact-store.ts"
import { canonicalize } from "../evidence/canonical.ts"
import { sha256Text } from "../evidence/digest.ts"
import { liveEnvironmentProbeLayer } from "../evidence/environment.ts"
import { FixtureStore, fixtureStoreLayer } from "../evidence/fixture-store.ts"
import { runEvidence } from "../evidence/harness.ts"
import type { EvidenceEnvelope, FixtureReference } from "../evidence/types.ts"
import { JourneyFixture, JourneySource, JourneyThresholds } from "./schema.ts"
import type { JourneyStepOutcome } from "./schema.ts"
import { journeySteps } from "./steps.ts"
import type { StepContext } from "./steps.ts"

export class JourneyStepFailure extends Error {
  readonly _tag = "JourneyStepFailure"
  readonly testId: string
  readonly checks: ReadonlyArray<string>

  constructor(testId: string, checks: ReadonlyArray<string>) {
    super(`${testId}: ${checks.join(", ")}`)
    this.testId = testId
    this.checks = checks
  }
}

export interface JourneyStepReport {
  readonly testId: string
  readonly outcome: JourneyStepOutcome["outcome"]
  readonly checks: ReadonlyArray<string>
  readonly metrics: Readonly<Record<string, number>>
  readonly artifacts: ReadonlyArray<string>
  /** Host-owned work this step cannot close, declared on the evidence envelope. */
  readonly acceptedResiduals: ReadonlyArray<string>
  readonly failureClass?: string
}

export interface JourneyReport {
  readonly schemaVersion: 1
  readonly dataset: string
  readonly fixtureTreeHash: string
  readonly thresholdBasis: string
  readonly thresholds: JourneyThresholds
  readonly steps: ReadonlyArray<JourneyStepReport>
  readonly metrics: Readonly<Record<string, number>>
  /** Every residual the run accepted, deduped and sorted; never silent. */
  readonly acceptedResiduals: ReadonlyArray<string>
  readonly violations: ReadonlyArray<string>
  readonly pass: boolean
  readonly runDigest: string
}

const stepIds = [
  "J2-INGEST",
  "J2-FRESHNESS",
  "J2-ADVICE",
  "J2-MEMORY",
  "J2-TODO",
  "J2-SEARCH",
  "J2-CSP",
  "J2-BOUNDARY",
] as const

const invariantIds: ReadonlyArray<ReadonlyArray<string>> = [
  ["I-02", "I-07", "I-08", "I-16"],
  ["I-16", "ZT-18"],
  ["I-07", "I-16"],
  ["I-01", "I-05", "I-06", "I-07"],
  ["ZT-17"],
  ["ZT-01", "ZT-17", "ZT-18"],
  ["ZT-16"],
  ["I-20", "ZT-15"],
]

export const fixtureDirectory = (root: string): string => `${root}/fixtures/journey-v2`

export const loadFixture = async (root: string): Promise<JourneyFixture> =>
  Schema.decodeUnknownSync(JourneyFixture)(
    JSON.parse(await Deno.readTextFile(`${fixtureDirectory(root)}/fixture.json`)),
  )

export const loadSource = async (root: string): Promise<JourneySource> =>
  Schema.decodeUnknownSync(JourneySource)(
    JSON.parse(await Deno.readTextFile(`${fixtureDirectory(root)}/source.json`)),
  )

export const loadThresholds = async (root: string): Promise<JourneyThresholds> =>
  Schema.decodeUnknownSync(JourneyThresholds)(
    JSON.parse(await Deno.readTextFile(`${fixtureDirectory(root)}/thresholds.json`)),
  )

const referenceOf = (fixture: JourneyFixture): FixtureReference => ({
  id: fixture.dataset,
  version: fixture.version,
  canonicalHead: fixture.canonicalHead,
  sequenceRange: fixture.sequenceRange,
})

const thresholdViolations = (
  metrics: Readonly<Record<string, number>>,
  thresholds: JourneyThresholds,
): ReadonlyArray<string> => {
  const violations: Array<string> = []
  const atLeast = (name: string, minimum: number) => {
    const value = metrics[name]
    if (value === undefined || value < minimum) {
      violations.push(`${name}: ${value ?? "absent"} < ${minimum}`)
    }
  }
  const atMost = (name: string, maximum: number) => {
    const value = metrics[name]
    if (value === undefined || value > maximum) {
      violations.push(`${name}: ${value ?? "absent"} > ${maximum}`)
    }
  }
  atLeast("advisoryPrecision", thresholds.advisory.precisionMin)
  atLeast("advisoryRecall", thresholds.advisory.recallMin)
  atLeast("memoryPrecision", thresholds.memory.precisionMin)
  atLeast("memoryRecall", thresholds.memory.recallMin)
  atMost("staleAsFresh", thresholds.freshness.staleAsFreshMax)
  atMost("missingAsCurrent", thresholds.freshness.missingAsCurrentMax)
  atMost("capViolations", thresholds.todo.capViolationsMax)
  atMost("conflictOverwrites", thresholds.todo.conflictOverwritesMax)
  atLeast("blockedRecall", thresholds.search.blockedRecallMin)
  atMost("budgetOverruns", thresholds.search.budgetOverrunsMax)
  atMost("forgedFieldsForwarded", thresholds.search.forgedFieldsForwardedMax)
  atMost("channelLeaks", thresholds.channelLeaksMax)
  atMost("cspRemoteRequests", thresholds.cspRemoteRequestsMax)
  return violations
}

const evaluate = (
  fixture: JourneyFixture,
  thresholds: JourneyThresholds,
  steps: ReadonlyArray<JourneyStepReport>,
): Effect.Effect<JourneyReport> =>
  Effect.gen(function* () {
    const metrics: Record<string, number> = {}
    for (const step of steps) Object.assign(metrics, step.metrics)
    const violations = thresholdViolations(metrics, thresholds)
    const failed = steps.filter((step) => step.outcome !== "pass")
    const pass = violations.length === 0 && failed.length === 0
    const acceptedResiduals = [...new Set(steps.flatMap((step) => step.acceptedResiduals))]
      .toSorted()
    const runDigest = yield* sha256Text(
      canonicalize(
        Schema.decodeUnknownSync(Schema.Json)({
          dataset: `${fixture.dataset}@${fixture.version}`,
          steps: steps.map((step) => ({
            testId: step.testId,
            outcome: step.outcome,
            checks: [...step.checks].toSorted(),
            metrics: step.metrics,
            artifacts: step.artifacts,
            acceptedResiduals: step.acceptedResiduals,
          })),
          thresholds,
          metrics,
          acceptedResiduals,
          violations,
        }),
      ),
    ).pipe(Effect.orDie)
    return {
      schemaVersion: 1,
      dataset: `${fixture.dataset}@${fixture.version}`,
      fixtureTreeHash: "",
      thresholdBasis: thresholds.basis,
      thresholds,
      steps,
      metrics,
      acceptedResiduals,
      violations,
      pass,
      runDigest,
    }
  })

export const runJourney = async (
  root: string,
): Promise<
  { readonly report: JourneyReport; readonly envelopes: ReadonlyArray<EvidenceEnvelope> }
> => {
  const fixture = await loadFixture(root)
  const source = await loadSource(root)
  const thresholds = await loadThresholds(root)
  const reference = referenceOf(fixture)
  const context: StepContext = {
    fixtureDirectory: fixtureDirectory(root),
    fixture,
    source,
  }
  const outputs = new Map<string, JourneyStepOutcome>()
  const layers = Layer.mergeAll(
    fileArtifactStoreLayer(`${root}/tmp/evidence-cas`),
    fixtureStoreLayer(root),
    liveEnvironmentProbeLayer,
  ).pipe(Layer.provide(DenoServices.layer))

  const envelopes = await Effect.runPromise(
    Effect.forEach(
      journeySteps.map((step, index) => ({ step, index })),
      ({ step, index }) =>
        runEvidence({
          testId: stepIds[index] ?? `J2-STEP-${index}`,
          testClass: "bench",
          invariantIds: invariantIds[index] ?? [],
          fixture: reference,
          seeds: [index + 1],
          residualsFrom: (outcome: JourneyStepOutcome) => outcome.residuals ?? [],
          body: (_snapshot, capture) =>
            Effect.gen(function* () {
              const outcome = yield* Effect.promise(() => step(context))
              outputs.set(outcome.testId, outcome)
              yield* capture.putArtifactText(JSON.stringify(outcome.detail, null, 2))
              if (outcome.checks.length > 0) {
                return yield* Effect.fail(
                  new JourneyStepFailure(outcome.testId, outcome.checks),
                )
              }
              return outcome
            }),
        }).pipe(Effect.map((envelope) => ({ envelope, index }))),
      { concurrency: 1 },
    ).pipe(Effect.provide(layers)),
  )

  const steps: ReadonlyArray<JourneyStepReport> = envelopes.map(({ envelope }) => {
    const outcome = outputs.get(envelope.testId)
    return {
      testId: envelope.testId,
      outcome: envelope.outcome === "pass" ? "pass" : "fail",
      checks: outcome?.checks ?? [envelope.failureClass ?? "unknown"],
      metrics: outcome?.metrics ?? {},
      artifacts: envelope.artifacts,
      acceptedResiduals: outcome?.residuals ?? [],
      ...(envelope.failureClass === undefined ? {} : { failureClass: envelope.failureClass }),
    }
  })
  const evaluated = await Effect.runPromise(evaluate(fixture, thresholds, steps))
  const treeHash = await Effect.runPromise(
    Effect.gen(function* () {
      const store = yield* FixtureStore
      const snapshot = yield* store.load(fixture.dataset, fixture.version)
      return snapshot.treeHash
    }).pipe(Effect.provide(fixtureStoreLayer(root)), Effect.provide(DenoServices.layer)),
  )
  return {
    report: { ...evaluated, fixtureTreeHash: treeHash },
    envelopes: envelopes.map((entry) => entry.envelope),
  }
}

export const reportText = (report: JourneyReport): string => `${JSON.stringify(report, null, 2)}\n`
