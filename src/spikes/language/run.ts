import { Effect, FileSystem, Option, Path, Schema } from "effect"
import { canonicalize } from "../../evidence/canonical.ts"
import type { Json } from "../../evidence/canonical.ts"
import { sha256Text } from "../../evidence/digest.ts"
import { bashArm } from "./arms/bash.ts"
import type { ArmDefinition } from "./arms/contract.ts"
import { javascriptArms } from "./arms/javascript.ts"
import { luaArms } from "./arms/lua.ts"
import { montyArm } from "./arms/monty.ts"
import { cpythonArm } from "./arms/python.ts"
import { measureArm } from "./measurement.ts"
import type { ArmMeasurement } from "./measurement.ts"
import { buildModelMatrix, ModelManifest } from "./model-arm.ts"
import type { ModelMatrix } from "./model-arm.ts"
import { progressiveDisclosureRows } from "./progressive-disclosure.ts"
import type { BakeoffEnvironment, LocalMeasurements, TaskSuiteFacts } from "./result.ts"
import { buildResult } from "./result.ts"
import type { BakeoffResult } from "./result.ts"
import { FAILURE_TAXONOMY } from "./taxonomy.ts"
import { TASK_SUITE, TASKS } from "./tasks.ts"
import { CommandRunner } from "./process.ts"

export const ARMS: ReadonlyArray<ArmDefinition> = [
  montyArm,
  ...javascriptArms,
  ...luaArms,
  bashArm,
  cpythonArm,
]

export const CANARY = "kanbei-bakeoff-canary-7f3a1c9e"

const FixtureRegistry = Schema.Struct({
  datasets: Schema.Array(Schema.Struct({ id: Schema.String, version: Schema.String })),
})

const readOptional = (
  path: string,
): Effect.Effect<Option.Option<string>, never, FileSystem.FileSystem> =>
  Effect.gen(function* () {
    const fs = yield* FileSystem.FileSystem
    const exists = yield* fs.exists(path).pipe(Effect.catch(() => Effect.succeed(false)))
    if (!exists) return Option.none()
    const text = yield* fs.readFileString(path).pipe(Effect.catch(() => Effect.succeed("")))
    return text.length === 0 ? Option.none() : Option.some(text)
  })

const registeredDatasets = (
  root: string,
): Effect.Effect<Readonly<Record<string, boolean>>, never, FileSystem.FileSystem | Path.Path> =>
  Effect.gen(function* () {
    const path = yield* Path.Path
    const text = yield* readOptional(path.join(root, "fixtures/registry.json"))
    if (Option.isNone(text)) return {}
    return yield* Effect.try({
      try: () => {
        const registry = Schema.decodeUnknownSync(FixtureRegistry)(JSON.parse(text.value))
        return Object.fromEntries(
          registry.datasets.map((dataset) => [`${dataset.id}@${dataset.version}`, true]),
        )
      },
      catch: () => ({}),
    }).pipe(Effect.catch(() => Effect.succeed({})))
  })

const modelManifest = (
  root: string,
): Effect.Effect<ModelManifest | undefined, never, FileSystem.FileSystem | Path.Path> =>
  Effect.gen(function* () {
    const path = yield* Path.Path
    const text = yield* readOptional(path.join(root, "spikes/language/model-manifest.json"))
    if (Option.isNone(text)) return undefined
    return yield* Effect.try({
      try: () => Schema.decodeUnknownSync(ModelManifest)(JSON.parse(text.value)),
      catch: () => undefined,
    }).pipe(Effect.catch(() => Effect.succeed(undefined)))
  })

const environment = (): Effect.Effect<BakeoffEnvironment> =>
  Effect.gen(function* () {
    const hostFacts = {
      hostname: Deno.hostname(),
      os: Deno.build.os,
      arch: Deno.build.arch,
      release: Deno.osRelease(),
      cpus: navigator.hardwareConcurrency,
    }
    const fingerprint = yield* sha256Text(canonicalize(hostFacts)).pipe(
      Effect.catch(() => Effect.succeed("sha256:unavailable")),
    )
    return {
      hostFingerprint: fingerprint,
      kernel: `${Deno.build.os}-${Deno.osRelease()}`,
      runtime: `deno@${Deno.version.deno}`,
      sandboxPosture: "unverified",
      brokerIdentity: "unconfigured",
      hostname: hostFacts.hostname,
      cpus: hostFacts.cpus,
    }
  })

