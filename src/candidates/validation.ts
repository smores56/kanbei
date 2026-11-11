import { Effect, Schema } from "effect"
import { canonicalize } from "../evidence/canonical.ts"
import { sha256Labeled } from "../evidence/digest.ts"
import type { CandidateManifest, CandidateWorkspace, CapabilityRequest } from "./manifest.ts"
import { CANDIDATE_CONTRACT_VERSION, capabilityClass } from "./manifest.ts"

export const ValidationStepName = Schema.Literals([
  "manifest_schema",
  "dependency_resolution",
  "static_capability_scan",
  "artifact_provenance",
  "dynamic_conformance",
  "migration_dry_run",
  "security_probes",
  "health_plan",
])
export type ValidationStepName = typeof ValidationStepName.Type

export const ValidationStep = Schema.Struct({
  step: ValidationStepName,
  outcome: Schema.Literals(["pass", "fail"]),
  detail: Schema.String,
  /** Digest of the exact evidence the step ruled on; a report cannot be replayed. */
  evidenceDigest: Schema.String,
})
export type ValidationStep = typeof ValidationStep.Type

export const ValidationReport = Schema.Struct({
  candidateId: Schema.String,
  candidateDigest: Schema.String,
  steps: Schema.Array(ValidationStep),
  verdict: Schema.Literals(["validated", "rejected", "quarantined"]),
  /** Digest over every input the run consumed, so evidence travels with the verdict. */
  inputsDigest: Schema.String,
  authorizedSources: Schema.Array(Schema.String),
  quarantineReason: Schema.optional(Schema.String),
})
export type ValidationReport = typeof ValidationReport.Type

/** Evidence from the sandboxed conformance, migration, and probe runs. */
export interface ValidationInputs {
  readonly artifactPresent: boolean
  /** `id@version` entries present in the image artifact set or candidate store. */
  readonly resolvedArtifacts: ReadonlyArray<string>
  readonly dynamicConformance: boolean
  readonly migrationDryRun: boolean
  readonly securityProbes: boolean
  readonly healthPlanDeclared: boolean
  readonly migrations?: ReadonlyArray<{
    readonly store: string
    readonly from: number
    readonly to: number
    readonly mode: "compatible" | "forward-only" | "requires-checkpoint"
  }>
  /** Source prefixes the declarative view may cite; an empty list authorizes none. */
  readonly authorizedSources?: ReadonlyArray<string>
}

export const passingInputs = (
  overrides: Partial<ValidationInputs> = {},
): ValidationInputs => ({
  artifactPresent: true,
  resolvedArtifacts: [],
  dynamicConformance: true,
  migrationDryRun: true,
  securityProbes: true,
  healthPlanDeclared: true,
  ...overrides,
})

const digestPattern = /^sha256:[0-9a-f]{64}$/u

const toJson = (value: unknown): Schema.Json => Schema.decodeUnknownSync(Schema.Json)(value)

const evidenceDigestOf = (
  step: ValidationStepName,
  evidence: Schema.Json,
): Effect.Effect<string> =>
  sha256Labeled("kanbei.validation.evidence.v1", [canonicalize(toJson({ step, evidence }))]).pipe(
    Effect.orDie,
  )

/** The immutable evidence a step ruled on; the digest is computed from exactly this. */
interface StepDraft {
  readonly step: ValidationStepName
  readonly outcome: "pass" | "fail"
  readonly detail: string
  readonly evidence: Schema.Json
}

const manifestStep = (manifest: CandidateManifest): StepDraft => {
  const problems: Array<string> = []
  if (manifest.candidateId.trim() === "") problems.push("candidateId is required")
  if (manifest.scope.trim() === "") problems.push("scope is required")
  if (!Number.isInteger(manifest.generation) || manifest.generation < 1) {
    problems.push("generation must be a positive integer")
  }
  if (manifest.schema.contractVersion !== CANDIDATE_CONTRACT_VERSION) {
    problems.push(
      `contract ${manifest.schema.contractVersion} is not ${CANDIDATE_CONTRACT_VERSION}`,
    )
  }
  if (!Number.isInteger(manifest.schema.dataVersion) || manifest.schema.dataVersion < 0) {
    problems.push("dataVersion must be a non-negative integer")
  }
  for (
    const [name, digest] of Object.entries({
      artifact: manifest.artifact.digest,
      artifactManifest: manifest.artifact.manifestDigest,
      manifestDigest: manifest.manifestDigest,
      dependencyLockDigest: manifest.dependencyLockDigest,
      capabilityRequestsDigest: manifest.capabilityRequestsDigest,
    })
  ) {
    if (!digestPattern.test(digest)) problems.push(`${name} is not a content digest`)
  }
  if (manifest.artifact.size <= 0) problems.push("artifact is empty")
  return {
    step: "manifest_schema",
    outcome: problems.length === 0 ? "pass" : "fail",
    detail: problems.length === 0 ? "manifest parses" : problems.join("; "),
    evidence: toJson(manifest),
  }
}

