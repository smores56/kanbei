import { DenoServices } from "@effect/platform-deno"
import { Effect, FileSystem, Layer, Schema } from "effect"
import { fileArtifactStoreLayer } from "../../evidence/artifact-store.ts"
import { fixtureStoreLayer } from "../../evidence/fixture-store.ts"
import { liveEnvironmentProbeLayer } from "../../evidence/environment.ts"
import { sealDeploymentEvidence, securityFixtureReference } from "./envelope.ts"
import { renderReport } from "./report.ts"
import { runSpike } from "./matrix.ts"
import { probeEnvLayer } from "./probe-env.ts"
import { SpikeResult } from "./types.ts"

const registrySchema = Schema.Struct({
  datasets: Schema.Array(Schema.Struct({ id: Schema.String, version: Schema.String })),
})

const datasetVersion = (root: string, id: string) =>
  Effect.gen(function* () {
    const fs = yield* FileSystem.FileSystem
    const text = yield* fs.readFileString(`${root}/fixtures/registry.json`)
    const registry = Schema.decodeUnknownSync(registrySchema)(JSON.parse(text))
    return registry.datasets.find((dataset) => dataset.id === id)?.version ?? "1.0.0"
  })

const program = (root: string) =>
  Effect.gen(function* () {
    const scratch = yield* Effect.promise(() => Deno.makeTempDir({ prefix: "kanbei-dep1-" }))
    const artifact = yield* Effect.gen(function* () {
      const result = yield* runSpike.pipe(Effect.provide(probeEnvLayer(scratch)))
      const version = yield* datasetVersion(root, "security-v1")
      // The same-UID probes measured no secret isolation; `fail` is the finding.
      const envelope = yield* sealDeploymentEvidence(
        result,
        securityFixtureReference(version),
        "fail",
      ).pipe(
        Effect.provide(
          Layer.mergeAll(
            fileArtifactStoreLayer(`${root}/tmp/evidence-cas`),
            fixtureStoreLayer(root),
            liveEnvironmentProbeLayer,
          ),
        ),
      )
      // The emitted artifact must satisfy the published schema, not just look right.
      return yield* Schema.decodeUnknownEffect(SpikeResult)({ ...result, envelope })
    }).pipe(
      Effect.ensuring(
        Effect.promise(() => Deno.remove(scratch, { recursive: true }).catch(() => undefined)),
      ),
      Effect.provide(DenoServices.layer),
    )
    const report = renderReport(artifact)
    yield* Effect.promise(async () => {
      await Deno.mkdir(`${root}/spikes/deployment`, { recursive: true })
      await Deno.writeTextFile(
        `${root}/spikes/deployment/result.json`,
        `${JSON.stringify(artifact, null, 2)}\n`,
      )
      await Deno.writeTextFile(`${root}/spikes/deployment/REPORT.md`, report)
    })
    const failures = artifact.probes.filter((probe) => probe.status === "fail").length
    yield* Effect.log(
      `DEP-1 complete: ${artifact.probes.length} probes, ${failures} measured failures, envelope ${artifact.envelope.fixtureVersion} (${artifact.envelope.outcome})`,
    )
  })

await Effect.runPromise(program(Deno.cwd()))