const taskSuiteJson = (): Json =>
  TASKS.map((task) => ({
    id: task.id,
    ordinal: task.ordinal,
    title: task.title,
    category: task.category,
    description: task.description,
    hostCallPattern: task.hostCallPattern,
    goldOutputJson: task.goldOutputJson,
    rubric: task.rubric.map((criterion) => ({ id: criterion.id, criterion: criterion.criterion })),
    requiredCapabilities: [...task.requiredCapabilities],
    adversarial: task.adversarial,
    ...(task.secretCanary === undefined ? {} : { secretCanary: task.secretCanary }),
  }))

const taskSuiteFacts = (): Effect.Effect<TaskSuiteFacts> =>
  Effect.gen(function* () {
    const digest = yield* sha256Text(JSON.stringify(taskSuiteJson())).pipe(
      Effect.catch(() => Effect.succeed("sha256:unavailable")),
    )
    return {
      suiteId: TASK_SUITE.suiteId,
      suiteVersion: TASK_SUITE.suiteVersion,
      digest,
      tasks: TASKS,
    }
  })

const localMeasurements = (runs: ReadonlyArray<ArmMeasurement>): LocalMeasurements => ({
  runtimes: runs.map((run) => run.measurement),
  hostCalls: runs.flatMap((run) => run.hostCalls),
  progressiveDisclosure: progressiveDisclosureRows(),
  security: runs.flatMap((run) => run.security),
  snapshot: runs.map((run) => run.snapshot),
})

const modelMatrix = (
  root: string,
): Effect.Effect<ModelMatrix, never, FileSystem.FileSystem | Path.Path> =>
  Effect.gen(function* () {
    const manifest = yield* modelManifest(root)
    const datasets = yield* registeredDatasets(root)
    return buildModelMatrix({
      manifest,
      credentials: {
        NEURALWATT_API_KEY: (Deno.env.get("NEURALWATT_API_KEY") ?? "").length > 0,
      },
      sandboxFrozen: false,
      registeredDatasets: datasets,
    })
  })

const temporaryDirectory = (): Effect.Effect<string> =>
  Effect.tryPromise({
    try: () => Deno.makeTempDir({ prefix: "kanbei-lang-bakeoff-" }),
    catch: () => new Error("cannot create temporary directory"),
  }).pipe(Effect.orDie)

export const runBakeoff = (
  root: string,
): Effect.Effect<BakeoffResult, never, CommandRunner | FileSystem.FileSystem | Path.Path> =>
  Effect.scoped(Effect.gen(function* () {
    const path = yield* Path.Path
    const fs = yield* FileSystem.FileSystem
    const scoped = yield* Effect.acquireRelease(
      temporaryDirectory(),
      (directory) =>
        Effect.tryPromise({
          try: () => Deno.remove(directory, { recursive: true }),
          catch: () => new Error("cannot remove temporary directory"),
        }).pipe(Effect.ignore),
    )

    const secrets = path.join(scoped, "secrets")
    yield* fs.makeDirectory(secrets, { recursive: true }).pipe(Effect.ignore)
    const canaryFile = path.join(secrets, "canary.txt")
    yield* fs.writeFileString(canaryFile, CANARY).pipe(Effect.ignore)
    const runsDirectory = path.join(scoped, "runs")
    yield* fs.makeDirectory(runsDirectory, { recursive: true }).pipe(Effect.ignore)

    const runs = yield* Effect.forEach(
      ARMS,
      (arm) => measureArm(arm, runsDirectory, CANARY, canaryFile),
      { concurrency: 1 },
    )

    const matrix = yield* modelMatrix(root)
    const result = yield* buildResult({
      environment: yield* environment(),
      taskSuite: yield* taskSuiteFacts(),
      local: localMeasurements(runs),
      modelMatrix: matrix,
      failureTaxonomy: FAILURE_TAXONOMY,
      generatedBy: "src/spikes/language (S1 LANG-1 harness)",
    }).pipe(Effect.orDie)
    return result
  }))
