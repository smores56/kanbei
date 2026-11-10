import { Cause, Context, Effect, Exit, FileSystem, Option, Path, Schema } from "effect"
import { ProbeUnavailable as ProbeUnavailableError } from "./errors.ts"
import type { HostCommandShape } from "./host.ts"
import type { ArmEnforcement, ProbeArm, ProbeCategory, ProbeResult, ProbeStatus } from "./types.ts"

export interface ProbeEnvShape {
  readonly fs: FileSystem.FileSystem
  readonly path: Path.Path
  readonly command: HostCommandShape
  readonly scratch: string
}

export const ProbeEnv = Context.Service<ProbeEnvShape>("kanbei/spike/ProbeEnv")

export interface ProbeSpec {
  readonly id: string
  readonly category: ProbeCategory
  readonly arm: ProbeArm
  readonly method: string
  readonly enforcement?: ArmEnforcement
  readonly prerequisites?: ReadonlyArray<string>
}

export interface ProbeFindings {
  readonly status: ProbeStatus
  readonly observed: string
  readonly evidence: string
  readonly residual?: string
}

const toResult = (spec: ProbeSpec, findings: ProbeFindings): ProbeResult => ({
  id: spec.id,
  category: spec.category,
  arm: spec.arm,
  method: spec.method,
  status: findings.status,
  observed: findings.observed,
  evidence: findings.evidence,
  ...(spec.enforcement === undefined ? {} : { enforcement: spec.enforcement }),
  ...(spec.prerequisites === undefined ? {} : { prerequisites: [...spec.prerequisites] }),
  ...(findings.residual === undefined ? {} : { residual: findings.residual }),
})

const UnavailableShape = Schema.Struct({
  _tag: Schema.Literal("ProbeUnavailable"),
  reason: Schema.String,
  detail: Schema.optional(Schema.String),
})

const asProbeUnavailable = (value: unknown) =>
  Option.getOrUndefined(Schema.decodeUnknownOption(UnavailableShape)(value))

const unavailableFinding = (cause: Cause.Cause<unknown>): ProbeFindings => {
  const error = Option.getOrUndefined(Cause.findErrorOption(cause))
  const tagged = asProbeUnavailable(error)
  if (tagged !== undefined) {
    return {
      status: "blocked",
      observed: `unavailable: ${tagged.reason}`,
      evidence: tagged.detail ?? "",
    }
  }
  if (Cause.hasDies(cause)) {
    return { status: "blocked", observed: "probe defect; no measurement exists", evidence: "" }
  }
  return { status: "blocked", observed: "probe did not complete", evidence: "" }
}

/**
 * Runs a probe body and always yields a classified result. An unavailable probe
 * becomes `blocked`, never a silent pass; a defect also becomes `blocked` because
 * no measurement exists to justify either verdict.
 */
export const probe = (
  spec: ProbeSpec,
  body: Effect.Effect<ProbeFindings, unknown, ProbeEnvShape>,
): Effect.Effect<ProbeResult, never, ProbeEnvShape> =>
  Effect.exit(body).pipe(
    Effect.map((exit) =>
      Exit.match(exit, {
        onSuccess: (findings) => toResult(spec, findings),
        onFailure: (cause) => toResult(spec, unavailableFinding(cause)),
      })
    ),
  )

/** Signals that a probe needs a capability this host does not expose. */
export const unavailable = (reason: string, detail?: string) =>
  Effect.fail(new ProbeUnavailableError(detail === undefined ? { reason } : { reason, detail }))

export const findings = (
  status: ProbeStatus,
  observed: string,
  evidence: string,
  residual?: string,
): ProbeFindings => ({
  status,
  observed,
  evidence,
  ...(residual === undefined ? {} : { residual }),
})