const dependencyStep = (
  workspace: CandidateWorkspace,
  inputs: ValidationInputs,
): StepDraft => {
  const resolved = new Set(inputs.resolvedArtifacts)
  const missing = workspace.dependencies
    .map((dependency) => `${dependency.id}@${dependency.version}`)
    .filter((entry) => !resolved.has(entry))
  return {
    step: "dependency_resolution",
    outcome: missing.length === 0 ? "pass" : "fail",
    detail: missing.length === 0
      ? `${workspace.dependencies.length} dependencies resolve`
      : `missing ${missing.join(", ")}`,
    evidence: toJson({
      dependencies: workspace.dependencies,
      resolvedArtifacts: inputs.resolvedArtifacts.toSorted(),
    }),
  }
}

const scanStep = (capabilities: ReadonlyArray<CapabilityRequest>): StepDraft => {
  const violations = capabilities.flatMap((entry) => {
    const kind = capabilityClass(entry.capability)
    return kind === "declared" ? [] : [`${entry.capability} is ${kind}`]
  })
  return {
    step: "static_capability_scan",
    outcome: violations.length === 0 ? "pass" : "fail",
    detail: violations.length === 0
      ? `${capabilities.length} capabilities declared`
      : violations.join(
        "; ",
      ),
    evidence: toJson(capabilities),
  }
}

const booleanStep = (
  step: ValidationStepName,
  evidence: Schema.Json,
  ok: boolean,
  pass: string,
  fail: string,
): StepDraft => ({ step, outcome: ok ? "pass" : "fail", detail: ok ? pass : fail, evidence })

const payloadDigest = (inputs: ValidationInputs): Effect.Effect<string> =>
  sha256Labeled("kanbei.validation.inputs.v1", [canonicalize(toJson(inputs))]).pipe(Effect.orDie)

/**
 * The eight ordered validations (`07 §4.8.4`). The first failure stops the run;
 * a trust-root, undeclared, or forbidden capability quarantines the candidate.
 * Every step commits to the digest of the evidence it ruled on.
 */
export const runValidation = (
  manifest: CandidateManifest,
  candidateDigest: string,
  workspace: CandidateWorkspace,
  inputs: ValidationInputs,
): Effect.Effect<ValidationReport> =>
  Effect.gen(function* () {
    const inputsDigest = yield* payloadDigest(inputs)
    const steps: Array<ValidationStep> = []
    const push = (draft: StepDraft): Effect.Effect<boolean> =>
      evidenceDigestOf(draft.step, draft.evidence).pipe(
        Effect.map((evidenceDigest) => {
          steps.push({
            step: draft.step,
            outcome: draft.outcome,
            detail: draft.detail,
            evidenceDigest,
          })
          return draft.outcome === "pass"
        }),
      )
    const finish = (verdict: ValidationReport["verdict"], reason?: string): ValidationReport => ({
      candidateId: manifest.candidateId,
      candidateDigest,
      steps,
      verdict,
      inputsDigest,
      authorizedSources: [...(inputs.authorizedSources ?? [])],
      ...(reason === undefined ? {} : { quarantineReason: reason }),
    })

    if (!(yield* push(manifestStep(manifest)))) return finish("rejected")
    if (!(yield* push(dependencyStep(workspace, inputs)))) return finish("rejected")
    const scan = scanStep(workspace.capabilities)
    const quarantineReason = workspace.capabilities.find((entry) =>
        capabilityClass(entry.capability) === "trust_root"
      ) ===
        undefined
      ? workspace.capabilities.find((entry) => capabilityClass(entry.capability) !== "declared")
      : workspace.capabilities.find((entry) => capabilityClass(entry.capability) === "trust_root")
    if (!(yield* push(scan))) {
      return finish("quarantined", quarantineReason?.capability ?? "capability scan failed")
    }
    const rest: ReadonlyArray<StepDraft> = [
      booleanStep(
        "artifact_provenance",
        toJson({ artifactPresent: inputs.artifactPresent, digest: manifest.artifact.digest }),
        inputs.artifactPresent,
        "artifact digest is present",
        "artifact digest is absent from the image",
      ),
      booleanStep(
        "dynamic_conformance",
        toJson({
          dynamicConformance: inputs.dynamicConformance,
          candidateDigest,
        }),
        inputs.dynamicConformance,
        "candidate boots with capabilities denied",
        "dynamic conformance failed",
      ),
      booleanStep(
        "migration_dry_run",
        toJson({
          migrationDryRun: inputs.migrationDryRun,
          migrations: inputs.migrations ?? [],
        }),
        inputs.migrationDryRun,
        "forward migrations ran on shadow state",
        "migration dry run failed",
      ),
      booleanStep(
        "security_probes",
        toJson({ securityProbes: inputs.securityProbes, capabilities: workspace.capabilities }),
        inputs.securityProbes,
        "trust-root and cross-scope probes denied",
        "a security probe did not fail closed",
      ),
      booleanStep(
        "health_plan",
        toJson({ healthPlanDeclared: inputs.healthPlanDeclared }),
        inputs.healthPlanDeclared,
        "health plan is executable",
        "health plan is missing or external",
      ),
    ]
    for (const step of rest) {
      if (!(yield* push(step))) return finish("rejected")
    }
    return finish("validated")
  })
