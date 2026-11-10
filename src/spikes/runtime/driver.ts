import { Clock, Effect, Schema } from "effect"
import type { LaunchSpec } from "./adapters/launch.ts"
import {
  capture,
  directoryBytes,
  ensureDirectory,
  fileSize,
  launch,
  readTextSync,
  removeIfExists,
  writeText,
} from "./adapters/launch.ts"
import type { ArmConfig, ArmMode } from "./arm.ts"
import { MARKER_DONE, MARKER_PERMISSIONS, MARKER_RESULT, MARKER_SIGNAL } from "./arm.ts"
import { decide } from "./decision.ts"
import type { DecisionOutcome } from "./decision.ts"
import { emptySummary, summarize, toMilliseconds } from "./metrics.ts"
import type { ArmMeasurements, ArmResult, PermissionProbe, RuntimeName } from "./schema.ts"
import {
  ArmMeasurements as ArmMeasurementsSchema,
  PermissionProbe as PermissionProbeSchema,
} from "./schema.ts"
import { PRE_REGISTRATION } from "./plan.ts"

export interface BakeOffOptions {
  readonly repoRoot: string
  readonly workdir: string
  readonly nodeBinary: string
  readonly nodeModules: string
}

export interface BlockedMeasurement {
  readonly arm: RuntimeName
  readonly measurement: string
  readonly reason: string
}

export interface BakeOff {
  readonly arms: Readonly<Record<RuntimeName, ArmResult>>
  readonly blocked: ReadonlyArray<BlockedMeasurement>
  readonly decisionBlocks: ReadonlyArray<string>
  readonly outcome: DecisionOutcome
  readonly unboundProbes: ReadonlyArray<string>
  readonly residualRisks: ReadonlyArray<string>
}

interface ArmTarget {
  readonly arm: RuntimeName
  readonly command: string
  readonly binaryPath: string
  readonly entry: string
  readonly defaultFlags: ReadonlyArray<string>
  readonly restrictedFlags: ReadonlyArray<string>
  readonly env: Readonly<Record<string, string>>
  readonly hookPath: string
}

const BASES = (options: BakeOffOptions): ArmConfig => ({
  workdir: options.workdir,
  checksDir: "",
  dbPath: "",
  warmDbPath: "",
  payloadsPath: `${options.workdir}/payloads.json`,
  workerPath: `${options.repoRoot}/src/spikes/runtime/adapters/worker-entry.ts`,
  appends: PRE_REGISTRATION.load.appends,
  appendChunk: PRE_REGISTRATION.load.appendChunk,
  readsPerChunk: PRE_REGISTRATION.load.readsPerChunk,
  warmStartIterations: PRE_REGISTRATION.sample.warmStartIterations,
  loopSamplerIntervalMs: PRE_REGISTRATION.sample.loopSamplerIntervalMs,
  workerRuns: PRE_REGISTRATION.sample.workerRuns,
})

const configFor = (options: BakeOffOptions, arm: RuntimeName, dbPath: string): ArmConfig => ({
  ...BASES(options),
  checksDir: `${options.workdir}/checks-${arm}`,
  dbPath,
  warmDbPath: `${options.workdir}/warm-${arm}.db`,
})

const nodeHook = (options: BakeOffOptions): string =>
  `${options.repoRoot}/src/spikes/runtime/node/register-hooks.mjs`

const targets = (options: BakeOffOptions): Readonly<Record<RuntimeName, ArmTarget>> => ({
  deno: {
    arm: "deno",
    command: Deno.execPath(),
    binaryPath: Deno.execPath(),
    entry: `${options.repoRoot}/src/spikes/runtime/deno/main.ts`,
    defaultFlags: ["run", "-A", "--no-check"],
    // No permission flags at all: Deno's default posture is deny.
    restrictedFlags: ["run", "--no-check"],
    env: {},
    hookPath: "",
  },
  node: {
    arm: "node",
    command: options.nodeBinary,
    binaryPath: options.nodeBinary,
    entry: `${options.repoRoot}/src/spikes/runtime/node/main.ts`,
    defaultFlags: ["--import", nodeHook(options), "--no-warnings"],
    restrictedFlags: [
      "--import",
      nodeHook(options),
      "--no-warnings",
      "--permission",
      // Repeated flags: Node does not accept a comma-joined read scope here.
      `--allow-fs-read=${options.repoRoot}/src`,
      `--allow-fs-read=${options.nodeModules}`,
    ],
    env: { KANBEI_NODE_MODULES: options.nodeModules },
    hookPath: nodeHook(options),
  },
})

