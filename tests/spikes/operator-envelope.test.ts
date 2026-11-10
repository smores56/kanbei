import { assertEquals } from "@std/assert"
import { Schema } from "effect"
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
    spendUsd: 1,
    wallClockMs: 1,
    modelCalls: 1,
    effectCalls: 1,
    decisionRequests: 1,
    decisionsConsumed: 1,
    maxOverrideMultiplier: 1,
  },
} as const

Deno.test("operator envelope schema accepts a complete configured envelope", () => {
  assertEquals(Schema.decodeUnknownSync(OperatorEnvelope)(sample).hardCeilings.spendUsd, 1)
})

Deno.test("operator envelope schema rejects a missing hard ceiling", () => {
  const { spendUsd: _spendUsd, ...incomplete } = sample.hardCeilings
  assertEquals(Schema.is(OperatorEnvelope)({ ...sample, hardCeilings: incomplete }), false)
})
