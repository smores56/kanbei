import { assertEquals, assertStringIncludes } from "@std/assert"
import { Schema } from "effect"
import { renderReport } from "../../../src/spikes/isolation/report.ts"
import { SECURITY_STATEMENT } from "../../../src/spikes/isolation/protocol.ts"
import { CHECK_IDS, SpikeResult } from "../../../src/spikes/isolation/types.ts"
import type { SpikeResult as SpikeResultType } from "../../../src/spikes/isolation/types.ts"
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

const sampleResult = (): SpikeResultType => {
  const arms = [
    arm("process-cgroup"),
    arm("wasm-instance", { checks: checks({ "hosts-agent-workload": "fail" }) }),
    arm("microvm", { status: "blocked", blockedReason: "no runtime", checks: [] }),
  ]
  return {
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
    security: {
      propertyClaimed: false,
      statement: SECURITY_STATEMENT,
      excluded: ["credential boundaries"],
    },
    arms,
    matrix: [],
    recommendation: {
      decision: "DG-4",
      status: "recommend",
      recommended: "process-cgroup",
      rule: "rule",
      targetConcurrency: 16,
      eligible: ["process-cgroup"],
      ineligible: [{ arm: "microvm", reason: "status blocked: no runtime" }],
      rationale: ["process-cgroup is the only arm that satisfies every pre-registered gate"],
    },
    residuals: ["a residual"],
    evidence: { status: "emitted", envelope },
  }
}

Deno.test("report states that no security property is claimed", () => {
  const report = renderReport(sampleResult())
  assertStringIncludes(report, "## No security property is claimed")
  assertStringIncludes(report, SECURITY_STATEMENT)
})

Deno.test("report names every arm and every containment check", () => {
  const report = renderReport(sampleResult())
  for (const id of ["process-cgroup", "wasm-instance", "microvm"]) {
    assertStringIncludes(report, id)
  }
  for (const check of CHECK_IDS) assertStringIncludes(report, check)
})

Deno.test("report carries the DG-4 outcome and operability costs", () => {
  const report = renderReport(sampleResult())
  assertStringIncludes(report, "Decision input for `DG-4`")
  assertStringIncludes(report, "recommend `process-cgroup`")
  assertStringIncludes(report, "## Operability costs")
  assertStringIncludes(report, "## Residuals")
})

Deno.test("report renders the supervisor and the emitted evidence envelope", () => {
  const report = renderReport(sampleResult())
  assertStringIncludes(report, "## Supervisor")
  assertStringIncludes(report, "rootless-userns-chroot-landlock")
  assertStringIncludes(report, "dependency 1")
  assertStringIncludes(report, "fixture `security-v1@1.0.0`")
  assertStringIncludes(report, "outcome `pass`")
  assertStringIncludes(report, "systemd-run --user --property=Delegate=yes")
})

Deno.test("emitted result.json decodes and matches the committed report", async () => {
  const resultText = await Deno.readTextFile(
    new URL("../../../spikes/isolation/result.json", import.meta.url),
  )
  const result = Schema.decodeUnknownSync(SpikeResult)(JSON.parse(resultText))
  assertEquals(result.security.propertyClaimed, false)
  assertEquals(result.validationId, "ISO-1")
  assertEquals(result.targetConcurrency, 16)
  const committed = await Deno.readTextFile(
    new URL("../../../spikes/isolation/REPORT.md", import.meta.url),
  )
  assertEquals(committed, renderReport(result))
})
