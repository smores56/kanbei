import { assertEquals } from "@std/assert"
import { Duration, Schema } from "effect"
import { OperatorEnvelope } from "../../src/spikes/gates/operator-envelope.ts"

const sample = {
  hardCeilings: {
    generation: "nix-generation-test",
    window: "calendar_month",
    spendUsd: 1,
    tokensIn: 1,
    tokensOut: 1,
    modelRequestsPerWindow: 1,
    brokerRequestsPerWindow: 1,
    concurrentLoops: 1,
  },
  defaultLoopBudget: {
    tokens: 1,
    spend_usd: 1,
    wall_clock: Duration.seconds(1),
    model_calls: 1,
    effect_calls: 1,
    decision_requests: 1,
    decisions_consumed: 1,
    max_override_multiplier: 1,
  },
} as const

Deno.test("operator envelope schema accepts a complete configured envelope", () => {
  assertEquals(Schema.decodeUnknownSync(OperatorEnvelope)(sample).hardCeilings.spendUsd, 1)
})

Deno.test("operator envelope schema rejects a missing hard ceiling", () => {
  const { spendUsd: _spendUsd, ...incomplete } = sample.hardCeilings
  assertEquals(Schema.is(OperatorEnvelope)({ ...sample, hardCeilings: incomplete }), false)
})
