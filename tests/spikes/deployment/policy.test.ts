import { assertEquals, assertStringIncludes } from "@std/assert"
import { Effect, Exit } from "effect"
import { assessArm, buildRecommendation } from "../../../src/spikes/deployment/deployment-plane.ts"
import { decidePosture, guardedEffect } from "../../../src/spikes/deployment/policy.ts"
import type {
  DeclaredPosture,
  ObservedCapabilities,
} from "../../../src/spikes/deployment/policy.ts"
import { probe, unavailable } from "../../../src/spikes/deployment/probe.ts"
import { renderReport } from "../../../src/spikes/deployment/report.ts"
import type { ProbeResult, SpikeResult } from "../../../src/spikes/deployment/types.ts"
import { runWithProbeEnv } from "./support.ts"

const sameUidPosture: DeclaredPosture = {
  secretIsolation: "same-uid",
  filesystemIsolation: "landlock",
  hardCeilings: "cgroup",
}

const healthy: ObservedCapabilities = {
  landlockAbi: 6,
  ptraceScope: 1,
  userNamespacesEnabled: true,
  cgroupDelegated: true,
  auditSinkWritable: true,
}

const probeOf = (
  id: string,
  status: ProbeResult["status"],
  enforcement: ProbeResult["enforcement"],
  category: ProbeResult["category"] = "same-uid-process",
): ProbeResult => ({
  id,
  category,
  arm: "both",
  method: "synthetic",
  status,
  observed: "synthetic observation",
  evidence: "synthetic evidence",
  ...(enforcement === undefined ? {} : { enforcement }),
})

Deno.test("a declared capability the kernel does not expose denies the posture", () => {
  const decision = decidePosture(sameUidPosture, { ...healthy, landlockAbi: 0 })
  assertEquals(decision.decision, "denied")
  assertStringIncludes(decision.reasons.join(" "), "Landlock")
})

Deno.test("an unwritable audit sink denies the posture", () => {
  const decision = decidePosture(sameUidPosture, { ...healthy, auditSinkWritable: false })
  assertEquals(decision.decision, "denied")
  assertStringIncludes(decision.reasons.join(" "), "audit sink")
})

Deno.test("a same-UID posture is allowed only with its secret-isolation residual recorded", () => {
  const decision = decidePosture(sameUidPosture, healthy)
  assertEquals(decision.decision, "allowed")
  assertStringIncludes(decision.acceptedResiduals.join(" "), "broker secrets are readable")
})

Deno.test("a guarded effect is denied without running when the audit sink is unwritable", async () => {
  const ran = { value: false }
  const exit = await Effect.runPromise(
    Effect.exit(
      guardedEffect(
        false,
        Effect.sync(() => {
          ran.value = true
        }),
      ),
    ),
  )
  assertEquals(Exit.isFailure(exit), true)
  assertEquals(ran.value, false)
})

Deno.test("a blocked kernel claim is unverified, never kernel-enforced", () => {
  const arm = assessArm(
    [probeOf("PR-02", "blocked", { pure: "kernel", dedicated: "kernel" })],
    "pure",
  )
  assertEquals(arm.kernelEnforced, [])
  assertEquals(arm.unverified, ["PR-02"])
})

Deno.test("a same-UID measurement never counts as kernel enforcement for the dedicated-UID arm", () => {
  const arm = assessArm(
    [probeOf("PR-02", "pass", { pure: "kernel", dedicated: "kernel" })],
    "dedicated",
  )
  assertEquals(arm.kernelEnforced, [])
  assertEquals(arm.unverified, ["PR-02"])
  assertEquals(arm.secretIsolation, "unverified")
})

Deno.test("a measured conventional failure makes pure user-space secret isolation none", () => {
  const arm = assessArm(
    [probeOf(
      "PR-05",
      "fail",
      { pure: "conventional", dedicated: "kernel" },
      "secret-reachability",
    )],
    "pure",
  )
  assertEquals(arm.secretIsolation, "none")
  assertEquals(arm.conventional, ["PR-05"])
})

Deno.test("the recommendation is the hybrid variant when pure user-space secret isolation is none", () => {
  const recommendation = buildRecommendation([
    {
      arm: "pure-user-space",
      kernelEnforced: [],
      conventional: ["PR-05"],
      unverified: [],
      secretIsolation: "none",
      note: "measured",
    },
    {
      arm: "dedicated-uid",
      kernelEnforced: [],
      conventional: [],
      unverified: ["BI-03"],
      secretIsolation: "unverified",
      note: "unverified",
    },
  ])
  assertEquals(recommendation.variant, "D")
  assertStringIncludes(recommendation.requiredConditions.join(" "), "kanbei-broker")
})

Deno.test("an unavailable probe body becomes blocked and never pass", async () => {
  const result = await runWithProbeEnv(
    probe(
      {
        id: "T-01",
        category: "fail-closed",
        arm: "both",
        method: "synthetic unavailable probe",
      },
      unavailable("capability absent", "detail"),
    ),
  )
  assertEquals(result.status, "blocked")
  assertStringIncludes(result.observed, "capability absent")
})

Deno.test("a probe defect becomes blocked because no measurement exists", async () => {
  const result = await runWithProbeEnv(
    probe(
      {
        id: "T-02",
        category: "fail-closed",
        arm: "both",
        method: "synthetic defect",
      },
      Effect.die(new Error("boom")),
    ),
  )
  assertEquals(result.status, "blocked")
})

const syntheticResult = (): SpikeResult => {
  const probes: ReadonlyArray<ProbeResult> = [
    probeOf("PR-05", "fail", { pure: "conventional", dedicated: "kernel" }, "secret-reachability"),
    probeOf("BI-02", "blocked", { pure: "not-applicable", dedicated: "kernel" }, "broker-identity"),
  ]
  return {
    schemaVersion: 1,
    protocol: "DEP-1",
    spike: "S2",
    dataset: "security-v1",
    host: {
      hostname: "test-host",
      kernel: "linux-test",
      user: "tester",
      uid: 1000,
      groups: ["users"],
      systemd: "systemd 261",
      cgroupVersion: "v2",
      yamaPtraceScope: "1",
      landlockAbi: "6",
      maxUserNamespaces: "1000",
      linger: "no",
      delegatedControllers: ["cpu", "memory", "pids"],
      ambientCapabilities: "0000000000000000",
    },
    probes,
    arms: [assessArm(probes, "pure"), assessArm(probes, "dedicated")],
    recommendation: buildRecommendation([
      assessArm(probes, "pure"),
      assessArm(probes, "dedicated"),
    ]),
    decisionGate: {
      gate: "DG-3",
      status: "input-ready-human-signature-required",
      blocks: ["E1", "E2", "H7"],
      evidenceProbes: ["PR-05", "BI-02"],
      statement: "decision input",
    },
    residuals: ["residual one"],
    blockedCapabilities: ["BI-02: unavailable"],
  }
}

Deno.test("the report lists every probe, every blocked capability, and the DG-3 impact", () => {
  const report = renderReport(syntheticResult())
  assertStringIncludes(report, "PR-05")
  assertStringIncludes(report, "BI-02")
  assertStringIncludes(report, "Unavailable capabilities")
  assertStringIncludes(report, "DG-3")
  assertStringIncludes(report, "Variant D")
  assertStringIncludes(report, "| PR-05 | secret-reachability | fail |")
})
