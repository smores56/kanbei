import { DenoServices } from "@effect/platform-deno"
import { Console, Data, Effect, FileSystem, Layer, Path } from "effect"
import { fileArtifactStoreLayer } from "../evidence/artifact-store.ts"
import { canonicalize } from "../evidence/canonical.ts"
import { sha256Text } from "../evidence/digest.ts"
import { dryRunJourney } from "../evidence/dry-run.ts"
import { liveEnvironmentProbeLayer } from "../evidence/environment.ts"
import { EvidenceError } from "../evidence/errors.ts"
import { fixtureStoreLayer } from "../evidence/fixture-store.ts"
import { runEvidence } from "../evidence/harness.ts"
import type { EvidenceEnvelope, FixtureReference } from "../evidence/types.ts"

class SimulatedFailure extends Data.TaggedError("SimulatedFailure")<{
  readonly reason: string
}> {}

const journeyReference: FixtureReference = {
  id: "journey-v1",
  version: "1.0.0",
  canonicalHead: {
    seq: 12,
    hash: "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  },
  sequenceRange: [1, 12],
}

const layers = (root: string) =>
  Layer.mergeAll(
    fileArtifactStoreLayer(`${root}/tmp/evidence-cas`),
    fixtureStoreLayer(root),
    liveEnvironmentProbeLayer,
  ).pipe(Layer.provide(DenoServices.layer))

const runOnce = Effect.gen(function* () {
  return yield* runEvidence({
    testId: "A0-DRY-RUN",
    testClass: "conformance",
    invariantIds: [],
    fixture: journeyReference,
    seeds: [0],
    body: dryRunJourney,
    metrics: { projects: 3 },
  })
})

const envelopeText = (envelope: EvidenceEnvelope): string =>
  `${JSON.stringify(envelope, null, 2)}\n`

const writeEnvelope = (
  root: string,
  envelope: EvidenceEnvelope,
): Effect.Effect<string, unknown, FileSystem.FileSystem | Path.Path> =>
  Effect.gen(function* () {
    const fs = yield* FileSystem.FileSystem
    const path = yield* Path.Path
    const body = envelopeText(envelope)
    const digest = yield* sha256Text(body).pipe(
      Effect.catch(() => Effect.succeed("sha256:unavailable")),
    )
    const outputDirectory = path.join(root, "tmp/evidence")
    const output = path.join(outputDirectory, `${envelope.testId}-${digest.slice(7, 19)}.json`)
    yield* fs.makeDirectory(outputDirectory, { recursive: true })
    yield* fs.writeFileString(output, body)
    return output
  })

const dryRun = (root: string) =>
  Effect.gen(function* () {
    const envelope = yield* runOnce
    const output = yield* writeEnvelope(root, envelope)
    yield* Console.log(envelopeText(envelope).trimEnd())
    yield* Console.log(`evidence: ${output}`)
  })

const determinism = (root: string) =>
  Effect.gen(function* () {
    const first = yield* runOnce
    const second = yield* runOnce
    const firstText = envelopeText(first)
    const secondText = envelopeText(second)
    if (firstText !== secondText) {
      return yield* Effect.fail(
        new EvidenceError({
          testId: "A0-DETERMINISM",
          reason: "two identical runs produced different envelopes",
        }),
      )
    }
    const output = yield* writeEnvelope(root, first)
    yield* Console.log(`deterministic: ${canonicalize({ equal: true, output })}`)
  })

const failure = (root: string) =>
  Effect.gen(function* () {
    const envelope = yield* runEvidence({
      testId: "A0-FAILURE-CLASS",
      testClass: "unit",
      invariantIds: [],
      fixture: journeyReference,
      seeds: [1],
      body: () => Effect.fail(new SimulatedFailure({ reason: "expected test failure" })),
      metrics: { projects: 3 },
    })
    const output = yield* writeEnvelope(root, envelope)
    yield* Console.log(envelopeText(envelope).trimEnd())
    yield* Console.log(`evidence: ${output}`)
  })

const command = Deno.args[0] ?? "dry-run"
const root = Deno.cwd()

const program = (() => {
  switch (command) {
    case "dry-run":
      return dryRun(root)
    case "determinism":
      return determinism(root)
    case "failure":
      return failure(root)
    default:
      return Effect.fail(
        new EvidenceError({ testId: "A0-CLI", reason: `unknown command: ${command}` }),
      )
  }
})().pipe(Effect.provide(layers(root)), Effect.provide(DenoServices.layer))

await Effect.runPromise(program)
