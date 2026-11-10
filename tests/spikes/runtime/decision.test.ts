import { assert, assertEquals } from "@std/assert"
import { summarize } from "../../../src/spikes/runtime/metrics.ts"
import type { ArmResult, RuntimeName } from "../../../src/spikes/runtime/schema.ts"
import { decide } from "../../../src/spikes/runtime/decision.ts"
import { PRE_REGISTRATION } from "../../../src/spikes/runtime/plan.ts"

const arm = (
  runtime: RuntimeName,
  overrides: Partial<ArmResult> = {},
): ArmResult => ({
  runtime,
  runtimeVersion: runtime === "node" ? "v24.21.0" : "2.9.2",
  sqliteVersion: "3.53.2",
  boot: {
    coldReady: summarize([100, 110, 120]),
    coldStart: summarize([120, 130, 140]),
    warmStart: summarize([10, 11, 12]),
  },
  memory: {
    rssAfterBoot: summarize([90]),
    rssAfterLoadBytes: 100_000_000,
    peakRssBytes: 100 * 1_048_576,
  },
  eventLoop: summarize([1, 2, 3]),
  ledger: {
    appends: 100,
    appendsPerSecond: 200,
    append: summarize([5, 6, 7]),
    read: summarize([0.01, 0.02]),
    walMode: "wal",
    readYourWrites: true,
    hashChainValid: true,
  },
  correctness: [
    { id: "wal-and-synchronous-full", pass: true, detail: "ok" },
    { id: "integrity-after-load", pass: true, detail: "ok" },
    { id: "appends-preserved", pass: true, detail: "issued=100 stored=100" },
  ],
  singleWriter: {
    concurrentWriterCommitted: false,
    failClosed: true,
    recoveryOk: true,
    busyBlockedEventLoop: true,
    busyBlockMs: 252,
  },
  isolation: {
    workerStartupMs: 70,
    roundtrip: summarize([0.2, 0.3]),
    crashObserved: true,
    parentSurvivedCrash: true,
  },
  signals: { handlerInvoked: true, shutdownMs: 20, exitCode: 0 },
  permissions: [
    { id: "fs-write", denied: true, code: "ERR_ACCESS_DENIED", model: "test" },
    { id: "process-spawn", denied: true, code: "ERR_ACCESS_DENIED", model: "test" },
    { id: "fs-read-outside-scope", denied: true, code: "ERR_ACCESS_DENIED", model: "test" },
  ],
  packaging: {
    runtimeBinaryBytes: 80_000_000,
    singleBinaryBytes: 0,
    singleBinaryStatus: "blocked",
    singleBinaryNote: "test",
  },
  ...overrides,
})

const arms = (node: ArmResult, deno: ArmResult): Readonly<Record<RuntimeName, ArmResult>> => ({
  node,
  deno,
})

Deno.test("a clean pair inside the decision margin falls to the pre-registered tie-breaker", () => {
  const node = arm("node", { memory: { ...arm("node").memory, peakRssBytes: 140 * 1_048_576 } })
  const deno = arm("deno", { memory: { ...arm("deno").memory, peakRssBytes: 100 * 1_048_576 } })
  const outcome = decide(arms(node, deno), PRE_REGISTRATION)
  assert(outcome.composite.margin < PRE_REGISTRATION.decisionMargin)
  assertEquals(outcome.hardFails, [])
  assertEquals(outcome.decision.value, "deno")
  assertEquals(outcome.decision.rule, "tie-breaker: peak RSS")
  assertEquals(outcome.rejectedAlternative.value, "node")
})

Deno.test("a composite margin above the registered margin decides without a tie-breaker", () => {
  const fast = arm("node", {
    boot: { ...arm("node").boot, coldReady: summarize([10, 10, 10]) },
  })
  const slow = arm("deno", {
    boot: { ...arm("deno").boot, coldReady: summarize([200, 200, 200]) },
  })
  const outcome = decide(arms(fast, slow), PRE_REGISTRATION)
  assert(outcome.composite.margin >= PRE_REGISTRATION.decisionMargin)
  assertEquals(outcome.decision.value, "node")
  assertEquals(outcome.decision.rule, "weighted composite")
  assert(outcome.rejectedAlternative.reasons.length > 0)
})

