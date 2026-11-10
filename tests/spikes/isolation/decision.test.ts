import { assertEquals } from "@std/assert"
import {
  buildMatrix,
  eligibilityFailure,
  recommend,
} from "../../../src/spikes/isolation/decision.ts"
import { CHECK_IDS } from "../../../src/spikes/isolation/types.ts"
import { arm, checks, metrics } from "./sample.ts"

Deno.test("measured arm passing every gate is eligible", () => {
  const candidate = arm("process-cgroup")
  assertEquals(eligibilityFailure(candidate), undefined)
})

Deno.test("blocked arm reports its blocking reason and is never selected", () => {
  const blocked = arm("microvm", { status: "blocked", blockedReason: "no runtime on PATH" })
  assertEquals(eligibilityFailure(blocked), "status blocked: no runtime on PATH")
  const recommendation = recommend([arm("process-cgroup"), blocked])
  assertEquals(recommendation.selected, "process-cgroup")
  assertEquals(recommendation.ineligible, [{
    arm: "microvm",
    reason: "status blocked: no runtime on PATH",
  }])
})

Deno.test("failing containment check makes the arm ineligible", () => {
  const candidate = arm("wasm-instance", {
    checks: checks({ "hosts-agent-workload": "fail" }),
  })
  assertEquals(eligibilityFailure(candidate), "containment failed: hosts-agent-workload")
})

Deno.test("density failures at target concurrency make the arm ineligible", () => {
  const candidate = arm("process-cgroup", {
    metrics: metrics({
      density: [
        {
          agents: 16,
          outcome: "partial",
          readyMs: 1200,
          totalRssBytes: 0,
          perAgentRssBytes: 0,
          failures: 2,
        },
      ],
    }),
  })
  assertEquals(eligibilityFailure(candidate), "density failed at 16 agents (2 failures)")
})

Deno.test("cold start above the pre-registered budget makes the arm ineligible", () => {
  const candidate = arm("process-cgroup", {
    metrics: metrics({ coldStartMs: { p50Ms: 300, p95Ms: 900, maxMs: 950, samples: 5 } }),
  })
  assertEquals(eligibilityFailure(candidate), "p95 cold start 900 ms exceeds 500 ms")
})

Deno.test("per-agent RSS above budget makes the arm ineligible", () => {
  const candidate = arm("process-cgroup", {
    metrics: metrics({ rssPerAgentBytes: 96 * 1024 * 1024 }),
  })
  assertEquals(eligibilityFailure(candidate), "per-agent RSS 96.0 MiB exceeds 64 MiB")
})

Deno.test("root requirement or too many host dependencies makes the arm ineligible", () => {
  const withRoot = arm("process-cgroup", {
    operability: { ...arm("process-cgroup").operability, requiresRoot: true },
  })
  assertEquals(eligibilityFailure(withRoot), "requires root")
  const manyDependencies = arm("microvm", {
    operability: {
      ...arm("microvm").operability,
      hostDependencies: ["a", "b", "c"],
    },
  })
  assertEquals(eligibilityFailure(manyDependencies), "3 host dependencies exceed 2")
})

Deno.test("among eligible arms the lowest p95 cold start wins, then RSS, then IPC", () => {
  const fast = arm("wasm-instance", {
    metrics: metrics({ coldStartMs: { p50Ms: 10, p95Ms: 20, maxMs: 25, samples: 5 } }),
  })
  const slow = arm("process-cgroup")
  const recommendation = recommend([slow, fast])
  assertEquals(recommendation.status, "recommend")
  assertEquals(recommendation.selected, "wasm-instance")
  assertEquals(recommendation.eligible, ["process-cgroup", "wasm-instance"])
})

Deno.test("no eligible arm yields no selection so the gate stays open", () => {
  const recommendation = recommend([
    arm("microvm", { status: "blocked", blockedReason: "absent" }),
    arm("wasm-instance", { checks: checks({ "hosts-agent-workload": "fail" }) }),
  ])
  assertEquals(recommendation.status, "no-selection")
  assertEquals(recommendation.selected, undefined)
  assertEquals(recommendation.eligible, [])
  assertEquals(recommendation.rationale.length, 3)
})

Deno.test("containment matrix has a row per check and a column per arm", () => {
  const matrix = buildMatrix([
    arm("process-cgroup"),
    arm("microvm", { status: "blocked", checks: [] }),
  ])
  assertEquals(matrix.map((row) => row.check), [...CHECK_IDS])
  assertEquals(matrix[0]?.cells, { "process-cgroup": "pass", microvm: "not-run" })
})
