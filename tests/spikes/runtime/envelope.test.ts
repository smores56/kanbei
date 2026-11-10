import { assertEquals } from "@std/assert"
import { DenoServices } from "@effect/platform-deno"
import { Effect, Layer } from "effect"
import { fileArtifactStoreLayer } from "../../../src/evidence/artifact-store.ts"
import { fixedEnvironmentProbeLayer } from "../../../src/evidence/environment.ts"
import { fixtureStoreLayer } from "../../../src/evidence/fixture-store.ts"
import type { SpikeResultDocument } from "../../../src/spikes/runtime/schema.ts"
import {
  appendFixtureReference,
  outcomeOf,
  sealSpikeEvidence,
} from "../../../src/spikes/runtime/envelope.ts"
import { PRE_REGISTRATION } from "../../../src/spikes/runtime/plan.ts"

const environmentLayer = fixedEnvironmentProbeLayer({
  hostFingerprint: "sha256:" + "c".repeat(64),
  kernel: "linux-test",
  runtime: "deno@test",
  sandboxPosture: "test",
  brokerIdentity: "test",
})

const dependencies = (casRoot: string) =>
  Layer.mergeAll(
    fileArtifactStoreLayer(casRoot),
    fixtureStoreLayer(Deno.cwd()),
    environmentLayer,
  ).pipe(Layer.provide(DenoServices.layer))

const document: Omit<SpikeResultDocument, "envelope"> = {
  schemaVersion: 1,
  protocol: "S1",
  decisionId: "NODE-DENO-1",
  preRegistration: PRE_REGISTRATION,
  environment: {
    hostFingerprint: "sha256:" + "d".repeat(64),
    kernel: "linux-test",
    runtime: "deno@test",
    sandboxPosture: "test",
    brokerIdentity: "test",
  },
  arms: {
    node: fixtureArm("node"),
    deno: fixtureArm("deno"),
  },
  measurements: [
    {
      id: "coldReady",
      label: "cold start to ready",
      unit: "ms",
      direction: "lower",
      node: 100,
      deno: 90,
      ratio: 1.1111,
      winner: "deno",
    },
  ],
  hardFails: [],
  composite: { node: 1.01, deno: 1, margin: 0.01 },
  decision: { value: "deno", rule: "weighted composite", rationale: ["test"] },
  rejectedAlternative: { value: "node", reasons: ["test"] },
  residualRisks: ["test"],
  blocked: [],
  unboundProbes: [],
}

function fixtureArm(runtime: "node" | "deno"): SpikeResultDocument["arms"]["node"] {
  return {
    runtime,
    runtimeVersion: "test",
    sqliteVersion: "3.53.2",
    boot: {
      coldReady: { count: 1, min: 1, max: 1, mean: 1, p50: 1, p95: 1 },
      coldStart: { count: 1, min: 1, max: 1, mean: 1, p50: 1, p95: 1 },
      warmStart: { count: 1, min: 1, max: 1, mean: 1, p50: 1, p95: 1 },
    },
    memory: {
      rssAfterBoot: { count: 1, min: 1, max: 1, mean: 1, p50: 1, p95: 1 },
      rssAfterLoadBytes: 1,
      peakRssBytes: 1,
    },
    eventLoop: { count: 1, min: 1, max: 1, mean: 1, p50: 1, p95: 1 },
    ledger: {
      appends: 1,
      appendsPerSecond: 1,
      append: { count: 1, min: 1, max: 1, mean: 1, p50: 1, p95: 1 },
      read: { count: 1, min: 1, max: 1, mean: 1, p50: 1, p95: 1 },
      walMode: "wal",
      readYourWrites: true,
      hashChainValid: true,
    },
    correctness: [],
    singleWriter: {
      concurrentWriterCommitted: false,
      failClosed: true,
      recoveryOk: true,
      busyBlockedEventLoop: true,
      busyBlockMs: 250,
    },
    isolation: {
      workerStartupMs: 1,
      roundtrip: { count: 1, min: 1, max: 1, mean: 1, p50: 1, p95: 1 },
      crashObserved: true,
      parentSurvivedCrash: true,
    },
    signals: { handlerInvoked: true, shutdownMs: 1, exitCode: 0 },
    permissions: [],
    packaging: {
      runtimeBinaryBytes: 1,
      singleBinaryBytes: 0,
      singleBinaryStatus: "blocked",
      singleBinaryNote: "test",
    },
  }
}

Deno.test("a spike envelope binds the fixture, records the artifact and carries the spike outcome", async () => {
  const cas = await Deno.makeTempDir({ prefix: "kanbei-spike-envelope-" })
  try {
    const envelope = await Effect.runPromise(
      sealSpikeEvidence(document, appendFixtureReference("1.0.0"), outcomeOf("deno", [])).pipe(
        Effect.provide(dependencies(cas)),
      ),
    )
    assertEquals(envelope.testId, "NODE-DENO-1")
    assertEquals(envelope.fixtureVersion, "append-v1@1.0.0")
    assertEquals(envelope.outcome, "pass")
    assertEquals(envelope.artifacts.length, 1)
    assertEquals(envelope.modelRecordings.length, 0)
    assertEquals(envelope.metrics?.["node.coldReady"], 100)
    assertEquals(envelope.metrics?.["deno.coldReady"], 90)
    assertEquals(envelope.environment.runtime, "deno@test")
  } finally {
    await Deno.remove(cas, { recursive: true })
  }
})

Deno.test("an inconclusive decision produces an inconclusive envelope, not a pass", async () => {
  const cas = await Deno.makeTempDir({ prefix: "kanbei-spike-envelope-" })
  try {
    const envelope = await Effect.runPromise(
      sealSpikeEvidence(document, appendFixtureReference("1.0.0"), outcomeOf("inconclusive", []))
        .pipe(
          Effect.provide(dependencies(cas)),
        ),
    )
    assertEquals(envelope.outcome, "inconclusive")
  } finally {
    await Deno.remove(cas, { recursive: true })
  }
})

Deno.test("a blocked decision produces a blocked envelope", () => {
  assertEquals(outcomeOf("deno", ["node: no measurements"]), "blocked")
  assertEquals(outcomeOf("inconclusive", []), "inconclusive")
  assertEquals(outcomeOf("node", []), "pass")
})
