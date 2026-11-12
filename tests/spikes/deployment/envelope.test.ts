import { assert, assertEquals, assertStringIncludes } from "@std/assert"
import { Schema } from "effect"
import { SAME_UID_INVARIANTS } from "../../../src/spikes/deployment/envelope.ts"
import {
  DEPLOYMENT_SCOPE,
  renderReport,
  SECURITY_SCOPE,
} from "../../../src/spikes/deployment/report.ts"
import { SpikeResult } from "../../../src/spikes/deployment/types.ts"

const published = (): SpikeResult =>
  Schema.decodeUnknownSync(SpikeResult)(
    JSON.parse(Deno.readTextFileSync("spikes/deployment/result.json")),
  )

Deno.test("the deployment result binds security-v1 and records the same-UID finding as fail", () => {
  const result = published()
  const envelope = result.envelope
  assertEquals(envelope.testId, "DEP-1")
  assertEquals(envelope.fixtureVersion, "security-v1@1.0.0")
  assertEquals(envelope.outcome, "fail")
  assertEquals(envelope.invariantIds, [...SAME_UID_INVARIANTS])
  assertEquals(envelope.invariantIds, [
    "same-uid-process-inspection",
    "same-uid-signal",
    "secret-fd-reachability",
  ])
  assertEquals(envelope.sequenceRange, [0, 0])
  assert(envelope.artifacts.length >= 1)
})

Deno.test("the fail envelope is a finding and leaves the DG-3 decision text unchanged", () => {
  const result = published()
  const report = renderReport(result)
  assertStringIncludes(report, `- outcome: \`fail\``)
  assertStringIncludes(report, SECURITY_SCOPE)
  assertStringIncludes(report, DEPLOYMENT_SCOPE)
  assertStringIncludes(report, `- covered invariants: ${SAME_UID_INVARIANTS.join(", ")}`)
  assertStringIncludes(report, result.decisionGate.statement)
  assertStringIncludes(report, `**Variant ${result.recommendation.variant}.**`)
})
