import { assertEquals, assertThrows } from "@std/assert"
import { Schema } from "effect"
import { canonicalize } from "../../src/evidence/canonical.ts"
import { sha256Text } from "../../src/evidence/digest.ts"
import { EvidenceEnvelope } from "../../src/evidence/types.ts"
import * as Effect from "effect/Effect"

const validEnvelope = {
  testId: "ZT-01",
  invariantIds: ["I-01"],
  fixtureVersion: "journey-v1@1.0.0",
  canonicalHead: { seq: 1, hash: "sha256:" + "a".repeat(64) },
  sequenceRange: [1, 1] as const,
  environment: {
    hostFingerprint: "sha256:" + "b".repeat(64),
    kernel: "linux-test",
    runtime: "deno@test",
    sandboxPosture: "test",
    brokerIdentity: "test",
  },
  seeds: [0],
  modelRecordings: [],
  artifacts: [],
  outcome: "pass" as const,
}

Deno.test("canonical JSON sorts keys deterministically", () => {
  assertEquals(canonicalize({ b: 1, a: { d: 2, c: 3 } }), '{"a":{"c":3,"d":2},"b":1}')
})

Deno.test("canonical JSON normalizes negative zero", () => {
  assertEquals(canonicalize(-0), "0")
})

Deno.test("sha256Text is stable", async () => {
  assertEquals(
    await Effect.runPromise(sha256Text("")),
    "sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
  )
})

Deno.test("EvidenceEnvelope accepts a complete envelope", () => {
  assertEquals(Schema.decodeUnknownSync(EvidenceEnvelope)(validEnvelope).testId, "ZT-01")
})

Deno.test("EvidenceEnvelope rejects a missing fixture version", () => {
  const { fixtureVersion: _fixtureVersion, ...invalid } = validEnvelope
  assertThrows(() => Schema.decodeUnknownSync(EvidenceEnvelope)(invalid))
})
