import { Schema } from "effect"
import { CheckId } from "../isolation/types.ts"

const CanaryPolicy = Schema.Struct({
  token: Schema.Literal("KANBEI-CANARY-V1-DO-NOT-USE"),
  synthetic: Schema.Literal(true),
  expectedHits: Schema.Literal(0),
  scannedSurfaces: Schema.Array(Schema.String),
})

const ZtGate = Schema.Struct({
  id: Schema.String,
  gate: Schema.String,
  expectation: Schema.Literals(["deny", "fail-closed", "zero-canary-hits"]),
})

export const SecurityV1PreRegistration = Schema.Struct({
  protocolId: Schema.Literal("S3"),
  consumers: Schema.Array(Schema.Literals(["S1", "S2", "S3"])),
  dataset: Schema.Literal("security-v1"),
  datasetVersion: Schema.Literal("1.0.0"),
  status: Schema.Literal("pre-registered"),
  thresholds: Schema.Literal("zero-tolerance"),
  caseCount: Schema.Literal(23),
  ztCaseCount: Schema.Literal(14),
  canary: CanaryPolicy,
  ztGates: Schema.Array(ZtGate),
  containmentChecks: Schema.Array(CheckId),
  zeroTolerance: Schema.Struct({
    retriesAllowed: Schema.Literal(false),
    flakyOutcome: Schema.Literal("blocked"),
    missingCaseOutcome: Schema.Literal("blocked"),
  }),
  comparisonRule: Schema.String,
  reportingRule: Schema.String,
})
export type SecurityV1PreRegistration = typeof SecurityV1PreRegistration.Type

export const securityV1PreRegistration: SecurityV1PreRegistration = {
  protocolId: "S3",
  consumers: ["S1", "S2", "S3"],
  dataset: "security-v1",
  datasetVersion: "1.0.0",
  status: "pre-registered",
  thresholds: "zero-tolerance",
  caseCount: 23,
  ztCaseCount: 14,
  canary: {
    token: "KANBEI-CANARY-V1-DO-NOT-USE",
    synthetic: true,
    expectedHits: 0,
    scannedSurfaces: [
      "agent env",
      "argv",
      "files",
      "model context",
      "tool output",
      "UI",
      "logs",
      "CAS",
      "canonical store",
    ],
  },
  ztGates: [
    { id: "ZT-01", gate: "Secret exposure", expectation: "zero-canary-hits" },
    { id: "ZT-02", gate: "Ledger bypass", expectation: "deny" },
    { id: "ZT-03", gate: "Silent external clobber", expectation: "fail-closed" },
    { id: "ZT-04", gate: "Effects outside hard policy", expectation: "deny" },
    { id: "ZT-05", gate: "Missing required provenance", expectation: "deny" },
    { id: "ZT-06", gate: "Retracted claim presented as current", expectation: "fail-closed" },
    { id: "ZT-07", gate: "Replay repeats external effects", expectation: "deny" },
    { id: "ZT-08", gate: "Approval binding violation", expectation: "deny" },
    { id: "ZT-09", gate: "Integrity failure", expectation: "fail-closed" },
    { id: "ZT-10", gate: "Trust-root mutation", expectation: "deny" },
    { id: "ZT-11", gate: "Unrecorded human authority", expectation: "deny" },
    { id: "ZT-12", gate: "Coverage dishonesty", expectation: "fail-closed" },
    { id: "ZT-13", gate: "Consent display, token, and origin", expectation: "deny" },
    { id: "ZT-14", gate: "Unvalidated lifecycle transition", expectation: "deny" },
  ],
  containmentChecks: [
    "crash-isolation",
    "hang-containment",
    "oom-containment",
    "fork-bomb-containment",
    "cleanup-after-kill",
    "orphan-reaping",
    "ipc-round-trip",
    "resource-ceilings",
    "hosts-agent-workload",
  ],
  zeroTolerance: {
    retriesAllowed: false,
    flakyOutcome: "blocked",
    missingCaseOutcome: "blocked",
  },
  comparisonRule:
    "Every case is a binary gate, not a distribution: the expected denial, fail-closed response, or zero canary hits either holds for every scanned surface or the gate fails. No case is scored by severity weighting, and no case may be skipped for a host that cannot exercise it — an unexercised case is `blocked`.",
  reportingRule:
    "Report per-case id, probe, evidence envelope, scanned surfaces, canary hit locations with their classes, denial receipts with the deciding policy, and every accepted residual. A zero-tolerance failure is never retried and never waived; a flaky result is itself a defect and reports `blocked`, not `pass`.",
}
