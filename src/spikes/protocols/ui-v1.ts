import { Schema } from "effect"

const Question = Schema.Struct({
  id: Schema.String,
  prompt: Schema.String,
  target: Schema.Literals(["contested", "retracted", "suspect", "stale", "current", "approval"]),
  requiredEvidence: Schema.Array(Schema.String),
})

export const UiV1PreRegistration = Schema.Struct({
  protocolId: Schema.Literal("S6"),
  dataset: Schema.Literal("ui-v1"),
  datasetVersion: Schema.Literal("1.0.0"),
  status: Schema.Literal("pre-registered"),
  thresholds: Schema.Literal("deferred-to-DG-6-and-DG-7"),
  baseline: Schema.Literal("raw-ledger-transcript"),
  questions: Schema.Array(Question),
  presentationReplay: Schema.Struct({
    required: Schema.Literal(true),
    liveModelCallsAllowed: Schema.Literal(false),
    selectionAndGestureState: Schema.Literal("preserved"),
  }),
  interruptGrouping: Schema.Struct({
    runGrouping: Schema.Literal("one-interrupt-group-per-run"),
    canonicalRulings: Schema.Literal("individual"),
    resumeRule: Schema.Literal("resume-complete-for-its-group"),
  }),
  accessibility: Schema.Struct({
    accessibilityTreeRequired: Schema.Literal(true),
    textFallbackRequired: Schema.Literal(true),
  }),
  comparisonRule: Schema.String,
  reportingRule: Schema.String,
})
export type UiV1PreRegistration = typeof UiV1PreRegistration.Type

export const uiV1PreRegistration: UiV1PreRegistration = {
  protocolId: "S6",
  dataset: "ui-v1",
  datasetVersion: "1.0.0",
  status: "pre-registered",
  thresholds: "deferred-to-DG-6-and-DG-7",
  baseline: "raw-ledger-transcript",
  questions: [
    {
      id: "UQ-01",
      prompt: "Which claim is currently working, and which alternatives remain contested?",
      target: "contested",
      requiredEvidence: ["claim id", "provenance", "alternatives", "working preference"],
    },
    {
      id: "UQ-02",
      prompt: "Which claim was retracted, and what remains inspectable?",
      target: "retracted",
      requiredEvidence: ["retraction record", "prior claim", "derivation history"],
    },
    {
      id: "UQ-03",
      prompt: "Which claims are suspect but not yet rebuilt?",
      target: "suspect",
      requiredEvidence: ["suspect mark", "coverage watermark", "pending rebuild count"],
    },
    {
      id: "UQ-04",
      prompt: "Is this view current, stale, or outside coverage?",
      target: "stale",
      requiredEvidence: ["watermark", "pending work", "staleness label"],
    },
    {
      id: "UQ-05",
      prompt: "What exact evidence, scope, hashes, tier, and constraints will this approval bind?",
      target: "approval",
      requiredEvidence: ["consent digest", "plan hash", "work ids", "scope", "expiry"],
    },
    {
      id: "UQ-06",
      prompt: "Which external commit changed state, and what speculative branch was preserved?",
      target: "current",
      requiredEvidence: ["external commit", "changed paths", "preserved branch", "conflict"],
    },
    {
      id: "UQ-07",
      prompt: "What capability and dependency changes does this candidate add or remove?",
      target: "current",
      requiredEvidence: ["capability diff", "dependency diff", "activation scope"],
    },
    {
      id: "UQ-08",
      prompt: "What checkpoint does this undo fork from, and which descendants remain?",
      target: "current",
      requiredEvidence: ["checkpoint", "branch set", "preserved descendants"],
    },
    {
      id: "UQ-09",
      prompt: "Which decisions are grouped by intent and deadline, and which evidence is shared?",
      target: "approval",
      requiredEvidence: ["intent group", "deadline", "shared evidence", "per-request evidence"],
    },
    {
      id: "UQ-10",
      prompt:
        "Can every critical graph state be recovered from the accessibility tree or text fallback?",
      target: "current",
      requiredEvidence: ["accessibility tree", "text fallback", "critical state labels"],
    },
  ],
  presentationReplay: {
    required: true,
    liveModelCallsAllowed: false,
    selectionAndGestureState: "preserved",
  },
  interruptGrouping: {
    runGrouping: "one-interrupt-group-per-run",
    canonicalRulings: "individual",
    resumeRule: "resume-complete-for-its-group",
  },
  accessibility: {
    accessibilityTreeRequired: true,
    textFallbackRequired: true,
  },
  comparisonRule:
    "Ask the same ten questions against the raw-ledger transcript and the structured view. Report per-question correctness, time, and evidence drill-down depth as distributions. Do not collapse the two baselines into one score.",
  reportingRule:
    "Report raw answers, scorer protocol, inter-rater agreement, confidence intervals, and every question that required expanding hidden evidence. Thresholds are set only after the baseline, at DG-6 and DG-7.",
}
