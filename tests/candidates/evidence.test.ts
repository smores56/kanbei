import { assertEquals } from "@std/assert"
import { DenoServices } from "@effect/platform-deno"
import { Effect, Layer, Option } from "effect"
import { fileBlobSinkLayer } from "../../src/adapters/blob-sink.ts"
import { fileLedgerStoreLayer } from "../../src/adapters/ledger-file.ts"
import { pointerFileLayer } from "../../src/adapters/pointer-file.ts"
import { artifact, inputs, workspace } from "./support.ts"
import { CandidateSupervisor, candidateSupervisorLayer } from "../../src/candidates/supervisor.ts"
import { fileArtifactStoreLayer } from "../../src/evidence/artifact-store.ts"
import { fixedEnvironmentProbeLayer } from "../../src/evidence/environment.ts"
import { fixtureStoreLayer } from "../../src/evidence/fixture-store.ts"
import { runEvidence } from "../../src/evidence/harness.ts"
import type { FixtureReference } from "../../src/evidence/types.ts"
import { contentStoreLayer } from "../../src/kernel/cas.ts"
import { ledgerLayer } from "../../src/kernel/ledger.ts"
import { withTempDir } from "../support.ts"

const reference: FixtureReference = {
  id: "journey-v1",
  version: "1.0.0",
  canonicalHead: { seq: 1, hash: `sha256:${"c".repeat(64)}` },
  sequenceRange: [1, 1],
}

const kernel = (directory: string) =>
  Layer.mergeAll(
    ledgerLayer("epoch-1").pipe(Layer.provide(fileLedgerStoreLayer(`${directory}/ledger.jsonl`))),
    contentStoreLayer.pipe(Layer.provide(fileBlobSinkLayer(`${directory}/cas`))),
    pointerFileLayer(`${directory}/pointers`),
  )

Deno.test("candidate approval emits an evidence envelope for ZT-08, ZT-10, and I-14", async () => {
  await withTempDir("candidate-evidence", async (directory) => {
    const kernelLayers = kernel(directory)
    const dependencies = Layer.mergeAll(
      fileArtifactStoreLayer(`${directory}/artifacts`),
      fixtureStoreLayer(Deno.cwd()),
      fixedEnvironmentProbeLayer({
        hostFingerprint: `sha256:${"d".repeat(64)}`,
        kernel: "linux-test",
        runtime: "deno@test",
        sandboxPosture: "test",
        brokerIdentity: "test",
      }),
      candidateSupervisorLayer.pipe(Layer.provide(kernelLayers)),
    ).pipe(Layer.provide(DenoServices.layer))

    const envelope = await Effect.runPromise(
      runEvidence({
        testId: "T34-CANDIDATE-APPROVAL",
        testClass: "conformance",
        invariantIds: ["ZT-08", "ZT-10", "I-14"],
        fixture: reference,
        seeds: [608],
        body: (fixture, captureStore) =>
          Effect.gen(function* () {
            yield* captureStore.putArtifactText(`fixture:${fixture.treeHash}`)
            const supervisor = yield* CandidateSupervisor
            yield* supervisor.stage(workspace(), artifact())
            yield* supervisor.validate("cand-1", inputs)
            const request = yield* supervisor.requestApproval("cand-1", "human-1", 60_000_000)
            yield* supervisor.approve("cand-1", {
              token: request.token,
              consentDigest: request.display.consentDigest,
              candidateDigest: request.display.manifest.candidateDigest,
              sessionId: "human-1",
            })
            const activated = yield* supervisor.activate("cand-1", "pass")
            const pointer = yield* supervisor.pointer("project/kanbei")
            return {
              activated: activated.state,
              candidate: Option.getOrThrow(pointer).candidateId,
            }
          }),
        metrics: { candidates: 1 },
        acceptedResiduals: ["activation crash matrix deferred to the supervision harness"],
      }).pipe(Effect.provide(dependencies)),
    )

    assertEquals(envelope.outcome, "pass")
    assertEquals(envelope.invariantIds, ["ZT-08", "ZT-10", "I-14"])
    assertEquals(envelope.fixtureVersion, "journey-v1@1.0.0")
    assertEquals(envelope.metrics?.candidates, 1)
  })
})
