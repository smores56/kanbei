import { Effect, Schema } from "effect"
import { ProbeOutputError } from "../errors.ts"
import type { CommandRunner } from "../process.ts"
import type { ArmId, ProbeOutput, RuntimeResolution, RuntimeVariantId } from "../types.ts"
import { ProbeOutput as ProbeOutputSchema } from "../types.ts"

export type AvailableRuntime = Extract<RuntimeResolution, { status: "available" }>

export interface ProbeOptions {
  readonly latencyMs: number
  readonly calls: number
  readonly scratchDir: string
  readonly canary: string
  readonly canaryFile: string
}

export interface ProgramFile {
  readonly name: string
  readonly text: string
}

export interface PreparedProgram {
  readonly files: ReadonlyArray<ProgramFile>
  readonly argv: (directory: string) => ReadonlyArray<string>
  readonly env: (directory: string) => Readonly<Record<string, string>>
  readonly latencySimulation: "sleep" | "busy-wait" | "external-sleep" | "host-sleep"
  readonly timeoutMs: number
}

export interface ArmDefinition {
  readonly arm: ArmId
  readonly variant: RuntimeVariantId
  readonly resolve: () => Effect.Effect<RuntimeResolution, never, CommandRunner>
  readonly prepare: (runtime: AvailableRuntime, options: ProbeOptions) => PreparedProgram
  readonly startup: (runtime: AvailableRuntime) => PreparedProgram
}

export const PROBE_MARKER = "KANBEI_PROBE_JSON:"

export const runTimeoutMs = (options: ProbeOptions): number =>
  Math.max(30_000, options.calls * options.latencyMs * 4 + 30_000)

export const parseProbeOutput = (
  variant: RuntimeVariantId,
  stdout: string,
): Effect.Effect<ProbeOutput, ProbeOutputError> => {
  const lines = stdout.split("\n").filter((line) => line.startsWith(PROBE_MARKER))
  const marker = lines[lines.length - 1]
  if (marker === undefined) {
    return Effect.fail(
      new ProbeOutputError({ variant, reason: "probe JSON marker is missing from stdout" }),
    )
  }
  const payload = marker.slice(PROBE_MARKER.length)
  return Effect.try({
    try: () => Schema.decodeUnknownSync(ProbeOutputSchema)(JSON.parse(payload)),
    catch: (error) =>
      new ProbeOutputError({
        variant,
        reason: error instanceof Error ? error.message : String(error),
      }),
  })
}

export const commonEnv = (options: ProbeOptions): Readonly<Record<string, string>> => ({
  KANBEI_LATENCY_MS: String(options.latencyMs),
  KANBEI_CALLS: String(options.calls),
  KANBEI_SCRATCH: options.scratchDir,
  KANBEI_SCRATCH_FILE: `${options.scratchDir}/derived.json`,
  KANBEI_BAKEOFF_CANARY: options.canary,
  KANBEI_CANARY_FILE: options.canaryFile,
  KANBEI_HOST_CALL_BUDGET: "200",
})
