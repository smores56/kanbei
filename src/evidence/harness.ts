import { Cause, Effect, Exit, Option, Schema } from "effect"
import type { ArtifactStore } from "./artifact-store.ts"
import { ArtifactStore as ArtifactStoreTag } from "./artifact-store.ts"
import { makeEvidenceCapture } from "./capture.ts"
import type { EvidenceCapture } from "./capture.ts"
import type { EnvironmentProbe } from "./environment.ts"
import { EnvironmentProbe as EnvironmentProbeTag } from "./environment.ts"
import type { FixtureSnapshot, FixtureStore } from "./fixture-store.ts"
import { FixtureStore as FixtureStoreTag } from "./fixture-store.ts"
import type { ArtifactError, DigestError, FixtureError } from "./errors.ts"
import { EvidenceError } from "./errors.ts"
import { EvidenceEnvelope, fixtureVersion } from "./types.ts"
import type { FixtureReference, Outcome, TestClass } from "./types.ts"

export interface EvidenceRun<A, E, R> {
  readonly testId: string
  readonly testClass: TestClass
  readonly invariantIds: ReadonlyArray<string>
  readonly fixture: FixtureReference
  readonly seeds: ReadonlyArray<number>
  readonly body: (fixture: FixtureSnapshot, capture: EvidenceCapture) => Effect.Effect<A, E, R>
  readonly metrics?: Readonly<Record<string, number>>
  readonly acceptedResiduals?: ReadonlyArray<string>
}

interface Verdict {
  readonly outcome: Outcome
  readonly failureClass?: string
}

const tagOf = (value: unknown): string => {
  if (typeof value === "object" && value !== null) {
    const tag = Reflect.get(value, "_tag")
    if (typeof tag === "string") return tag
  }
  return "UnknownFailure"
}

const classifyCause = <E>(cause: Cause.Cause<E>): Verdict => {
  if (Cause.hasInterruptsOnly(cause)) {
    return { outcome: "blocked", failureClass: "Interrupted" }
  }
  if (Cause.hasDies(cause)) {
    return { outcome: "fail", failureClass: "Defect" }
  }
  return {
    outcome: "fail",
    failureClass: Option.match(Cause.findErrorOption(cause), {
      onNone: () => "UnknownFailure",
      onSome: tagOf,
    }),
  }
}

const classify = <A, E>(exit: Exit.Exit<A, E>): Verdict =>
  Exit.match(exit, {
    onSuccess: () => ({ outcome: "pass" }),
    onFailure: classifyCause,
  })

const validate = (candidate: EvidenceEnvelope): Effect.Effect<EvidenceEnvelope, EvidenceError> =>
  Effect.tryPromise({
    try: () => Promise.resolve(Schema.decodeUnknownSync(EvidenceEnvelope)(candidate)),
    catch: (error) =>
      new EvidenceError({
        testId: candidate.testId,
        reason: `invalid evidence envelope: ${
          error instanceof Error ? error.message : String(error)
        }`,
      }),
  })

export const runEvidence = <A, E, R>(
  run: EvidenceRun<A, E, R>,
): Effect.Effect<
  EvidenceEnvelope,
  EvidenceError | FixtureError | ArtifactError | DigestError,
  ArtifactStore | FixtureStore | EnvironmentProbe | R
> =>
  Effect.gen(function* () {
    const store = yield* ArtifactStoreTag
    const fixtures = yield* FixtureStoreTag
    const environmentProbe = yield* EnvironmentProbeTag

    const fixture = yield* fixtures.load(run.fixture.id, run.fixture.version)
    const capture = yield* makeEvidenceCapture(store)
    const exit = yield* Effect.exit(run.body(fixture, capture))
    const captured = yield* capture.snapshot
    const environment = yield* environmentProbe.fingerprint
    const verdict = classify(exit)

    const candidate: EvidenceEnvelope = {
      testId: run.testId,
      invariantIds: [...run.invariantIds],
      fixtureVersion: fixtureVersion(run.fixture),
      canonicalHead: run.fixture.canonicalHead,
      sequenceRange: run.fixture.sequenceRange,
      environment,
      seeds: [...run.seeds],
      modelRecordings: [...captured.modelRecordings],
      artifacts: [...captured.artifacts],
      outcome: verdict.outcome,
      ...(verdict.failureClass === undefined ? {} : { failureClass: verdict.failureClass }),
      ...(run.metrics === undefined ? {} : { metrics: { ...run.metrics } }),
      ...(run.acceptedResiduals === undefined
        ? {}
        : { acceptedResiduals: [...run.acceptedResiduals] }),
    }

    return yield* validate(candidate)
  })
