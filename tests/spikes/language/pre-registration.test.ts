import { assertEquals, assertExists } from "@std/assert"
import { Schema } from "effect"
import {
  Lang1PreRegistration,
  lang1PreRegistration,
  modelSlotKey,
} from "../../../src/spikes/language/pre-registration.ts"
import {
  LATENCY_PROFILES_MS,
  TEMPERATURE,
  TRIALS_PER_CELL,
} from "../../../src/spikes/language/types.ts"

Deno.test("LANG-1 pre-registration validates and fixes the spike protocol", () => {
  const decoded = Schema.decodeUnknownSync(Lang1PreRegistration)(lang1PreRegistration)
  assertEquals(decoded.protocolId, "S1")
  assertEquals(decoded.decisionIds, ["LANG-1"])
  assertEquals(decoded.datasets.includes("cost-v1@1.0.0"), true)
  assertEquals(decoded.datasets.includes("security-v1@1.0.0"), true)
  assertEquals(decoded.hostCallLatenciesMs, [...LATENCY_PROFILES_MS])
  assertEquals(decoded.trialsPerCell, TRIALS_PER_CELL)
  assertEquals(decoded.temperature, TEMPERATURE)
})

Deno.test("pre-registration requires two providers with a primary and a smaller model each", () => {
  const providers = [...new Set(lang1PreRegistration.modelSlots.map((slot) => slot.provider))]
  assertEquals(providers.length, lang1PreRegistration.providersRequired)
  for (const provider of providers) {
    const tiers = lang1PreRegistration.modelSlots
      .filter((slot) => slot.provider === provider)
      .map((slot) => slot.tier)
      .toSorted()
    assertEquals(tiers, ["primary", "smaller"])
  }
  assertEquals(lang1PreRegistration.smallerModelRequired, true)
  assertEquals(modelSlotKey({ provider: "anthropic", tier: "smaller" }), "anthropic:smaller")
})

Deno.test("pre-registration declares every required metric and a blocked-safe decision rule", () => {
  const ids = new Set(lang1PreRegistration.metrics.map((metric) => metric.id))
  for (
    const required of [
      "task.success",
      "tokens.total",
      "turns",
      "toolCalls",
      "retries",
      "latency.p50",
      "latency.p95",
      "startup.cold",
      "rss.peak",
      "snapshot.available",
      "progressiveDisclosure.chars",
      "security.canaryHits",
    ]
  ) {
    assertEquals(ids.has(required), true, `missing metric ${required}`)
  }
  assertExists(lang1PreRegistration.decisionRules.blocked)
  assertEquals(lang1PreRegistration.decisionRules.blocked.includes("blocked cell"), true)
  assertEquals(
    lang1PreRegistration.decisionRules.inconclusive.includes("do not choose a default"),
    true,
  )
  assertEquals(
    lang1PreRegistration.hardFails.some((fail) => fail.id === "HF-06"),
    true,
  )
})

Deno.test("pre-registration keeps model IDs unfrozen and requires them before trials", () => {
  assertEquals(lang1PreRegistration.modelSlots.every((slot) => slot.modelId === null), true)
  const modelInput = lang1PreRegistration.frozenInputs.find((input) => input.id === "FI-03")
  assertEquals(modelInput?.value, null)
  assertEquals(modelInput?.requiredBeforeTrials, true)
})

Deno.test("hypothesis margins are explicit and reference paired analysis", () => {
  for (const hypothesis of lang1PreRegistration.hypotheses) {
    assertEquals(hypothesis.nonInferiorityMargin.length > 10, true)
    assertEquals(hypothesis.analysis.length > 5, true)
  }
  assertEquals(lang1PreRegistration.analysis.design, "paired-interleaved")
  assertEquals(lang1PreRegistration.analysis.bootstrapResamples >= 1000, true)
})
