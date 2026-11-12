import { Effect, Schema } from "effect"
import { Outcome } from "../../evidence/types.ts"
import { ResultError } from "./errors.ts"
import type { ModelMatrix } from "./model-arm.ts"
import { Lang1PreRegistration, lang1PreRegistration } from "./pre-registration.ts"
import type { TaskSpec } from "./types.ts"
import {
  DecisionRecord,
  FailureClass,
  HostCallMeasurement,
  MissingInput,
  ModelCell,
  ModelSlot,
  ProgressiveDisclosureRow,
  RuntimeMeasurement,
  SecurityRow,
  SnapshotRow,
  TaskSpec as TaskSpecSchema,
  TEMPERATURE,
  TRIALS_PER_CELL,
} from "./types.ts"

export const BakeoffEnvironment = Schema.Struct({
  hostFingerprint: Schema.String,
  kernel: Schema.String,
  runtime: Schema.String,
  sandboxPosture: Schema.String,
  brokerIdentity: Schema.String,
  hostname: Schema.String,
  cpus: Schema.Number,
})
export type BakeoffEnvironment = typeof BakeoffEnvironment.Type

export const LocalMeasurements = Schema.Struct({
  runtimes: Schema.Array(RuntimeMeasurement),
  hostCalls: Schema.Array(HostCallMeasurement),
  progressiveDisclosure: Schema.Array(ProgressiveDisclosureRow),
  security: Schema.Array(SecurityRow),
  snapshot: Schema.Array(SnapshotRow),
})
export type LocalMeasurements = typeof LocalMeasurements.Type

export const ModelMatrixReport = Schema.Struct({
  status: Schema.Literals(["blocked", "ready"]),
  plannedCells: Schema.Number,
  plannedTrials: Schema.Number,
  trialsPerCell: Schema.Number,
  temperature: Schema.Number,
  slots: Schema.Array(ModelSlot),
  missingInputs: Schema.Array(MissingInput),
  cells: Schema.Array(ModelCell),
  modelResults: Schema.Literal("none"),
})
export type ModelMatrixReport = typeof ModelMatrixReport.Type

export const EvidenceStatus = Schema.Struct({
  testId: Schema.Literal("A4-LANG-1"),
  testClass: Schema.Literal("spike"),
  outcome: Outcome,
  fixtureVersions: Schema.Array(Schema.String),
  envelope: Schema.Literal("not-issued"),
  reasons: Schema.Array(Schema.String),
})
export type EvidenceStatus = typeof EvidenceStatus.Type

export const BakeoffResult = Schema.Struct({
  schemaVersion: Schema.Literal(1),
  protocolId: Schema.Literal("S1"),
  decisionIds: Schema.Array(Schema.Literal("LANG-1")),
  generatedBy: Schema.String,
  environment: BakeoffEnvironment,
  preRegistration: Lang1PreRegistration,
  taskSuite: Schema.Struct({
    suiteId: Schema.String,
    suiteVersion: Schema.String,
    digest: Schema.String,
    tasks: Schema.Array(TaskSpecSchema),
  }),
  localMeasurements: LocalMeasurements,
  modelMatrix: ModelMatrixReport,
  failureTaxonomy: Schema.Array(FailureClass),
  evidence: EvidenceStatus,
  decisionRecord: DecisionRecord,
  residuals: Schema.Array(Schema.String),
})
export type BakeoffResult = typeof BakeoffResult.Type

export interface TaskSuiteFacts {
  readonly suiteId: string
  readonly suiteVersion: string
  readonly digest: string
  readonly tasks: ReadonlyArray<TaskSpec>
}

export interface ResultInputs {
  readonly environment: BakeoffEnvironment
  readonly taskSuite: TaskSuiteFacts
  readonly local: LocalMeasurements
  readonly modelMatrix: ModelMatrix
  readonly failureTaxonomy: ReadonlyArray<FailureClass>
  readonly generatedBy: string
}

