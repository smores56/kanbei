import { Schema } from "effect"
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
})
export type ValidationStep = typeof ValidationStep.Type

export const ValidationReport = Schema.Struct({
  candidateId: Schema.String,
  candidateDigest: Schema.String,
  steps: Schema.Array(ValidationStep),
  verdict: Schema.Literals(["validated", "rejected", "quarantined"]),
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

const manifestStep = (manifest: CandidateManifest): ValidationStep => {
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
  }
}

const dependencyStep = (
  workspace: CandidateWorkspace,
  inputs: ValidationInputs,
): ValidationStep => {
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
  }
}

const scanStep = (capabilities: ReadonlyArray<CapabilityRequest>): ValidationStep => {
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
  }
}

const booleanStep = (
  step: ValidationStepName,
  ok: boolean,
  pass: string,
  fail: string,
): ValidationStep => ({ step, outcome: ok ? "pass" : "fail", detail: ok ? pass : fail })

/**
 * The eight ordered validations (`07 §4.8.4`). The first failure stops the run;
 * a trust-root, undeclared, or forbidden capability quarantines the candidate.
 */
export const runValidation = (
  manifest: CandidateManifest,
  candidateDigest: string,
  workspace: CandidateWorkspace,
  inputs: ValidationInputs,
): ValidationReport => {
  const steps: Array<ValidationStep> = []
  const push = (step: ValidationStep): boolean => {
    steps.push(step)
    return step.outcome === "pass"
  }
  if (!push(manifestStep(manifest))) {
    return { candidateId: manifest.candidateId, candidateDigest, steps, verdict: "rejected" }
  }
  if (!push(dependencyStep(workspace, inputs))) {
    return { candidateId: manifest.candidateId, candidateDigest, steps, verdict: "rejected" }
  }
  const scan = scanStep(workspace.capabilities)
  const quarantineReason =
    workspace.capabilities.find((entry) => capabilityClass(entry.capability) === "trust_root") ===
        undefined
      ? workspace.capabilities.find((entry) => capabilityClass(entry.capability) !== "declared")
      : workspace.capabilities.find((entry) => capabilityClass(entry.capability) === "trust_root")
  if (!push(scan)) {
    return {
      candidateId: manifest.candidateId,
      candidateDigest,
      steps,
      verdict: "quarantined",
      quarantineReason: quarantineReason?.capability ?? "capability scan failed",
    }
  }
  const rest: ReadonlyArray<ValidationStep> = [
    booleanStep(
      "artifact_provenance",
      inputs.artifactPresent,
      "artifact digest is present",
      "artifact digest is absent from the image",
    ),
    booleanStep(
      "dynamic_conformance",
      inputs.dynamicConformance,
      "candidate boots with capabilities denied",
      "dynamic conformance failed",
    ),
    booleanStep(
      "migration_dry_run",
      inputs.migrationDryRun,
      "forward migrations ran on shadow state",
      "migration dry run failed",
    ),
    booleanStep(
      "security_probes",
      inputs.securityProbes,
      "trust-root and cross-scope probes denied",
      "a security probe did not fail closed",
    ),
    booleanStep(
      "health_plan",
      inputs.healthPlanDeclared,
      "health plan is executable",
      "health plan is missing or external",
    ),
  ]
  for (const step of rest) {
    if (!push(step)) {
      return { candidateId: manifest.candidateId, candidateDigest, steps, verdict: "rejected" }
    }
  }
  return { candidateId: manifest.candidateId, candidateDigest, steps, verdict: "validated" }
}
