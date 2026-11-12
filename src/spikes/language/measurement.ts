import { Effect, FileSystem, Path } from "effect"
import type { ArmDefinition, PreparedProgram, ProbeOptions } from "./arms/contract.ts"
import { parseProbeOutput } from "./arms/contract.ts"
import { ArmError } from "./errors.ts"
import { fanoutGold, PROBE_CALLS } from "./host-calls.ts"
import { CommandRunner, percentile } from "./process.ts"
import type {
  HostCallMeasurement,
  LatencyProfileMs,
  ProbeOutput,
  RuntimeMeasurement,
  SecurityRow,
  SnapshotRow,
  StartupMeasurement,
} from "./types.ts"
import { LATENCY_PROFILES_MS } from "./types.ts"

export interface ArmMeasurement {
  readonly measurement: RuntimeMeasurement
  readonly hostCalls: ReadonlyArray<HostCallMeasurement>
  readonly security: ReadonlyArray<SecurityRow>
  readonly snapshot: SnapshotRow
}

interface ProbeRun {
  readonly latencyMs: LatencyProfileMs
  readonly wallMs: number
  readonly peakRssBytes: number
  readonly rssSampled: boolean
  readonly error: string | undefined
  readonly output: ProbeOutput | undefined
  readonly simulation: HostCallMeasurement["latencySimulation"]
}

const STARTUP_RUNS = 8

const writePrepared = (
  arm: string,
  fs: FileSystem.FileSystem,
  path: Path.Path,
  directory: string,
  prepared: PreparedProgram,
): Effect.Effect<void, ArmError> =>
  Effect.gen(function* () {
    yield* fs.makeDirectory(directory, { recursive: true }).pipe(
      Effect.mapError((error) =>
        new ArmError({ arm, reason: `cannot create directory: ${String(error)}` })
      ),
    )
    yield* Effect.forEach(prepared.files, (file) =>
      fs.writeFileString(path.join(directory, file.name), file.text).pipe(
        Effect.mapError((error) =>
          new ArmError({ arm, reason: `cannot write probe file: ${String(error)}` })
        ),
      ))
  })

const startupMeasurement = (
  definition: ArmDefinition,
  runtime: Extract<RuntimeMeasurement["resolution"], { status: "available" }>,
  directory: string,
): Effect.Effect<
  StartupMeasurement | undefined,
  ArmError,
  CommandRunner | FileSystem.FileSystem | Path.Path
> =>
  Effect.gen(function* () {
    const runner = yield* CommandRunner
    const fs = yield* FileSystem.FileSystem
    const path = yield* Path.Path
    const prepared = definition.startup(runtime)
    yield* writePrepared(definition.variant, fs, path, directory, prepared)
    const samples = yield* Effect.forEach(
      Array.from({ length: STARTUP_RUNS }, (_unused, index) => index),
      () =>
        runner
          .run({
            argv: prepared.argv(directory),
            env: prepared.env(directory),
            timeoutMs: prepared.timeoutMs,
          })
          .pipe(
            Effect.map((
              outcome,
            ) => (outcome.exitCode === 0 && !outcome.timedOut ? outcome.wallMs : -1)),
            Effect.catch(() => Effect.succeed(-1)),
          ),
      { concurrency: 1 },
    )
    const valid = samples.filter((value) => value >= 0)
    if (valid.length === 0) return undefined
    return {
      runs: valid.length,
      p50Ms: percentile(valid, 0.5),
      p95Ms: percentile(valid, 0.95),
      minMs: Math.min(...valid),
      maxMs: Math.max(...valid),
    }
  })

const fanoutMatches = (output: ProbeOutput, calls: number): boolean => {
  const expected = fanoutGold(calls)
  return (
    output.fanout.checksum === expected.checksum &&
    JSON.stringify(output.fanout.groups) === JSON.stringify(expected.groups) &&
    JSON.stringify(output.fanout.top) === JSON.stringify(expected.top)
  )
}

const securityNote = (variant: RuntimeMeasurement["variant"], allowed: boolean): string => {
  if (allowed) {
    return variant === "monty"
      ? "allowed by host-provided OS access in this probe"
      : "capability available in this local posture"
  }
  return variant === "deno"
    ? "denied by the Deno permission boundary"
    : "denied by the runtime boundary"
}