const decisionRecord = (
  local: LocalMeasurements,
  model: ModelMatrix,
): DecisionRecord => {
  const measured = local.runtimes.filter((runtime) => runtime.resolution.status === "available")
  const unavailable = local.runtimes.filter((runtime) =>
    runtime.resolution.status === "unavailable"
  )
  const missingIds = model.missingInputs.map((input) => input.id).join(", ")
  return {
    gate: "DG-1",
    status: "open",
    recommendation: "none",
    rationale: [
      `${model.plannedCells} model cells (${model.plannedTrials} planned trials) are blocked; 0 trials were run.`,
      `Blocked inputs: ${missingIds.length === 0 ? "none" : missingIds}.`,
      "A language selection requires complete paired model trials; local runtime measurements do not substitute.",
    ],
    supportedFindings: [
      `Local runtime resolution succeeded for ${measured.length} of ${local.runtimes.length} variants; ${unavailable.length} are unavailable with exact missing inputs.`,
      "The ten-task fixture validates against its schema and carries gold outputs for the deterministic tasks.",
      "Host-call facade latency, snapshot support, and capability boundaries were measured locally for the resolved variants.",
      "Progressive-disclosure character counts were measured at 10, 50, 200, and 1000 tools; provider token counts remain blocked.",
    ],
    unsupportedFindings: [
      "Any LANG-1 arm selection or rejection on model success, tokens, turns, or tool calls.",
      "Any cost or latency comparison that depends on model behaviour.",
      "Any security-gate pass: no frozen OS-sandbox profile is available on this host.",
      "Any snapshot trust claim beyond the local Monty dump/load_snapshot probe.",
    ],
  }
}

const evidenceStatus = (model: ModelMatrix): EvidenceStatus => ({
  testId: "A4-LANG-1",
  testClass: "spike",
  outcome: "blocked",
  fixtureVersions: ["cost-v1@1.0.0", "security-v1@1.0.0", "lang-v1@1.0.0"],
  envelope: "not-issued",
  reasons: [
    "no EvidenceEnvelope is issued: S1 has no completed model trials",
    `${model.plannedCells} model cells are ${model.status}; no model recordings or artifacts exist`,
    "sandboxPosture is unverified; S1 security cells cannot be decided",
    ...model.missingInputs.map((input) => `${input.id}: ${input.exact}`),
  ],
})

export const buildResult = (
  inputs: ResultInputs,
): Effect.Effect<BakeoffResult, ResultError> => {
  const report: ModelMatrixReport = {
    status: inputs.modelMatrix.status,
    plannedCells: inputs.modelMatrix.plannedCells,
    plannedTrials: inputs.modelMatrix.plannedTrials,
    trialsPerCell: TRIALS_PER_CELL,
    temperature: TEMPERATURE,
    slots: [...inputs.modelMatrix.slots],
    missingInputs: [...inputs.modelMatrix.missingInputs],
    cells: [...inputs.modelMatrix.cells],
    modelResults: "none",
  }

  const candidate: BakeoffResult = {
    schemaVersion: 1,
    protocolId: "S1",
    decisionIds: ["LANG-1"],
    generatedBy: inputs.generatedBy,
    environment: inputs.environment,
    preRegistration: lang1PreRegistration,
    taskSuite: {
      suiteId: inputs.taskSuite.suiteId,
      suiteVersion: inputs.taskSuite.suiteVersion,
      digest: inputs.taskSuite.digest,
      tasks: [...inputs.taskSuite.tasks],
    },
    localMeasurements: inputs.local,
    modelMatrix: report,
    failureTaxonomy: [...inputs.failureTaxonomy],
    evidence: evidenceStatus(inputs.modelMatrix),
    decisionRecord: decisionRecord(inputs.local, inputs.modelMatrix),
    residuals: [
      "Host-call latency is simulated in-process or by CPU spin in Lua/Luau, not by the real broker IPC path.",
      "Peak RSS is sampled from /proc/<pid>/status VmHWM at 5 ms intervals; very short processes may be undercounted.",
      "No frozen OS-sandbox profile exists on this host, so capability rows describe the local posture, not sandbox enforcement.",
      "Monty startup includes CPython process startup and pydantic-monty import; it is not an in-process sandbox-only number.",
      "Progressive-disclosure rows report characters, not provider tokens; token measurement is blocked.",
      "Model matrix cells remain blocked; this result contains no model-generated success, token, turn, or latency data.",
      "Deno's node-compatibility shim on PATH is rejected by a version pattern; the Node arm uses nixpkgs Node, not the shim.",
      "Local host-call probes use 50, 20, and 3 calls at 10, 100, and 1000 ms to bound wall time; the pre-registered model workload keeps the full task call counts.",
    ],
  }

  return Effect.try({
    try: () => Schema.decodeUnknownSync(BakeoffResult)(candidate),
    catch: (error) =>
      new ResultError({
        reason: error instanceof Error ? error.message : String(error),
      }),
  })
}

export { lang1PreRegistration }
