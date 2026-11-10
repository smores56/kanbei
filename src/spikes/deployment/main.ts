import { Effect, Schema } from "effect"
import { renderReport } from "./report.ts"
import { runSpike } from "./matrix.ts"
import { probeEnvLayer } from "./probe-env.ts"
import { SpikeResult } from "./types.ts"

const program = (root: string) =>
  Effect.gen(function* () {
    const scratch = yield* Effect.promise(() => Deno.makeTempDir({ prefix: "kanbei-dep1-" }))
    const result = yield* runSpike.pipe(
      Effect.provide(probeEnvLayer(scratch)),
      Effect.ensuring(
        Effect.promise(() => Deno.remove(scratch, { recursive: true }).catch(() => undefined)),
      ),
    )
    // The emitted artifact must satisfy the published schema, not just look right.
    const artifact = yield* Schema.decodeUnknownEffect(SpikeResult)(result)
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
      `DEP-1 complete: ${artifact.probes.length} probes, ${failures} measured failures`,
    )
  })

await Effect.runPromise(program(Deno.cwd()))
