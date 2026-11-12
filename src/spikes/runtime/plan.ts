import type { PreRegistration } from "./schema.ts"

/**
 * Frozen before measurement (task `A1`, `S1` / `NODE-DENO-1`).
 *
 * Every number the decision depends on lives here, so the run cannot pick its
 * thresholds after seeing the data. Changing this file invalidates the run and
 * requires a new `frozenRevision`.
 */
export const PRE_REGISTRATION: PreRegistration = {
  protocol: "S1",
  decisionId: "NODE-DENO-1",
  dataset: { id: "append-v1", version: "1.0.0" },
  arms: { node: "node@24.21.0", deno: "deno@2.9.2" },
  sample: {
    coldStartRuns: 11,
    warmStartIterations: 40,
    loopSamplerIntervalMs: 1,
    workerRuns: 5,
  },
  load: {
    appends: 1000,
    // One durable append per event-loop turn: the lag sampler then measures
    // scheduling latency under real write load instead of a harness-made burst.
    appendChunk: 1,
    readsPerChunk: 1,
  },
  hardFails: [
    {
      id: "HF-CORRECTNESS",
      condition: "any SQLite correctness check reports pass=false",
      rationale:
        "A driver that returns wrong rows or loses a committed append cannot hold the canonical ledger.",
    },
    {
      id: "HF-SINGLE-WRITER",
      condition:
        "a second connection commits while the first holds BEGIN IMMEDIATE, or a rejected concurrent write is visible afterwards, or the ledger cannot recover after the lock is released",
      rationale:
        "I-02 requires one sequencer; SQLite must refuse a concurrent writer and leave no partial row.",
    },
    {
      id: "HF-INTEGRITY",
      condition: "PRAGMA integrity_check is not 'ok' after the load",
      rationale: "ZT-09 refuses a store whose integrity check cannot pass.",
    },
    {
      id: "HF-DATA-LOSS",
      condition: "appends recorded in SQLite differ from appends issued",
      rationale: "I-01 forbids losing an acknowledged append.",
    },
    {
      id: "HF-TAIL-LATENCY",
      condition: "event-loop lag p95 exceeds 250 ms during the append/read load",
      rationale:
        "The control plane arbitrates approvals and sandbox supervision on the same loop; a quarter-second tail starves those paths.",
    },
    {
      id: "HF-APPEND-TAIL",
      condition: "append latency p95 exceeds 100 ms",
      rationale:
        "Append is the canonical write path; a 100 ms durable write tail is unusable interactively.",
    },
    {
      id: "HF-ISOLATION",
      condition:
        "a crashing worker terminates the parent process or the crash is not observed by the parent",
      rationale:
        "Agent work runs off-loop; a worker crash must be a recoverable event, not a control-plane outage.",
    },
    {
      id: "HF-SIGNAL",
      condition:
        "SIGTERM handler is not invoked, or shutdown takes longer than 3000 ms, or exit code is non-zero",
      rationale: "systemd stop must be graceful and observable for the supervisor contract.",
    },
    {
      id: "HF-PERMISSION",
      condition: "an operation the runtime was told to deny succeeds",
      rationale:
        "Fail-closed is required (ZT-04); a permission model that fails open is worse than none.",
    },
  ],
  compositeWeights: [
    {
      id: "coldReady",
      weight: 0.25,
      rationale: "Control-plane restarts should not stall an approval queue.",
    },
    { id: "peakRss", weight: 0.15, rationale: "The control plane shares a host with agents." },
    {
      id: "eventLoopP95",
      weight: 0.2,
      rationale:
        "Tail latency decides whether approvals and supervisor ticks meet their deadlines.",
    },
    { id: "appendP95", weight: 0.2, rationale: "Canonical durable append latency." },
    {
      id: "warmStart",
      weight: 0.1,
      rationale: "The supervisor restarts components without a process restart.",
    },
    { id: "workerStartup", weight: 0.1, rationale: "Per-task isolation cost." },
  ],
  thresholds: { eventLoopP95Ms: 250, appendP95Ms: 100, shutdownMs: 3000 },
  permissionExpectations: {
    // Node 24 has no network capability, so net-connect is not a denial case for it.
    node: ["fs-write", "process-spawn", "fs-read-outside-scope"],
    deno: ["fs-write", "net-connect", "process-spawn", "fs-read-outside-scope"],
  },
  decisionRule:
    "Hard fails disqualify an arm. If both arms are clean, the arm with the lower weighted composite cost wins when the relative margin is at least decisionMargin; otherwise the pre-registered tie-breakers are applied in order, and if they do not separate the arms the result is inconclusive. A blocked measurement yields inconclusive; no value is imputed.",
  nonInferiorityMargin: 0.2,
  decisionMargin: 0.1,
  tieBreakers: [
    "lower peak RSS",
    "lower cold start to ready",
    "inner capability model present (Deno permissions) versus opt-in process flag",
  ],
  frozenRevision: "a1-node-deno-1-prereg-v1",
}
