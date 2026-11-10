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
import { ModelManifest } from "../../../src/spikes/language/model-arm.ts"

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

Deno.test("pre-registration fixes one provider with a primary and a smaller NeuralWatt model", () => {
  const providers = [...new Set(lang1PreRegistration.modelSlots.map((slot) => slot.provider))]
  assertEquals(providers, ["neuralwatt"])
  assertEquals(providers.length, lang1PreRegistration.providersRequired)
  assertEquals(lang1PreRegistration.providersRequired, 1)
  assertEquals(lang1PreRegistration.modelsPerProvider, 2)
  for (const provider of providers) {
    const tiers = lang1PreRegistration.modelSlots
      .filter((slot) => slot.provider === provider)
      .map((slot) => slot.tier)
      .toSorted()
    assertEquals(tiers, ["primary", "smaller"])
  }
  assertEquals(
    lang1PreRegistration.modelSlots.every((slot) => slot.credentialEnv === "NEURALWATT_API_KEY"),
    true,
  )
  assertEquals(lang1PreRegistration.smallerModelRequired, true)
  assertEquals(modelSlotKey({ provider: "neuralwatt", tier: "smaller" }), "neuralwatt:smaller")
})

Deno.test("the single-provider amendment is recorded before any trial", () => {
  const amendments = lang1PreRegistration.amendments
  assertEquals(amendments.length, 1)
  assertEquals(amendments[0]?.id, "AMD-1")
  assertEquals(amendments[0]?.recordedAt, "2026-11-10")
  assertEquals(amendments[0]?.beforeTrials, true)
  assertEquals(
    amendments[0]?.changes.includes("providersRequired 2 -> 1"),
    true,
  )
  assertEquals(lang1PreRegistration.status, "pre-registered")
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

Deno.test("pre-registration freezes the model IDs through FI-03 and the manifest", async () => {
  const modelInput = lang1PreRegistration.frozenInputs.find((input) => input.id === "FI-03")
  assertEquals(modelInput?.value?.includes("deepseek-v4.1-flash"), true)
  assertEquals(modelInput?.value?.includes("qwen-3.8-27b"), true)
  assertEquals(modelInput?.requiredBeforeTrials, true)
  const manifest = Schema.decodeUnknownSync(ModelManifest)(
    JSON.parse(
      await Deno.readTextFile(
        new URL("../../../spikes/language/model-manifest.json", import.meta.url),
      ),
    ),
  )
  const manifestIds = manifest.providers.flatMap((provider) =>
    lang1PreRegistration.modelSlots
      .filter((slot) => slot.provider === provider.provider)
      .map((slot) => slot.tier === "primary" ? provider.primary : provider.smaller)
  )
  assertEquals(manifestIds, lang1PreRegistration.modelSlots.map((slot) => slot.modelId))
})

Deno.test("hypothesis margins are explicit and reference paired analysis", () => {
  for (const hypothesis of lang1PreRegistration.hypotheses) {
    assertEquals(hypothesis.nonInferiorityMargin.length > 10, true)
    assertEquals(hypothesis.analysis.length > 5, true)
  }
  assertEquals(lang1PreRegistration.analysis.design, "paired-interleaved")
  assertEquals(lang1PreRegistration.analysis.bootstrapResamples >= 1000, true)
})
