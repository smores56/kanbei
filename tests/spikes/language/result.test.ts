import { assertEquals, assertStringIncludes } from "@std/assert"
import { Effect } from "effect"
import { buildModelMatrix } from "../../../src/spikes/language/model-arm.ts"
import { progressiveDisclosureRows } from "../../../src/spikes/language/progressive-disclosure.ts"
import { renderReport } from "../../../src/spikes/language/report.ts"
import { buildResult } from "../../../src/spikes/language/result.ts"
import type { LocalMeasurements } from "../../../src/spikes/language/result.ts"
import { FAILURE_TAXONOMY } from "../../../src/spikes/language/taxonomy.ts"
import { TASK_SUITE, TASKS } from "../../../src/spikes/language/tasks.ts"

const local: LocalMeasurements = {
  runtimes: [],
  hostCalls: [],
  progressiveDisclosure: progressiveDisclosureRows(),
  security: [],
  snapshot: [],
}

const matrix = buildModelMatrix({
  manifest: undefined,
  credentials: { ANTHROPIC_API_KEY: false, OPENAI_API_KEY: false },
  sandboxFrozen: false,
  registeredDatasets: { "cost-v1@1.0.0": true, "security-v1@1.0.0": false },
})

const build = () =>
  Effect.runPromise(
    buildResult({
      environment: {
        hostFingerprint: "sha256:" + "a".repeat(64),
        kernel: "linux-test",
        runtime: "deno@test",
        sandboxPosture: "unverified",
        brokerIdentity: "unconfigured",
        hostname: "test-host",
        cpus: 4,
      },
      taskSuite: {
        suiteId: TASK_SUITE.suiteId,
        suiteVersion: TASK_SUITE.suiteVersion,
        digest: "sha256:" + "b".repeat(64),
        tasks: TASKS,
      },
      local,
      modelMatrix: matrix,
      failureTaxonomy: FAILURE_TAXONOMY,
      generatedBy: "test",
    }),
  )

Deno.test("a blocked model arm keeps DG-1 open and recommends nothing", async () => {
  const result = await build()
  assertEquals(result.evidence.outcome, "blocked")
  assertEquals(result.evidence.envelope, "not-issued")
  assertEquals(result.decisionRecord.status, "open")
  assertEquals(result.decisionRecord.recommendation, "none")
  assertEquals(result.modelMatrix.status, "blocked")
  assertEquals(result.modelMatrix.modelResults, "none")
  assertEquals(result.modelMatrix.cells.every((cell) => cell.trials === 0), true)
  assertEquals(result.modelMatrix.plannedTrials, 18_000)
  assertEquals(result.localMeasurements.runtimes.length, 0)
})

Deno.test("the report states the block, publishes the matrix, and claims no model result", async () => {
  const result = await build()
  const report = renderReport(result)
  assertStringIncludes(report, "Status: **blocked**")
  assertStringIncludes(report, "No model results are reported.")
  assertStringIncludes(report, "## 1. Pre-registration")
  assertStringIncludes(report, "## 4. Local measurements")
  assertStringIncludes(report, "## 5. Blocked model matrix")
  assertStringIncludes(report, "## 6. Failure taxonomy")
  assertStringIncludes(report, "## 7. Recommendation status")
  assertStringIncludes(report, "Gate `DG-1` status: **open**; recommendation: **none**.")
  assertStringIncludes(report, "completed model trials: 0")
  assertEquals(report.includes("Selected arm"), false)
  assertEquals(report.includes("selected arm"), false)
})

Deno.test("result remains schema-valid with a fully blocked matrix", async () => {
  const result = await build()
  assertEquals(result.failureTaxonomy.length, 20)
  assertEquals(result.preRegistration.trialsPerCell, 30)
  assertEquals(result.taskSuite.tasks.length, 10)
  assertEquals(result.localMeasurements.progressiveDisclosure.length, 4)
  assertEquals(result.residuals.some((line) => line.includes("No frozen OS-sandbox")), true)
})
