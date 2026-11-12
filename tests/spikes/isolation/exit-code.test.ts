import { assertEquals } from "@std/assert"
import { exitVerdict } from "../../../src/spikes/isolation/exit-code.ts"
import type { SpikeResult } from "../../../src/spikes/isolation/types.ts"
import { arm, checks, supervisor } from "./sample.ts"
import type { EvidenceEnvelope } from "../../../src/evidence/types.ts"

const envelope: EvidenceEnvelope = {
  testId: "S3",
  invariantIds: ["crash-isolation"],
  fixtureVersion: "security-v1@1.0.0",
  canonicalHead: { seq: 0, hash: `sha256:${"0".repeat(64)}` },
  sequenceRange: [0, 0],
  environment: {
    hostFingerprint: `sha256:${"1".repeat(64)}`,
    kernel: "linux-test",
    runtime: "deno@test",
    sandboxPosture: "unverified",
    brokerIdentity: "unconfigured",
  },
  seeds: [0],
  modelRecordings: [],
  artifacts: [`sha256:${"2".repeat(64)}`],
  outcome: "pass",
}

const result = (overrides: Partial<SpikeResult> = {}): SpikeResult => ({
  schemaVersion: 3,
  spikeId: "S3",
  validationId: "ISO-1",
  dataset: "security-v1",
  generatedAt: "2026-01-01T00:00:00.000Z",
  host: {
    kernel: "linux-test",
    runtime: "deno@test",
    cpus: 4,
    totalMemoryBytes: 8 * 1024 * 1024 * 1024,
    cgroupVersion: "v2",
    cgroupControllers: ["memory", "pids", "cpu"],
    userNamespaces: "available",
    landlockAbi: 7,
    cgroupNsdelegate: true,
    delegatedParent: "owned-by-uid",
    kvmDevice: "absent",
    microvmRuntimes: [],
    generalVmm: [],
    wasmEngine: "v8",
    privilegedOperations: false,
  },
  supervisor: supervisor(),
  targetConcurrency: 16,
  security: { propertyClaimed: false, statement: "no claim", excluded: [] },
  arms: [
    arm("process-cgroup"),
    arm("microvm", { status: "blocked", blockedReason: "no runtime", checks: [] }),
  ],
  matrix: [],
  recommendation: {
    decision: "DG-4",
    status: "recommend",
    recommended: "process-cgroup",
    rule: "rule",
    targetConcurrency: 16,
    eligible: ["process-cgroup"],
    ineligible: [],
    rationale: ["clean"],
  },
  residuals: [],
  evidence: { status: "emitted", envelope },
  ...overrides,
})

Deno.test("a clean recommendation with emitted evidence exits 0", () => {
  assertEquals(exitVerdict(result()).code, 0)
  assertEquals(exitVerdict(result()).reasons, [])
})

Deno.test("a measured arm failing a gating check exits non-zero even when DG-4 recommends", () => {
  const verdict = exitVerdict(result({
    arms: [
      arm("process-cgroup"),
      arm("wasm-instance", { checks: checks({ "hosts-agent-workload": "fail" }) }),
      arm("microvm", { status: "blocked", blockedReason: "no runtime", checks: [] }),
    ],
  }))
  assertEquals(verdict.code, 1)
  assertEquals(
    verdict.reasons[0],
    "measured arm wasm-instance failed eligibility-gating checks: hosts-agent-workload=fail",
  )
})

Deno.test("not-applicable checks do not block a clean exit", () => {
  const verdict = exitVerdict(result({
    arms: [
      arm("process-cgroup", { checks: checks({ "orphan-reaping": "not-applicable" }) }),
    ],
  }))
  assertEquals(verdict.code, 0)
})

Deno.test("no recommendation or missing evidence exits non-zero", () => {
  const recommendation = result().recommendation
  const noSelection = exitVerdict(result({
    recommendation: {
      ...recommendation,
      status: "no-selection",
      eligible: [],
      ineligible: [{ arm: "process-cgroup", reason: "budget" }],
    },
  }))
  assertEquals(noSelection.code, 1)
  assertEquals(noSelection.reasons[0], "DG-4 produced no recommendation")

  const unavailable = exitVerdict(result({ evidence: { status: "unavailable", reason: "store" } }))
  assertEquals(unavailable.code, 1)
  assertEquals(unavailable.reasons[0], "the evidence envelope was not emitted")
})
