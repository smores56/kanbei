import { assertEquals } from "@std/assert"
import { reportText, runJourney } from "../../src/journey/runner.ts"

Deno.test("journey-v2 passes every step and threshold on the sealed fixture", async () => {
  const { report } = await runJourney(Deno.cwd())
  assertEquals(report.pass, true)
  assertEquals(report.violations, [])
  assertEquals(report.steps.map((step) => step.testId), [
    "J2-INGEST",
    "J2-FRESHNESS",
    "J2-ADVICE",
    "J2-MEMORY",
    "J2-TODO",
    "J2-SEARCH",
    "J2-CSP",
    "J2-BOUNDARY",
  ])
  assertEquals(report.metrics.advisoryPrecision, 1)
  assertEquals(report.metrics.advisoryRecall, 1)
  assertEquals(report.metrics.staleAsFresh, 0)
  assertEquals(report.metrics.capViolations, 0)
  assertEquals(report.metrics.channelLeaks, 0)
})

Deno.test("a rerun on an unchanged head produces the same digest as the recorded run", async () => {
  const first = await runJourney(Deno.cwd())
  const second = await runJourney(Deno.cwd())
  assertEquals(first.report.runDigest, second.report.runDigest)
  const recorded = await Deno.readTextFile(`${Deno.cwd()}/docs/evidence/journey-v2.json`)
  assertEquals(recorded, reportText(first.report))
})
