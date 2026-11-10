import { Schema } from "effect"
import { LATENCY_PROFILES_MS, TEMPERATURE, TRIALS_PER_CELL } from "./types.ts"

const FrozenInput = Schema.Struct({
  id: Schema.String,
  description: Schema.String,
  value: Schema.NullOr(Schema.String),
  requiredBeforeTrials: Schema.Boolean,
})

const ArmRegistration = Schema.Struct({
  arm: Schema.Literals(["monty", "typescript", "lua", "bash", "cpython"]),
  candidates: Schema.Array(Schema.String),
  evaluatorVariantRule: Schema.String,
  role: Schema.String,
})

const ModelSlot = Schema.Struct({
  provider: Schema.String,
  tier: Schema.Literals(["primary", "smaller"]),
  modelId: Schema.NullOr(Schema.String),
  credentialEnv: Schema.String,
})

const Metric = Schema.Struct({
  id: Schema.String,
  unit: Schema.String,
  direction: Schema.Literals(["lower-is-better", "higher-is-better", "reported-only"]),
  source: Schema.Literals(["host", "provider", "runtime", "harness"]),
})

const Hypothesis = Schema.Struct({
  id: Schema.String,
  statement: Schema.String,
  nonInferiorityMargin: Schema.String,
  analysis: Schema.String,
})

const HardFail = Schema.Struct({
  id: Schema.String,
  condition: Schema.String,
  consequence: Schema.String,
})

const DecisionRules = Schema.Struct({
  selection: Schema.String,
  inconclusive: Schema.String,
  blocked: Schema.String,
  tieBreak: Schema.String,
  partialEvidence: Schema.String,
})

const AnalysisPlan = Schema.Struct({
  design: Schema.Literal("paired-interleaved"),
  trialsPerCell: Schema.Number,
  temperature: Schema.Number,
  seeds: Schema.Struct({
    base: Schema.Number,
    derivation: Schema.String,
  }),
  confidenceLevel: Schema.Number,
  bootstrapResamples: Schema.Number,
  multiplicity: Schema.String,
  missingData: Schema.String,
})

export const Lang1PreRegistration = Schema.Struct({
  protocolId: Schema.Literal("S1"),
  decisionIds: Schema.Array(Schema.Literal("LANG-1")),
  datasets: Schema.Array(Schema.String),
  status: Schema.Literal("pre-registered"),
  frozenInputs: Schema.Array(FrozenInput),
  arms: Schema.Array(ArmRegistration),
  modelSlots: Schema.Array(ModelSlot),
  providersRequired: Schema.Number,
  modelsPerProvider: Schema.Number,
  smallerModelRequired: Schema.Literal(true),
  tasks: Schema.Struct({
    suiteId: Schema.String,
    suiteVersion: Schema.String,
    count: Schema.Number,
  }),
  hostCallLatenciesMs: Schema.Array(Schema.Number),
  trialsPerCell: Schema.Number,
  temperature: Schema.Number,
  metrics: Schema.Array(Metric),
  hypotheses: Schema.Array(Hypothesis),
  hardFails: Schema.Array(HardFail),
  decisionRules: DecisionRules,
  analysis: AnalysisPlan,
  failureTaxonomyIds: Schema.Array(Schema.String),
  securityProtocol: Schema.String,
  operabilityProtocol: Schema.String,
  progressiveDisclosureProtocol: Schema.String,
  reportingRule: Schema.String,
})
export type Lang1PreRegistration = typeof Lang1PreRegistration.Type

const frozenInputs = [
  {
    id: "FI-01",
    description: "Runtime and interpreter versions",
    value: null,
    requiredBeforeTrials: true,
  },
  {
    id: "FI-02",
    description: "Container image digests and OS-sandbox profile",
    value: null,
    requiredBeforeTrials: true,
  },
  {
    id: "FI-03",
    description: "Model IDs per provider and tier",
    value: null,
    requiredBeforeTrials: true,
  },
  {
    id: "FI-04",
    description: "Prompt, tool catalog, and tool descriptions",
    value: "task-fixture.json@1.0.0",
    requiredBeforeTrials: true,
  },
  {
    id: "FI-05",
    description: "Host-call facade semantics and latency profiles",
    value: "10ms,100ms,1000ms per call",
    requiredBeforeTrials: true,
  },
  {
    id: "FI-06",
    description: "Hardware fingerprint, timeouts, and resource ceilings",
    value: null,
    requiredBeforeTrials: true,
  },
  {
    id: "FI-07",
    description: "Analysis code, bootstrap seed, and multiplicity rule",
    value: "paired percentile bootstrap, seed 20261109, Holm-Bonferroni",
    requiredBeforeTrials: true,
  },
] as const

