import { assert, assertEquals } from "@std/assert"
import { Schema } from "effect"
import { CHECK_IDS } from "../../src/spikes/isolation/types.ts"
import { CostV1PreRegistration, costV1PreRegistration } from "../../src/spikes/protocols/cost-v1.ts"
import {
  SecurityV1PreRegistration,
  securityV1PreRegistration,
} from "../../src/spikes/protocols/security-v1.ts"
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

const ZT_IDS = [
  "ZT-01",
  "ZT-02",
  "ZT-03",
  "ZT-04",
  "ZT-05",
  "ZT-06",
  "ZT-07",
  "ZT-08",
  "ZT-09",
  "ZT-10",
  "ZT-11",
  "ZT-12",
  "ZT-13",
  "ZT-14",
] as const

const ContainmentCase = Schema.Struct({
  id: Schema.String,
  probe: Schema.String,
  expectation: Schema.String,
})

const SecurityDataset = Schema.Struct({
  schemaVersion: Schema.Literal(1),
  dataset: Schema.Literal("security-v1"),
  version: Schema.Literal("1.0.0"),
  canary: Schema.Struct({
    token: Schema.String,
    synthetic: Schema.Literal(true),
    expectedHits: Schema.Literal(0),
  }),
  surfaces: Schema.Array(Schema.String),
  ztCases: Schema.Array(
    Schema.Struct({
      id: Schema.String,
      surface: Schema.String,
      expectation: Schema.String,
      probe: Schema.String,
    }),
  ),
  containmentCases: Schema.Array(ContainmentCase),
})

Deno.test("S3 security pre-registration validates as zero-tolerance", () => {
  const decoded = Schema.decodeUnknownSync(SecurityV1PreRegistration)(securityV1PreRegistration)
  assertEquals(decoded.protocolId, "S3")
  assertEquals(decoded.datasetVersion, "1.0.0")
  assertEquals(decoded.status, "pre-registered")
  assertEquals(decoded.thresholds, "zero-tolerance")
  assertEquals(decoded.canary.synthetic, true)
  assertEquals(decoded.canary.expectedHits, 0)
  assertEquals(decoded.zeroTolerance.retriesAllowed, false)
  assertEquals(decoded.consumers, ["S1", "S2", "S3"])
})

Deno.test("security-v1 fixture covers exactly ZT-01..ZT-14 and every S3 containment check", async () => {
  const raw = await Deno.readTextFile(
    new URL("../../fixtures/security-v1/cases.json", import.meta.url),
  )
  const dataset = Schema.decodeUnknownSync(SecurityDataset)(JSON.parse(raw))

  assertEquals(dataset.ztCases.map((entry) => entry.id), [...ZT_IDS])
  assertEquals(
    dataset.containmentCases.map((entry) => entry.id).toSorted(),
    [...CHECK_IDS].toSorted(),
  )
  assertEquals(dataset.surfaces, securityV1PreRegistration.canary.scannedSurfaces)
  assertEquals(dataset.canary.token, securityV1PreRegistration.canary.token)

  // The fixture and the pre-registration are frozen independently; they must still agree.
  assertEquals(
    dataset.ztCases.map((entry) => [entry.id, entry.expectation]),
    securityV1PreRegistration.ztGates.map((entry) => [entry.id, entry.expectation]),
  )
  assert(
    dataset.ztCases.every((entry) => entry.probe.length > 0 && entry.surface.length > 0),
  )
  assertEquals(
    securityV1PreRegistration.caseCount,
    dataset.ztCases.length + dataset.containmentCases.length,
  )
})
