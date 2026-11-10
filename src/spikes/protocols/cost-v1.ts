import { Schema } from "effect"

const Repetitions = Schema.Struct({
  warmupRuns: Schema.Number,
  measuredRuns: Schema.Number,
  coldStartRuns: Schema.Number,
  trialOrder: Schema.Literal("paired-interleaved"),
})

const Bootstrap = Schema.Struct({
  method: Schema.Literal("bootstrap-percentile"),
  resamples: Schema.Number,
  confidenceLevel: Schema.Number,
  paired: Schema.Literal(true),
})

const Metric = Schema.Struct({
  id: Schema.String,
  unit: Schema.String,
  direction: Schema.Literals(["lower-is-better", "higher-is-better", "reported-only"]),
})

export const CostV1PreRegistration = Schema.Struct({
  protocolId: Schema.Literal("S4"),
  dataset: Schema.Literal("cost-v1"),
  datasetVersion: Schema.Literal("1.0.0"),
  status: Schema.Literal("pre-registered"),
  thresholds: Schema.Literal("deferred-to-DG-6"),
  scenarios: Schema.Array(Schema.String),
  repetitions: Repetitions,
  bootstrap: Bootstrap,
  coldWarmSeparation: Schema.Literal("cold-after-process-exit; warm-after-warmup"),
  enforcedCeilingChecks: Schema.Array(
    Schema.Struct({
      ceiling: Schema.String,
      assertBeforeDispatch: Schema.Literal(true),
      assertObservableAfterDecision: Schema.Literal(true),
    }),
  ),
  metrics: Schema.Array(Metric),
  qualityMetrics: Schema.Array(Schema.String),
  comparisonRule: Schema.String,
  reportingRule: Schema.String,
})
export type CostV1PreRegistration = typeof CostV1PreRegistration.Type

export const costV1PreRegistration: CostV1PreRegistration = {
  protocolId: "S4",
  dataset: "cost-v1",
  datasetVersion: "1.0.0",
  status: "pre-registered",
  thresholds: "deferred-to-DG-6",
  scenarios: [
    "full-journey",
    "idle",
    "fan-out-burst",
    "reconciliation-backlog",
    "external-conflict",
    "backup-restore",
  ],
  repetitions: {
    warmupRuns: 3,
    measuredRuns: 30,
    coldStartRuns: 5,
    trialOrder: "paired-interleaved",
  },
  bootstrap: {
    method: "bootstrap-percentile",
    resamples: 10_000,
    confidenceLevel: 0.95,
    paired: true,
  },
  coldWarmSeparation: "cold-after-process-exit; warm-after-warmup",
  enforcedCeilingChecks: [
    {
      ceiling: "global-spend",
      assertBeforeDispatch: true,
      assertObservableAfterDecision: true,
    },
    {
      ceiling: "global-token",
      assertBeforeDispatch: true,
      assertObservableAfterDecision: true,
    },
    {
      ceiling: "global-rate",
      assertBeforeDispatch: true,
      assertObservableAfterDecision: true,
    },
    {
      ceiling: "global-concurrency",
      assertBeforeDispatch: true,
      assertObservableAfterDecision: true,
    },
    {
      ceiling: "sandbox-memory",
      assertBeforeDispatch: true,
      assertObservableAfterDecision: true,
    },
    {
      ceiling: "sandbox-cpu",
      assertBeforeDispatch: true,
      assertObservableAfterDecision: true,
    },
    {
      ceiling: "sandbox-pids",
      assertBeforeDispatch: true,
      assertObservableAfterDecision: true,
    },
    {
      ceiling: "cas-admission",
      assertBeforeDispatch: true,
      assertObservableAfterDecision: true,
    },
  ],
  metrics: [
    { id: "latency.p50", unit: "ms", direction: "lower-is-better" },
    { id: "latency.p95", unit: "ms", direction: "lower-is-better" },
    { id: "cpu.seconds", unit: "s", direction: "lower-is-better" },
    { id: "rss.peak", unit: "bytes", direction: "lower-is-better" },
    { id: "disk.growth", unit: "bytes", direction: "lower-is-better" },
    { id: "cas.growth", unit: "bytes", direction: "lower-is-better" },
    { id: "tokens.total", unit: "tokens", direction: "lower-is-better" },
    { id: "spend.total", unit: "currency", direction: "lower-is-better" },
    { id: "provider.rate", unit: "requests", direction: "reported-only" },
    { id: "freshness.coverage-lag", unit: "ms", direction: "lower-is-better" },
  ],
  qualityMetrics: [
    "task-success",
    "decision-queue-age",
    "decision-to-effect",
    "reconciliation-catch-up",
    "stale-retracted-reads",
    "ui-comprehension",
  ],
  comparisonRule:
    "Compare paired runs within the same scenario, host, fixture, concurrency, and approved configuration. Report absolute distributions and paired deltas. Do not select a winner when the pre-registered decision rule is inconclusive.",
  reportingRule:
    "Report raw samples, p50/p95, bootstrap confidence intervals, cold/warm separation, enforcement evidence, and every accepted residual. No sample may be dropped without an explicit failure class.",
}