const securityRow = (
  variant: RuntimeMeasurement["variant"],
  probe: SecurityRow["probe"],
  value: { readonly allowed: boolean } | undefined,
): SecurityRow =>
  value === undefined
    ? { variant, probe, allowed: null, note: "not measured: probe did not run" }
    : { variant, probe, allowed: value.allowed, note: securityNote(variant, value.allowed) }

const snapshotRow = (variant: RuntimeMeasurement["variant"], output: ProbeOutput): SnapshotRow => ({
  variant,
  snapshotSupported: output.snapshot.supported,
  snapshotOk: output.snapshot.ok,
  approvalPauseSupported: output.approvalPause.supported,
  approvalPauseOk: output.approvalPause.ok,
  trust: variant === "monty"
    ? "host-authored snapshot bytes; model output cannot author or tamper with serialized state"
    : "unsupported; the runtime exposes no execution-state snapshot API",
  ...(output.snapshot.detail === undefined ? {} : { detail: output.snapshot.detail }),
})

const unavailableMeasurement = (
  definition: ArmDefinition,
  measurement: RuntimeMeasurement,
): ArmMeasurement => ({
  measurement,
  hostCalls: [],
  security: [
    securityRow(definition.variant, "file-write", undefined),
    securityRow(definition.variant, "env-read", undefined),
    securityRow(definition.variant, "process-spawn", undefined),
    securityRow(definition.variant, "network-socket", undefined),
    securityRow(definition.variant, "secret-file-read", undefined),
  ],
  snapshot: {
    variant: definition.variant,
    snapshotSupported: false,
    snapshotOk: false,
    approvalPauseSupported: false,
    approvalPauseOk: false,
    trust: "not measured: runtime unavailable",
  },
})

const probeRun = (
  definition: ArmDefinition,
  runtime: Extract<RuntimeMeasurement["resolution"], { status: "available" }>,
  directory: string,
  options: ProbeOptions,
): Effect.Effect<ProbeRun, ArmError, CommandRunner | FileSystem.FileSystem | Path.Path> =>
  Effect.gen(function* () {
    const runner = yield* CommandRunner
    const fs = yield* FileSystem.FileSystem
    const path = yield* Path.Path
    const prepared = definition.prepare(runtime, options)
    yield* writePrepared(definition.variant, fs, path, directory, prepared)
    const outcome = yield* runner
      .run({
        argv: prepared.argv(directory),
        env: prepared.env(directory),
        timeoutMs: prepared.timeoutMs,
        sampleRss: true,
      })
      .pipe(Effect.catch(() => Effect.succeed(undefined)))

    const base = {
      latencyMs: options.latencyMs as LatencyProfileMs,
      wallMs: outcome?.wallMs ?? 0,
      peakRssBytes: outcome?.peakRssBytes ?? 0,
      rssSampled: outcome?.rssSampled ?? false,
      simulation: prepared.latencySimulation,
    }

    if (outcome === undefined) {
      return { ...base, error: "command could not be started", output: undefined }
    }
    if (outcome.timedOut) {
      return {
        ...base,
        error: `probe timed out after ${prepared.timeoutMs} ms`,
        output: undefined,
      }
    }
    const parsed = yield* parseProbeOutput(definition.variant, outcome.stdout).pipe(
      Effect.map((output) => ({ output, error: undefined })),
      Effect.catch((error) => Effect.succeed({ output: undefined, error: error.reason })),
    )
    if (parsed.output === undefined) {
      return {
        ...base,
        error: `${parsed.error ?? "unknown probe failure"}; exit ${outcome.exitCode}; stderr ${
          outcome.stderr.trim().slice(0, 200)
        }`,
        output: undefined,
      }
    }
    const validated: ProbeOutput = {
      ...parsed.output,
      fanout: { ...parsed.output.fanout, ok: fanoutMatches(parsed.output, options.calls) },
    }
    return { ...base, error: undefined, output: validated }
  })