export const lang1PreRegistration: Lang1PreRegistration = {
  protocolId: "S1",
  decisionIds: ["LANG-1"],
  datasets: ["cost-v1@1.0.0", "security-v1@1.0.0", "lang-v1@1.0.0"],
  status: "pre-registered",
  frozenInputs: [...frozenInputs],
  arms: [
    {
      arm: "monty",
      candidates: ["pydantic-monty (python-subset)"],
      evaluatorVariantRule:
        "Monty runs under the same OS sandbox; host calls are generated stubs; no ambient OS access unless mounted by the host",
      role: "language-level sandbox candidate",
    },
    {
      arm: "typescript",
      candidates: ["node", "deno"],
      evaluatorVariantRule:
        "The TypeScript arm is evaluated twice (Node and Deno) for NODE-DENO-1; the language result uses the pre-registered winner of NODE-DENO-1, not the faster variant",
      role: "full TS/JS runtime",
    },
    {
      arm: "lua",
      candidates: ["lua5.4", "luau"],
      evaluatorVariantRule:
        "Luau is the evaluator variant because it omits io/os.execute by default; Lua 5.4 is measured as the permissive baseline and recorded separately",
      role: "small embeddable runtime",
    },
    {
      arm: "bash",
      candidates: ["bash"],
      evaluatorVariantRule:
        "Structured result wrapper required; quoting and injection probes are part of the security checks",
      role: "shell plus OS sandbox",
    },
    {
      arm: "cpython",
      candidates: ["cpython3.14"],
      evaluatorVariantRule:
        "Full CPython with pinned wheels; package provenance is recorded per trial",
      role: "full language and package ecosystem",
    },
  ],
  modelSlots: [
    {
      provider: "anthropic",
      tier: "primary",
      modelId: null,
      credentialEnv: "ANTHROPIC_API_KEY",
    },
    {
      provider: "anthropic",
      tier: "smaller",
      modelId: null,
      credentialEnv: "ANTHROPIC_API_KEY",
    },
    {
      provider: "openai",
      tier: "primary",
      modelId: null,
      credentialEnv: "OPENAI_API_KEY",
    },
    {
      provider: "openai",
      tier: "smaller",
      modelId: null,
      credentialEnv: "OPENAI_API_KEY",
    },
  ],
  providersRequired: 2,
  modelsPerProvider: 2,
  smallerModelRequired: true,
  tasks: { suiteId: "lang-v1", suiteVersion: "1.0.0", count: 10 },
  hostCallLatenciesMs: [...LATENCY_PROFILES_MS],
  trialsPerCell: TRIALS_PER_CELL,
  temperature: TEMPERATURE,
  metrics: [
    { id: "task.success", unit: "rate", direction: "higher-is-better", source: "harness" },
    { id: "rubric.coverage", unit: "rate", direction: "higher-is-better", source: "harness" },
    { id: "tokens.input", unit: "tokens", direction: "lower-is-better", source: "provider" },
    { id: "tokens.output", unit: "tokens", direction: "lower-is-better", source: "provider" },
    { id: "tokens.total", unit: "tokens", direction: "lower-is-better", source: "provider" },
    { id: "turns", unit: "turns", direction: "lower-is-better", source: "harness" },
    { id: "toolCalls", unit: "calls", direction: "lower-is-better", source: "harness" },
    { id: "retries", unit: "retries", direction: "lower-is-better", source: "harness" },
    { id: "latency.p50", unit: "ms", direction: "lower-is-better", source: "harness" },
    { id: "latency.p95", unit: "ms", direction: "lower-is-better", source: "harness" },
    { id: "startup.cold", unit: "ms", direction: "lower-is-better", source: "runtime" },
    { id: "rss.peak", unit: "bytes", direction: "lower-is-better", source: "host" },
    { id: "snapshot.available", unit: "boolean", direction: "reported-only", source: "runtime" },
    { id: "snapshot.restoreOk", unit: "boolean", direction: "reported-only", source: "runtime" },
    {
      id: "progressiveDisclosure.chars",
      unit: "chars",
      direction: "lower-is-better",
      source: "harness",
    },
    { id: "security.canaryHits", unit: "count", direction: "lower-is-better", source: "harness" },
    {
      id: "operability.hardFailures",
      unit: "count",
      direction: "lower-is-better",
      source: "harness",
    },
  ],
  hypotheses: [
    {
      id: "H1-success",
      statement:
        "Each candidate arm is non-inferior to the best arm in paired task success and rubric coverage.",
      nonInferiorityMargin:
        "paired difference lower 95% bound > -0.05 absolute success rate and > -0.05 rubric coverage",
      analysis: "paired bootstrap over (task, latency, trial) with Holm-Bonferroni across arms",
    },
    {
      id: "H2-tokens",
      statement:
        "Token cost of a successful task does not materially exceed the TypeScript baseline.",
      nonInferiorityMargin:
        "median paired tokens.total ratio <= 1.25 and upper 95% bound <= 1.50 versus TypeScript",
      analysis: "paired ratio bootstrap; only successful trials are compared",
    },
    {
      id: "H3-latency",
      statement: "Task p95 latency does not materially exceed the TypeScript baseline.",
      nonInferiorityMargin:
        "paired p95 difference <= 25% of baseline p95 + 250 ms at each host-call latency profile",
      analysis: "paired bootstrap of p95 differences per latency profile",
    },
    {
      id: "H4-operability",
      statement:
        "No candidate has an unexplained operability hard failure inside the frozen OS sandbox.",
      nonInferiorityMargin:
        "zero hard failures; every capability failure is recorded and classified",
      analysis: "hard-fail table, no statistical test",
    },
    {
      id: "H5-security",
      statement: "No candidate leaks the enrolled canary inside the frozen OS sandbox.",
      nonInferiorityMargin: "zero canary hits across stdout, stderr, files, and effect arguments",
      analysis: "exact count; any hit is a hard fail",
    },
    {
      id: "H6-snapshot",
      statement:
        "Snapshot/pause-resume capability is sufficient for the approval-gate and fork-branch tasks, or the arm succeeds without runtime snapshots by replaying host-recorded state.",
      nonInferiorityMargin:
        "task success on lang-task-06 and lang-task-07 is non-inferior under H1",
      analysis: "per-task success plus snapshot availability/trust table",
    },
  ],
  hardFails: [
    {
      id: "HF-01",
      condition: "Canary bytes appear in any measured surface",
      consequence: "arm is rejected for LANG-1; defect recorded",
    },
    {
      id: "HF-02",
      condition: "The arm cannot run under the frozen OS-sandbox profile",
      consequence: "capability failure recorded; sandbox is never relaxed for that arm",
    },
    {
      id: "HF-03",
      condition: "A trial mutates the sealed task fixture or prompt set",
      consequence: "trial invalidated; arm result blocked",
    },
    {
      id: "HF-04",
      condition: "A host call escapes the host-call facade with no attribution and no timeout",
      consequence: "arm rejected for the affected task class",
    },
    {
      id: "HF-05",
      condition: "Snapshot restore executes an external effect twice",
      consequence: "arm hard-fails H6 regardless of success rate",
    },
    {
      id: "HF-06",
      condition: "A model cell is missing, blocked, or inconclusive",
      consequence: "LANG-1 has no selection; DG-1 stays open",
    },
  ],
  decisionRules: {
    selection:
      "Select an arm only when every pre-registered cell has complete paired trials, all hard fails pass, and exactly one arm satisfies H1-H5 and is best on the composite of tokens.total (primary) and latency.p95 (secondary).",
    inconclusive:
      "If the confidence intervals overlap the margins or two arms are within 2% on the composite, record inconclusive and leave DG-1 open; do not choose a default.",
    blocked:
      "Missing model manifest entries, credentials, frozen runtime digests, or OS-sandbox posture make the affected cells blocked. Any blocked cell blocks LANG-1 and DG-1.",
    tieBreak:
      "Only after human confirmation: prefer the arm with the smaller ambient capability surface and stronger snapshot/approval support, then the smaller peak RSS. Never infer a tie-break from partial evidence.",
    partialEvidence:
      "Local runtime, operability, and security measurements inform the decision record but never select an arm.",
  },
  analysis: {
    design: "paired-interleaved",
    trialsPerCell: TRIALS_PER_CELL,
    temperature: TEMPERATURE,
    seeds: {
      base: 20261109,
      derivation:
        "sha256(arm|variant|provider|model|task|latency|trialIndex), first 4 bytes mod 2^31",
    },
    confidenceLevel: 0.95,
    bootstrapResamples: 10_000,
    multiplicity: "Holm-Bonferroni across the five arm comparisons per hypothesis family",
    missingData:
      "No sample is dropped. Every failure is classified with the failure taxonomy and remains in the denominator.",
  },
  failureTaxonomyIds: [
    "F-01",
    "F-02",
    "F-03",
    "F-04",
    "F-05",
    "F-06",
    "F-07",
    "F-08",
    "F-09",
    "F-10",
    "F-11",
    "F-12",
    "F-13",
    "F-14",
    "F-15",
    "F-16",
    "F-17",
    "F-18",
    "F-19",
    "F-20",
  ],
  securityProtocol:
    "Every trial runs inside the frozen OS-sandbox profile with the same filesystem bindings, process limits, egress policy, and cgroup/rlimit/wall-clock ceilings. Canary probes cover env, argv, files, stdout/stderr, host-call args, and model context. A runtime that cannot run under the profile records a capability failure; the profile is never weakened.",
  operabilityProtocol:
    "Record startup cold/warm, peak RSS, host-call p50/p95, stream ordering, retry behavior, filesystem grants, dependency availability with versions, snapshot/restore, and approval pause/resume under the same ceilings. Any missing capability is a classified failure, not a silent substitution.",
  progressiveDisclosureProtocol:
    "Measure tool catalog prompt cost at 10, 50, 200, and 1000 tools for full-schema exposure and for search/list/describe progressive disclosure. Provider token counts require the frozen tokenizer and model; character counts are recorded locally and are not a decision metric.",
  reportingRule:
    "Publish pre-registration, raw traces, prompts, tool schemas, failure taxonomy, blocked cells with exact missing inputs, and every residual. Report absolute distributions and paired deltas. Never publish invented or partially simulated results as model outcomes.",
}

export const modelSlotKey = (slot: { provider: string; tier: string }): string =>
  `${slot.provider}:${slot.tier}`
