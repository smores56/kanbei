import { assert, assertEquals } from "@std/assert"
import { Schema } from "effect"
import { PreRegistration, SpikeResultDocument } from "../../../src/spikes/runtime/schema.ts"
import { PRE_REGISTRATION } from "../../../src/spikes/runtime/plan.ts"
import { METRICS } from "../../../src/spikes/runtime/decision.ts"

const readJson = (path: string): unknown => JSON.parse(Deno.readTextFileSync(path))

Deno.test("the frozen pre-registration matches the published result", () => {
  const published = readJson("spikes/runtime/result.json")
  const decoded = Schema.decodeUnknownSync(SpikeResultDocument)(published)
  assertEquals(decoded.preRegistration, PRE_REGISTRATION)
  assertEquals(Schema.decodeUnknownSync(PreRegistration)(PRE_REGISTRATION), PRE_REGISTRATION)
})

Deno.test("result.json satisfies the spike result schema and binds the append fixture", () => {
  const published = Schema.decodeUnknownSync(SpikeResultDocument)(
    readJson("spikes/runtime/result.json"),
  )
  assertEquals(published.schemaVersion, 1)
  assertEquals(published.protocol, "S1")
  assertEquals(published.decisionId, "NODE-DENO-1")
  assertEquals(published.envelope.testId, "NODE-DENO-1")
  assertEquals(published.envelope.fixtureVersion, "append-v1@1.0.0")
  assertEquals(published.envelope.sequenceRange, [0, 0])
  assert(published.envelope.artifacts.length >= 1)
})

Deno.test("every composite weight names a metric that the decision table reports", () => {
  const ids = new Set(METRICS.map((metric) => metric.id))
  for (const weight of PRE_REGISTRATION.compositeWeights) {
    assert(ids.has(weight.id), `composite weight ${weight.id} has no metric`)
  }
  const total = PRE_REGISTRATION.compositeWeights.reduce((sum, weight) => sum + weight.weight, 0)
  assertEquals(Number(total.toFixed(6)), 1)
})

Deno.test("the published envelope outcome agrees with the published decision", () => {
  const published = Schema.decodeUnknownSync(SpikeResultDocument)(
    readJson("spikes/runtime/result.json"),
  )
  if (published.unboundProbes.length > 0) {
    assertEquals(published.envelope.outcome, "blocked")
  } else if (published.decision.value === "inconclusive") {
    assertEquals(published.envelope.outcome, "inconclusive")
  } else if (published.blocked.some((entry) => entry.measurement === "arm-measurements")) {
    assertEquals(published.envelope.outcome, "blocked")
  } else {
    assertEquals(published.envelope.outcome, "pass")
  }
})

Deno.test("the security probes are flagged as unbound from a sealed dataset", () => {
  const published = Schema.decodeUnknownSync(SpikeResultDocument)(
    readJson("spikes/runtime/result.json"),
  )
  assert(published.unboundProbes.some((probe) => probe.includes("security-v1")))
})
