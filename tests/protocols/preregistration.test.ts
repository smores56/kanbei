import { assertEquals } from "@std/assert"
import { Schema } from "effect"
import { CostV1PreRegistration, costV1PreRegistration } from "../../src/spikes/protocols/cost-v1.ts"
import { UiV1PreRegistration, uiV1PreRegistration } from "../../src/spikes/protocols/ui-v1.ts"

Deno.test("S4 cost pre-registration validates and defers thresholds", () => {
  const decoded = Schema.decodeUnknownSync(CostV1PreRegistration)(costV1PreRegistration)
  assertEquals(decoded.protocolId, "S4")
  assertEquals(decoded.thresholds, "deferred-to-DG-6")
  assertEquals(decoded.repetitions.measuredRuns, 30)
})

Deno.test("S6 UI pre-registration validates and defers thresholds", () => {
  const decoded = Schema.decodeUnknownSync(UiV1PreRegistration)(uiV1PreRegistration)
  assertEquals(decoded.protocolId, "S6")
  assertEquals(decoded.thresholds, "deferred-to-DG-6-and-DG-7")
  assertEquals(decoded.questions.length, 10)
  assertEquals(decoded.presentationReplay.liveModelCallsAllowed, false)
})