const failedMeasurement = (definition: ArmDefinition, reason: string): ArmMeasurement =>
  unavailableMeasurement(definition, {
    arm: definition.arm,
    variant: definition.variant,
    resolution: {
      status: "unavailable",
      variant: definition.variant,
      reason,
      missingInput: "repair local harness filesystem access and rerun",
      log: [],
    },
    unavailableReason: reason,
  })

const measureArmUnsafe = (
  definition: ArmDefinition,
  baseDirectory: string,
  canary: string,
  canaryFile: string,
): Effect.Effect<ArmMeasurement, ArmError, CommandRunner | FileSystem.FileSystem | Path.Path> =>
  Effect.gen(function* () {
    const fs = yield* FileSystem.FileSystem
    const path = yield* Path.Path

    const resolution = yield* definition.resolve()
    if (resolution.status === "unavailable") {
      return unavailableMeasurement(definition, {
        arm: definition.arm,
        variant: definition.variant,
        resolution,
        unavailableReason: resolution.reason,
      })
    }

    const directory = path.join(baseDirectory, definition.variant)
    const startup = yield* startupMeasurement(
      definition,
      resolution,
      path.join(directory, "startup"),
    )
    const scratchDir = path.join(directory, "scratch")
    yield* fs.makeDirectory(scratchDir, { recursive: true }).pipe(
      Effect.mapError((error) =>
        new ArmError({
          arm: definition.variant,
          reason: `cannot create scratch directory: ${String(error)}`,
        })
      ),
    )

    const runs = yield* Effect.forEach(
      LATENCY_PROFILES_MS,
      (
        latencyMs,
      ): Effect.Effect<ProbeRun, ArmError, CommandRunner | FileSystem.FileSystem | Path.Path> =>
        probeRun(definition, resolution, directory, {
          latencyMs,
          calls: PROBE_CALLS[String(latencyMs)] ?? 10,
          scratchDir,
          canary,
          canaryFile,
        }),
      { concurrency: 1 },
    )

    const successful = runs.filter((run) => run.output !== undefined)
    const primary = successful[0]
    const probeFailure = runs.find((run) => run.error !== undefined)?.error

    if (primary === undefined || primary.output === undefined) {
      return unavailableMeasurement(definition, {
        arm: definition.arm,
        variant: definition.variant,
        resolution,
        ...(startup === undefined ? {} : { startup }),
        unavailableReason: probeFailure ?? "no probe run completed",
      })
    }

    const output = primary.output
    const hostCalls: ReadonlyArray<HostCallMeasurement> = successful.flatMap((run) => {
      if (run.output === undefined) return []
      return [{
        variant: definition.variant,
        latencyMs: run.latencyMs,
        calls: run.output.fanout.calls,
        wallMs: run.wallMs,
        p50OverheadMs: percentile(run.output.fanout.callOverheadsMs, 0.5),
        p95OverheadMs: percentile(run.output.fanout.callOverheadsMs, 0.95),
        fanoutOk: run.output.fanout.ok,
        latencySimulation: run.simulation,
      }]
    })

    return {
      measurement: {
        arm: definition.arm,
        variant: definition.variant,
        resolution,
        ...(startup === undefined ? {} : { startup }),
        probeWallMs: primary.wallMs,
        peakRssBytes: primary.peakRssBytes,
        rssSampled: primary.rssSampled,
        probe: output,
        ...(probeFailure === undefined ? {} : { probeFailure }),
      },
      hostCalls,
      security: [
        securityRow(definition.variant, "file-write", output.fileWrite),
        securityRow(definition.variant, "env-read", output.envRead),
        securityRow(definition.variant, "process-spawn", output.processSpawn),
        securityRow(definition.variant, "network-socket", output.network),
        securityRow(definition.variant, "secret-file-read", output.secretFileRead),
      ],
      snapshot: snapshotRow(definition.variant, output),
    }
  })

export const measureArm = (
  definition: ArmDefinition,
  baseDirectory: string,
  canary: string,
  canaryFile: string,
): Effect.Effect<ArmMeasurement, never, CommandRunner | FileSystem.FileSystem | Path.Path> =>
  measureArmUnsafe(definition, baseDirectory, canary, canaryFile).pipe(
    Effect.catch((error) => Effect.succeed(failedMeasurement(definition, error.reason))),
  )
