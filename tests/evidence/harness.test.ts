import { assertEquals, assertRejects } from "@std/assert"
import { DenoServices } from "@effect/platform-deno"
import { Data, Effect, Layer } from "effect"
import { fileArtifactStoreLayer } from "../../src/evidence/artifact-store.ts"
import { dryRunJourney } from "../../src/evidence/dry-run.ts"
import { fixedEnvironmentProbeLayer } from "../../src/evidence/environment.ts"
import { FixtureError } from "../../src/evidence/errors.ts"
import { FixtureStore, fixtureStoreLayer } from "../../src/evidence/fixture-store.ts"
import { runEvidence } from "../../src/evidence/harness.ts"
import type { EvidenceEnvelope, FixtureReference } from "../../src/evidence/types.ts"

class SimulatedFailure extends Data.TaggedError("SimulatedFailure")<Record<PropertyKey, never>> {}

const reference: FixtureReference = {
  id: "journey-v2",
  version: "1.0.0",
  canonicalHead: { seq: 24, hash: "sha256:" + "0".repeat(64) },
  sequenceRange: [1, 24],
}

const environmentLayer = fixedEnvironmentProbeLayer({
  hostFingerprint: "sha256:" + "b".repeat(64),
  kernel: "linux-test",
  runtime: "deno@test",
  sandboxPosture: "test",
  brokerIdentity: "test",
})

const dependencies = (root: string, casRoot: string) =>
  Layer.mergeAll(
    fileArtifactStoreLayer(casRoot),
    fixtureStoreLayer(root),
    environmentLayer,
  ).pipe(Layer.provide(DenoServices.layer))

const runEvidenceOnce = (testId: string) =>
  runEvidence({
    testId,
    testClass: "conformance",
    invariantIds: [],
    fixture: reference,
    seeds: [7],
    body: dryRunJourney,
  })

Deno.test("evidence harness is deterministic for identical inputs", async () => {
  const temp = await Deno.makeTempDir({ prefix: "kanbei-evidence-" })
  try {
    const [first, second] = await Effect.runPromise(
      Effect.all([runEvidenceOnce("A0-TEST"), runEvidenceOnce("A0-TEST")], {
        concurrency: 1,
      }).pipe(Effect.provide(dependencies(Deno.cwd(), temp))),
    )
    assertEquals(first.outcome, "pass")
    assertEquals(JSON.stringify(first), JSON.stringify(second))
  } finally {
    await Deno.remove(temp, { recursive: true })
  }
})

Deno.test("evidence harness records failure class without losing evidence", async () => {
  const temp = await Deno.makeTempDir({ prefix: "kanbei-evidence-" })
  try {
    const envelope: EvidenceEnvelope = await Effect.runPromise(
      runEvidence({
        testId: "A0-FAILURE",
        testClass: "unit",
        invariantIds: [],
        fixture: reference,
        seeds: [8],
        body: (_fixture, capture) =>
          Effect.gen(function* () {
            yield* capture.putArtifactText("captured before failure")
            return yield* Effect.fail(new SimulatedFailure({}))
          }),
      }).pipe(Effect.provide(dependencies(Deno.cwd(), temp))),
    )
    assertEquals(envelope.outcome, "fail")
    assertEquals(envelope.failureClass, "SimulatedFailure")
    assertEquals(envelope.artifacts.length, 1)
  } finally {
    await Deno.remove(temp, { recursive: true })
  }
})

Deno.test("fixture store rejects a tampered sealed fixture", async () => {
  const temp = await Deno.makeTempDir({ prefix: "kanbei-fixture-" })
  try {
    await new Deno.Command("cp", { args: ["-R", `${Deno.cwd()}/fixtures`, `${temp}/fixtures`] })
      .output()
    await Deno.writeTextFile(`${temp}/fixtures/journey-v2/fixture.json`, "{}\n")

    const program = Effect.gen(function* () {
      const store = yield* FixtureStore
      return yield* store.load("journey-v2", "1.0.0")
    }).pipe(Effect.provide(dependencies(temp, `${temp}/cas`)))

    await assertRejects(() => Effect.runPromise(program), FixtureError)
  } finally {
    await Deno.remove(temp, { recursive: true })
  }
})