const specFor = (
  target: ArmTarget,
  mode: ArmMode,
  config: ArmConfig,
  restricted: boolean,
): LaunchSpec => ({
  command: target.command,
  args: [
    ...(restricted ? target.restrictedFlags : target.defaultFlags),
    target.entry,
    mode,
    JSON.stringify(config),
  ],
  env: target.env,
})

const section = (stdout: string, marker: string): string => {
  const index = stdout.indexOf(marker)
  if (index < 0) return ""
  const rest = stdout.slice(index + marker.length)
  const newline = rest.indexOf("\n")
  return newline < 0 ? rest : rest.slice(0, newline)
}

const decodeMeasurements = (raw: string): ArmMeasurements | undefined => {
  if (raw === "") return undefined
  try {
    return Schema.decodeUnknownSync(Schema.fromJsonString(ArmMeasurementsSchema))(raw)
  } catch {
    return undefined
  }
}

const decodeProbes = (raw: string): ReadonlyArray<PermissionProbe> => {
  if (raw === "") return []
  try {
    return Schema.decodeUnknownSync(Schema.fromJsonString(Schema.Array(PermissionProbeSchema)))(raw)
  } catch {
    return []
  }
}

const cleanDatabase = (path: string): Effect.Effect<void> =>
  Effect.gen(function* () {
    yield* removeIfExists(path)
    yield* removeIfExists(`${path}-wal`)
    yield* removeIfExists(`${path}-shm`)
  })

const emptyArm = (arm: RuntimeName): ArmResult => ({
  runtime: arm,
  runtimeVersion: "unknown",
  sqliteVersion: "unknown",
  boot: { coldReady: emptySummary, coldStart: emptySummary, warmStart: emptySummary },
  memory: { rssAfterBoot: emptySummary, rssAfterLoadBytes: 0, peakRssBytes: 0 },
  eventLoop: emptySummary,
  ledger: {
    appends: 0,
    appendsPerSecond: 0,
    append: emptySummary,
    read: emptySummary,
    walMode: "unknown",
    readYourWrites: false,
    hashChainValid: false,
  },
  correctness: [],
  singleWriter: {
    concurrentWriterCommitted: false,
    failClosed: false,
    recoveryOk: false,
    busyBlockedEventLoop: false,
    busyBlockMs: 0,
  },
  isolation: {
    workerStartupMs: 0,
    roundtrip: emptySummary,
    crashObserved: false,
    parentSurvivedCrash: false,
  },
  signals: { handlerInvoked: false, shutdownMs: 0, exitCode: -1 },
  permissions: [],
  packaging: {
    runtimeBinaryBytes: 0,
    singleBinaryBytes: 0,
    singleBinaryStatus: "blocked",
    singleBinaryNote: "",
  },
})

