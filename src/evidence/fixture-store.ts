import { Context, Effect, FileSystem, Layer, Path, Schema } from "effect"
import { canonicalize } from "./canonical.ts"
import type { Json } from "./canonical.ts"
import { sha256, sha256Text } from "./digest.ts"
import { FixtureError } from "./errors.ts"

const DatasetRegistration = Schema.Struct({
  id: Schema.String,
  version: Schema.String,
  path: Schema.String,
})
type DatasetRegistration = typeof DatasetRegistration.Type

const Registry = Schema.Struct({
  schemaVersion: Schema.Literal(1),
  datasets: Schema.Array(DatasetRegistration),
})
type Registry = typeof Registry.Type

const ManifestEntry = Schema.Struct({
  treeHash: Schema.String,
  files: Schema.Record(Schema.String, Schema.String),
})
type ManifestEntry = typeof ManifestEntry.Type

const Manifest = Schema.Struct({
  schemaVersion: Schema.Literal(1),
  generatedBy: Schema.String,
  datasets: Schema.Record(Schema.String, ManifestEntry),
})
type Manifest = typeof Manifest.Type

export interface FixtureSnapshot {
  readonly id: string
  readonly version: string
  readonly path: string
  readonly treeHash: string
  readonly files: Readonly<Record<string, string>>
}

export interface FixtureStore {
  readonly load: (id: string, version: string) => Effect.Effect<FixtureSnapshot, FixtureError>
}

export const FixtureStore = Context.Service<FixtureStore>("kanbei/evidence/FixtureStore")

const readJsonFile = (
  fs: FileSystem.FileSystem,
  path: Path.Path,
  root: string,
  relativePath: string,
  dataset: string,
  version: string,
): Effect.Effect<Json, FixtureError> => {
  const target = path.join(root, relativePath)
  return fs.readFileString(target).pipe(
    Effect.mapError(
      (error) =>
        new FixtureError({
          dataset,
          version,
          reason: `cannot read ${relativePath}: ${
            error instanceof Error ? error.message : String(error)
          }`,
        }),
    ),
    Effect.flatMap((text) =>
      Effect.tryPromise({
        try: () => Promise.resolve(Schema.decodeUnknownSync(Schema.Json)(JSON.parse(text))),
        catch: (error) =>
          new FixtureError({
            dataset,
            version,
            reason: `invalid ${relativePath}: ${
              error instanceof Error ? error.message : String(error)
            }`,
          }),
      })
    ),
  )
}

const decodeRegistry = (json: Json): Effect.Effect<Registry, FixtureError> =>
  Effect.tryPromise({
    try: () => Promise.resolve(Schema.decodeUnknownSync(Registry)(json)),
    catch: (error) =>
      new FixtureError({
        dataset: "registry",
        version: "1",
        reason: `invalid registry: ${error instanceof Error ? error.message : String(error)}`,
      }),
  })

const decodeManifest = (json: Json): Effect.Effect<Manifest, FixtureError> =>
  Effect.tryPromise({
    try: () => Promise.resolve(Schema.decodeUnknownSync(Manifest)(json)),
    catch: (error) =>
      new FixtureError({
        dataset: "manifest",
        version: "1",
        reason: `invalid manifest: ${error instanceof Error ? error.message : String(error)}`,
      }),
  })

const readRegistry = (
  fs: FileSystem.FileSystem,
  path: Path.Path,
  root: string,
): Effect.Effect<Registry, FixtureError> =>
  readJsonFile(fs, path, root, "fixtures/registry.json", "registry", "1").pipe(
    Effect.flatMap(decodeRegistry),
  )

const readManifest = (
  fs: FileSystem.FileSystem,
  path: Path.Path,
  root: string,
): Effect.Effect<Manifest, FixtureError> =>
  readJsonFile(fs, path, root, "fixtures/manifest.json", "manifest", "1").pipe(
    Effect.flatMap(decodeManifest),
  )

const computeTreeHash = (
  files: Readonly<Record<string, string>>,
): Effect.Effect<string, FixtureError> =>
  sha256Text(canonicalize(files)).pipe(
    Effect.mapError((error) =>
      new FixtureError({ dataset: "tree", version: "1", reason: error.reason })
    ),
  )

const make = (
  fs: FileSystem.FileSystem,
  path: Path.Path,
  root: string,
): FixtureStore => ({
  load: (id, version) =>
    Effect.gen(function* () {
      const registry = yield* readRegistry(fs, path, root)
      const dataset = registry.datasets.find((entry) =>
        entry.id === id && entry.version === version
      )
      if (dataset === undefined) {
        return yield* Effect.fail(
          new FixtureError({ dataset: id, version, reason: "dataset is not registered" }),
        )
      }

      const manifest = yield* readManifest(fs, path, root)
      const key = `${id}@${version}`
      const entry = manifest.datasets[key]
      if (entry === undefined) {
        return yield* Effect.fail(
          new FixtureError({ dataset: id, version, reason: "sealed manifest entry is missing" }),
        )
      }

      yield* Effect.forEach(
        Object.entries(entry.files),
        ([relativePath, expected]) =>
          Effect.gen(function* () {
            const target = path.join(root, dataset.path, relativePath)
            const bytes = yield* fs.readFile(target).pipe(
              Effect.mapError(
                (error) =>
                  new FixtureError({
                    dataset: id,
                    version,
                    reason: `cannot read ${relativePath}: ${
                      error instanceof Error ? error.message : String(error)
                    }`,
                  }),
              ),
            )
            const actual = yield* sha256(bytes).pipe(
              Effect.mapError((error) =>
                new FixtureError({ dataset: id, version, reason: error.reason })
              ),
            )
            if (actual !== expected) {
              return yield* Effect.fail(
                new FixtureError({
                  dataset: id,
                  version,
                  reason: `${relativePath} digest mismatch: expected ${expected}, got ${actual}`,
                }),
              )
            }
          }),
        { concurrency: 8 },
      )

      const treeHash = yield* computeTreeHash(entry.files)
      if (treeHash !== entry.treeHash) {
        return yield* Effect.fail(
          new FixtureError({
            dataset: id,
            version,
            reason: `tree digest mismatch: expected ${entry.treeHash}, got ${treeHash}`,
          }),
        )
      }

      return { id, version, path: dataset.path, treeHash, files: entry.files }
    }),
})

export const fixtureStoreLayer = (
  root: string,
): Layer.Layer<FixtureStore, never, FileSystem.FileSystem | Path.Path> =>
  Layer.effect(
    FixtureStore,
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem
      const path = yield* Path.Path
      return make(fs, path, root)
    }),
  )

export const readRegisteredDatasets = (
  fs: FileSystem.FileSystem,
  path: Path.Path,
  root: string,
): Effect.Effect<ReadonlyArray<DatasetRegistration>, FixtureError> =>
  readRegistry(fs, path, root).pipe(Effect.map((registry) => registry.datasets))

export const readSealedManifest = (
  fs: FileSystem.FileSystem,
  path: Path.Path,
  root: string,
): Effect.Effect<Manifest, FixtureError> => readManifest(fs, path, root)

export const treeHashForFiles = computeTreeHash
