import { assertEquals } from "@std/assert"
import { loadFixture, reportText, runJourney } from "../../src/journey/runner.ts"

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

Deno.test("the evidence envelopes bind the real ledger head and declare host residuals", async () => {
  const { envelopes, report } = await runJourney(Deno.cwd())
  const fixture = await loadFixture(Deno.cwd())
  const ingest = envelopes.find((envelope) => envelope.testId === "J2-INGEST")
  if (ingest === undefined) throw new Error("J2-INGEST envelope is missing")
  assertEquals(ingest.canonicalHead, fixture.canonicalHead)
  assertEquals(ingest.canonicalHead.seq, report.metrics.ledgerRecords)
  const csp = envelopes.find((envelope) => envelope.testId === "J2-CSP")
  const search = envelopes.find((envelope) => envelope.testId === "J2-SEARCH")
  assertEquals(
    csp?.acceptedResiduals?.some((residual) => residual.includes("host-owned")),
    true,
  )
  assertEquals(
    search?.acceptedResiduals?.some((residual) => residual.includes("Search VM")),
    true,
  )
  // A step with no residual must not carry an empty claim.
  for (const envelope of envelopes) {
    if (envelope.testId !== "J2-CSP" && envelope.testId !== "J2-SEARCH") {
      assertEquals(envelope.acceptedResiduals ?? [], [])
    }
  }
})

Deno.test("the recorded report keeps every accepted residual, not just the envelope", async () => {
  const { report } = await runJourney(Deno.cwd())
  assertEquals(
    report.acceptedResiduals.some((residual) => residual.includes("host-owned")),
    true,
  )
  assertEquals(
    report.acceptedResiduals.some((residual) => residual.includes("Search VM")),
    true,
  )
  const recorded = JSON.parse(
    await Deno.readTextFile(`${Deno.cwd()}/docs/evidence/journey-v2.json`),
  ) as { readonly acceptedResiduals: ReadonlyArray<string> }
  assertEquals(recorded.acceptedResiduals, report.acceptedResiduals)
  const csp = report.steps.find((step) => step.testId === "J2-CSP")
  assertEquals(csp?.acceptedResiduals.length, 1)
})
