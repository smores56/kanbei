import { DenoServices } from "@effect/platform-deno"
import { Console, Effect, FileSystem, Layer, Path } from "effect"
import { liveCommandRunnerLayer } from "./process.ts"
import { renderReport } from "./report.ts"
import { runBakeoff } from "./run.ts"

const program = Effect.gen(function* () {
  const root = Deno.cwd()
  const result = yield* runBakeoff(root)
  const report = renderReport(result)
  const fs = yield* FileSystem.FileSystem
  const path = yield* Path.Path
  const outputDirectory = path.join(root, "spikes/language")
  yield* fs.makeDirectory(outputDirectory, { recursive: true })
  yield* fs.writeFileString(
    path.join(outputDirectory, "result.json"),
    `${JSON.stringify(result, null, 2)}\n`,
  )
  yield* fs.writeFileString(path.join(outputDirectory, "REPORT.md"), report)

  const measured = result.localMeasurements.runtimes.filter(
    (runtime) => runtime.resolution.status === "available",
  )
  yield* Console.log(
    `S1 LANG-1 outcome=${result.evidence.outcome} gate=${result.decisionRecord.gate} ` +
      `recommendation=${result.decisionRecord.recommendation} ` +
      `runtimeVariants=${measured.length}/${result.localMeasurements.runtimes.length} ` +
      `modelCells=${result.modelMatrix.plannedCells} completedModelTrials=${
        result.modelMatrix.cells.reduce(
          (total, modelCell) => total + modelCell.trials,
          0,
        )
      }`,
  )
  yield* Console.log(`report: ${path.join(outputDirectory, "REPORT.md")}`)
  yield* Console.log(`result: ${path.join(outputDirectory, "result.json")}`)
})

await Effect.runPromise(
  program.pipe(Effect.provide(Layer.mergeAll(liveCommandRunnerLayer, DenoServices.layer))),
)