export const runBakeOff = (
  options: BakeOffOptions,
  environment: {
    readonly hostFingerprint: string
    readonly kernel: string
    readonly runtime: string
    readonly sandboxPosture: string
    readonly brokerIdentity: string
  },
): Effect.Effect<BakeOff> =>
  Effect.gen(function* () {
    const clock = yield* Clock.Clock
    const now = (): number => Number(clock.monotonicTimeNanosUnsafe()) / 1_000_000
    const targetMap = targets(options)
    const blocked: Array<BlockedMeasurement> = []
    const decisionBlocks: Array<string> = []
    const arms: Record<RuntimeName, ArmResult> = {
      node: emptyArm("node"),
      deno: emptyArm("deno"),
    }

    yield* ensureDirectory(options.workdir)

    const fixtureBodies = yield* Effect.sync(() => {
      const text = readTextSync(`${options.repoRoot}/fixtures/append-v1/fixture.json`)
      const parsed: unknown = JSON.parse(text)
      const cases = typeof parsed === "object" && parsed !== null
        ? Reflect.get(parsed, "cases")
        : []
      const records = Array.isArray(cases)
        ? cases.map((entry) => JSON.stringify(entry)).filter((entry) => entry.length > 0)
        : []
      return records.length > 0 ? records : ['{"recordId":"fallback"}']
    })
    yield* writeText(`${options.workdir}/payloads.json`, JSON.stringify(fixtureBodies))

    const coldStart = (arm: RuntimeName) =>
      Effect.gen(function* () {
        const target = targetMap[arm]
        const runs = yield* Effect.forEach(
          Array.from({ length: PRE_REGISTRATION.sample.coldStartRuns }, (_, index) => index),
          (index) =>
            Effect.gen(function* () {
              const dbPath = `${options.workdir}/cold-${arm}-${index}.db`
              yield* cleanDatabase(dbPath)
              const outcome = yield* launch(
                specFor(target, "boot", configFor(options, arm, dbPath), false),
                { timeoutMs: 120_000 },
                now,
              )
              const ok = outcome.code === 0 && outcome.stdout.includes(MARKER_DONE)
              return { ready: outcome.readyDelayMs, total: outcome.totalMs, ok }
            }),
          { concurrency: 1 },
        )
        const successful = runs.filter((run) => run.ok)
        const ready = summarize(successful.map((run) => run.ready).filter((value) => value >= 0))
        const total = summarize(successful.map((run) => run.total))
        return { ready, total, failures: runs.length - successful.length }
      })

    const fullRun = (arm: RuntimeName) =>
      Effect.gen(function* () {
        const target = targetMap[arm]
        const dbPath = `${options.workdir}/full-${arm}.db`
        yield* cleanDatabase(dbPath)
        yield* removeIfExists(`${options.workdir}/checks-${arm}`)
        yield* ensureDirectory(`${options.workdir}/checks-${arm}`)
        const outcome = yield* launch(
          specFor(target, "full", configFor(options, arm, dbPath), false),
          { timeoutMs: 300_000 },
          now,
        )
        const measurements = decodeMeasurements(section(outcome.stdout, MARKER_RESULT))
        if (measurements === undefined) {
          blocked.push({
            arm,
            measurement: "arm-measurements",
            reason: `full arm run failed (exit ${outcome.code}): ${
              outcome.stderr.slice(0, 400).trim() || outcome.stdout.slice(-400).trim()
            }`,
          })
          decisionBlocks.push(`${arm}: full arm run produced no measurements`)
        }
        return { measurements, ok: outcome.code === 0 }
      })

    const signalRun = (arm: RuntimeName) =>
      Effect.gen(function* () {
        const target = targetMap[arm]
        const dbPath = `${options.workdir}/signal-${arm}.db`
        yield* cleanDatabase(dbPath)
        const outcome = yield* launch(
          specFor(target, "signal", configFor(options, arm, dbPath), false),
          { signalAfterReady: "SIGTERM", timeoutMs: 60_000 },
          now,
        )
        const handled = outcome.stdout.includes(MARKER_SIGNAL)
        const shutdownMs = outcome.readyDelayMs < 0
          ? 0
          : Math.max(0, outcome.totalMs - outcome.readyDelayMs)
        if (!handled) {
          blocked.push({
            arm,
            measurement: "signals",
            reason: `no ${MARKER_SIGNAL} marker (exit ${outcome.code})`,
          })
        }
        return {
          handlerInvoked: handled,
          shutdownMs: Number(shutdownMs.toFixed(3)),
          exitCode: outcome.code,
        }
      })

    const permissionRun = (arm: RuntimeName) =>
      Effect.gen(function* () {
        const target = targetMap[arm]
        const outcome = yield* launch(
          specFor(
            target,
            "permissions",
            configFor(options, arm, `${options.workdir}/perm-${arm}.db`),
            true,
          ),
          { timeoutMs: 90_000 },
          now,
        )
        const probes = decodeProbes(section(outcome.stdout, MARKER_PERMISSIONS))
        if (probes.length === 0) {
          blocked.push({
            arm,
            measurement: "permissions",
            reason: `no permission marker (exit ${outcome.code}): ${
              outcome.stderr.slice(0, 300).trim()
            }`,
          })
        }
        return probes
      })

    const packaging = (arm: RuntimeName) =>
      Effect.gen(function* () {
        const target = targetMap[arm]
        const runtimeBinaryBytes = yield* fileSize(target.binaryPath)
        if (arm === "deno") {
          const output = `${options.workdir}/kanbei-spike-deno`
          yield* removeIfExists(output)
          yield* capture({
            command: Deno.execPath(),
            args: ["compile", "-A", "--no-check", "--output", output, target.entry],
          })
          const bytes = yield* fileSize(output)
          return {
            runtimeBinaryBytes,
            singleBinaryBytes: bytes,
            singleBinaryStatus: bytes > 0 ? ("measured" as const) : ("blocked" as const),
            singleBinaryNote: bytes > 0
              ? "deno compile of the spike entrypoint"
              : "deno compile produced no binary",
          }
        }
        const closure = yield* directoryBytes(options.nodeModules)
        blocked.push({
          arm: "node",
          measurement: "packaging.single-binary",
          reason:
            "Node 24 ships single-executable applications only as a SEA blob injected with postject; " +
            "that toolchain is not in this sandbox. The runtime binary and the pinned effect closure are measured instead.",
        })
        return {
          runtimeBinaryBytes,
          singleBinaryBytes: 0,
          singleBinaryStatus: "blocked" as const,
          singleBinaryNote:
            `Node single executable requires a SEA blob plus postject injection, which this sandbox does not provide; ` +
            `pinned dependency closure is ${toMilliseconds(closure)} MiB`,
        }
      })

    yield* Effect.forEach(
      ["node", "deno"] as const,
      (arm) =>
        Effect.gen(function* () {
          const cold = yield* coldStart(arm)
          const full = yield* fullRun(arm)
          const signals = yield* signalRun(arm)
          const permissions = yield* permissionRun(arm)
          const packaged = yield* packaging(arm)
          const measurements = full.measurements ?? emptyArm(arm)
          arms[arm] = {
            ...measurements,
            runtime: arm,
            boot: {
              coldReady: cold.ready,
              coldStart: cold.total,
              warmStart: measurements.boot.warmStart,
            },
            signals,
            permissions,
            packaging: packaged,
          }
        }),
      { concurrency: 1 },
    )

    const outcome = decide(arms, PRE_REGISTRATION, decisionBlocks)

    return {
      arms,
      blocked,
      decisionBlocks,
      outcome,
      unboundProbes: [
        "S1 also cites `cost-v1`; the runtime arm binds `append-v1@1.0.0` for its ledger workload and does not consume the model/broker cost workload, which belongs to the LANG-1 arm (A4)",
      ],
      residualRisks: [
        `environment: hostFingerprint=${environment.hostFingerprint} kernel=${environment.kernel} runtime=${environment.runtime}`,
        "sandbox posture is unverified: both arms ran with the operator's normal OS permissions, so the comparison is runtime-only and does not claim containment",
        "warm start is app-layer bring-up inside one process (open database, prepare statements, insert and read), not a second process launch",
        "the ledger workload writes to the repository tmp filesystem; a different filesystem changes absolute durable-append numbers, though both arms shared it",
        "each arm contributed one full workload run; cold start is 11 runs per arm and the distributions are reported rather than a significance test",
        "Node's permission model has no network capability, so network containment for the Node arm remains an OS-sandbox obligation",
      ],
    }
  })