Deno.test("a hard fail disqualifies an arm even when its composite is better", () => {
  const clean = arm("deno")
  const broken = arm("node", {
    correctness: [
      { id: "wal-and-synchronous-full", pass: false, detail: "journal_mode=delete" },
    ],
    boot: { ...arm("node").boot, coldReady: summarize([1, 1, 1]) },
  })
  const outcome = decide(arms(broken, clean), PRE_REGISTRATION)
  assertEquals(outcome.hardFails.map((failure) => failure.id), ["HF-CORRECTNESS"])
  assertEquals(outcome.decision.value, "deno")
  assertEquals(outcome.decision.rule, "hard-fail elimination")
})

Deno.test("both arms tripping a hard fail yields inconclusive and rejects no alternative", () => {
  const brokenNode = arm("node", {
    singleWriter: { ...arm("node").singleWriter, concurrentWriterCommitted: true },
  })
  const brokenDeno = arm("deno", {
    signals: { handlerInvoked: false, shutdownMs: 0, exitCode: -1 },
  })
  const outcome = decide(arms(brokenNode, brokenDeno), PRE_REGISTRATION)
  assertEquals(outcome.decision.value, "inconclusive")
  assertEquals(outcome.rejectedAlternative.value, "none")
  assert(outcome.hardFails.length >= 2)
})

Deno.test("a blocked decision measurement yields inconclusive without imputing a value", () => {
  const outcome = decide(arms(arm("node"), arm("deno")), PRE_REGISTRATION, [
    "node: no measurements",
  ])
  assertEquals(outcome.decision.value, "inconclusive")
  assertEquals(outcome.decision.rule, "blocked measurement")
  assert(outcome.decision.rationale.some((line) => line.includes("forbids imputing")))
})

Deno.test("a permission probe that was supposed to deny but succeeded is a hard fail", () => {
  const failOpen = arm("deno", {
    permissions: [
      { id: "fs-write", denied: false, code: "ALLOWED", model: "deno-inner" },
      { id: "net-connect", denied: true, code: "NotCapable", model: "deno-inner" },
      { id: "process-spawn", denied: true, code: "NotCapable", model: "deno-inner" },
      { id: "fs-read-outside-scope", denied: true, code: "NotCapable", model: "deno-inner" },
    ],
  })
  const outcome = decide(arms(arm("node"), failOpen), PRE_REGISTRATION)
  assertEquals(outcome.hardFails.map((failure) => failure.id), ["HF-PERMISSION"])
  assertEquals(outcome.decision.value, "node")
})

Deno.test("an unconfigured network probe does not count as fail-open for Node", () => {
  const node = arm("node", {
    permissions: [
      { id: "fs-write", denied: true, code: "ERR_ACCESS_DENIED", model: "node-process-flag" },
      { id: "net-connect", denied: false, code: "ECONNREFUSED", model: "node-process-flag" },
      { id: "process-spawn", denied: true, code: "ERR_ACCESS_DENIED", model: "node-process-flag" },
      {
        id: "fs-read-outside-scope",
        denied: true,
        code: "ERR_ACCESS_DENIED",
        model: "node-process-flag",
      },
    ],
  })
  const outcome = decide(arms(node, arm("deno")), PRE_REGISTRATION)
  assertEquals(outcome.hardFails, [])
})

Deno.test("tail latency and append tail thresholds come from the pre-registration", () => {
  const slow = arm("node", {
    eventLoop: summarize([PRE_REGISTRATION.thresholds.eventLoopP95Ms + 50]),
    ledger: {
      ...arm("node").ledger,
      append: summarize([PRE_REGISTRATION.thresholds.appendP95Ms + 50]),
    },
  })
  const outcome = decide(arms(slow, arm("deno")), PRE_REGISTRATION)
  assertEquals(
    outcome.hardFails.map((failure) => failure.id).toSorted(),
    ["HF-APPEND-TAIL", "HF-TAIL-LATENCY"],
  )
})
