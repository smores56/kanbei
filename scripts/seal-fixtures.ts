import { DenoFileSystem, DenoPath } from "@effect/platform-deno"
import { Effect, FileSystem, Layer, Path } from "effect"
import { sha256 } from "../src/evidence/digest.ts"
import { FixtureError } from "../src/evidence/errors.ts"
import { readRegisteredDatasets, treeHashForFiles } from "../src/evidence/fixture-store.ts"

const listFiles = (
  fs: FileSystem.FileSystem,
  path: Path.Path,
  directory: string,
): Effect.Effect<ReadonlyArray<string>, FixtureError> =>
  Effect.gen(function* () {
    const names = yield* fs.readDirectory(directory).pipe(
      Effect.mapError(
        (error) =>
          new FixtureError({
            dataset: "seal",
            version: "1",
            reason: `cannot list ${directory}: ${
              error instanceof Error ? error.message : String(error)
            }`,
          }),
      ),
    )

    const nested = yield* Effect.forEach(
      [...names].toSorted(),
      (name) =>
        Effect.gen(function* () {
          const target = path.join(directory, name)
          const info = yield* fs.lstat(target).pipe(
            Effect.mapError(
              (error) =>
                new FixtureError({
                  dataset: "seal",
                  version: "1",
                  reason: `cannot stat ${target}: ${
                    error instanceof Error ? error.message : String(error)
                  }`,
                }),
            ),
          )
          if (info.type === "Directory") return yield* listFiles(fs, path, target)
          if (info.type === "File") return [target]
          return []
        }),
      { concurrency: 8 },
    )

    return nested.flat()
  })

const hashFiles = (
  fs: FileSystem.FileSystem,
  path: Path.Path,
  datasetRoot: string,
  files: ReadonlyArray<string>,
): Effect.Effect<Readonly<Record<string, string>>, FixtureError> =>
  Effect.gen(function* () {
    const entries = yield* Effect.forEach(
      files,
      (file) =>
        Effect.gen(function* () {
          const relativePath = path.relative(datasetRoot, file).replaceAll("\\", "/")
          const bytes = yield* fs.readFile(file).pipe(
            Effect.mapError(
              (error) =>
                new FixtureError({
                  dataset: datasetRoot,
                  version: "1",
                  reason: `cannot read ${file}: ${
                    error instanceof Error ? error.message : String(error)
                  }`,
                }),
            ),
          )
          const digest = yield* sha256(bytes).pipe(
            Effect.mapError(
              (error) =>
                new FixtureError({ dataset: datasetRoot, version: "1", reason: error.reason }),
            ),
          )
          return [relativePath, digest] as const
        }),
      { concurrency: 8 },
    )

    return Object.fromEntries(entries)
  })

const program = Effect.gen(function* () {
  const fs = yield* FileSystem.FileSystem
  const path = yield* Path.Path
  const root = Deno.cwd()
  const datasets = yield* readRegisteredDatasets(fs, path, root)

  const manifestEntries = yield* Effect.forEach(
    datasets,
    (dataset) =>
      Effect.gen(function* () {
        const datasetRoot = path.join(root, dataset.path)
        const files = yield* listFiles(fs, path, datasetRoot)
        const digests = yield* hashFiles(fs, path, datasetRoot, files)
        const treeHash = yield* treeHashForFiles(digests)
        return [
          `${dataset.id}@${dataset.version}`,
          {
            treeHash,
            files: Object.fromEntries(
              Object.entries(digests).toSorted(([a], [b]) => a.localeCompare(b)),
            ),
          },
        ] as const
      }),
    { concurrency: 2 },
  )

  const manifest = {
    schemaVersion: 1,
    generatedBy: "deno task fixtures:seal",
    datasets: Object.fromEntries(manifestEntries),
  }
  const manifestPath = path.join(root, "fixtures/manifest.json")
  const body = `${JSON.stringify(manifest, null, 2)}\n`
  yield* fs.writeFileString(manifestPath, body).pipe(
    Effect.mapError(
      (error) =>
        new FixtureError({
          dataset: "manifest",
          version: "1",
          reason: `cannot write ${manifestPath}: ${
            error instanceof Error ? error.message : String(error)
          }`,
        }),
    ),
  )
  yield* Effect.log(`sealed ${manifestEntries.length} fixture datasets`)
})

const live = Layer.mergeAll(DenoFileSystem.layer, DenoPath.layer)

await Effect.runPromise(program.pipe(Effect.provide(live)))
