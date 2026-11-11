import { assertEquals } from "@std/assert"
import { DenoServices } from "@effect/platform-deno"
import { Effect, Layer } from "effect"
import { fileBlobSinkLayer } from "../../src/adapters/blob-sink.ts"
import { fileLedgerStoreLayer } from "../../src/adapters/ledger-file.ts"
import { fileArtifactStoreLayer } from "../../src/evidence/artifact-store.ts"
import { sha256Text } from "../../src/evidence/digest.ts"
import { fixedEnvironmentProbeLayer } from "../../src/evidence/environment.ts"
import { fixtureStoreLayer } from "../../src/evidence/fixture-store.ts"
import { runEvidence } from "../../src/evidence/harness.ts"
import type { FixtureReference } from "../../src/evidence/types.ts"
import { ContentStore, contentStoreLayer } from "../../src/kernel/cas.ts"
import { Ledger, ledgerLayer } from "../../src/kernel/ledger.ts"
import { publishCapture, readCaptures } from "../../src/kernel/observation.ts"
import { buildProjection, projectionDigest } from "../../src/kernel/projection.ts"
import { withTempDir } from "../support.ts"

const EPOCH = "epoch-1"

const kernelServices = (directory: string) =>
  Layer.mergeAll(
    ledgerLayer(EPOCH).pipe(Layer.provide(fileLedgerStoreLayer(`${directory}/ledger.jsonl`))),
    contentStoreLayer.pipe(Layer.provide(fileBlobSinkLayer(`${directory}/cas`))),
  )

const capture = {
  origin: "tuta-mail",
  sequence: 1,
  resourceKey: "message/1",
  locator: "fixture:message/1",
  capturedAt: "2026-11-11T00:00:00.000Z",
  mediaType: "application/json",
  coverage: { complete: true, gaps: 0, suppressed: 0 },
} as const

Deno.test("one capture traverses CAS, manifest, and ledger and reads back after restart", async () => {
  await withTempDir("observation", async (directory) => {
    const bytes = new TextEncoder().encode('{"subject":"hello"}')
    const first = await Effect.runPromise(
      Effect.gen(function* () {
        const cas = yield* ContentStore
        const published = yield* publishCapture({ ...capture, bytes, idempotencyKey: "obs|1" })
        const manifest = yield* cas.readManifest(published.observation.manifest)
        const body = yield* cas.read(published.observation.body, manifest.size)
        return { published, manifest, body, captures: yield* readCaptures() }
      }).pipe(Effect.provide(kernelServices(directory))),
    )
    assertEquals(first.published.record.seq, 1)
    assertEquals(
      first.published.observation.body,
      await Effect.runPromise(sha256Text('{"subject":"hello"}')),
    )
    assertEquals(first.manifest.digest, first.published.observation.body)
    assertEquals(new TextDecoder().decode(first.body), '{"subject":"hello"}')
    assertEquals(first.captures.length, 1)

    const restarted = await Effect.runPromise(
      Effect.gen(function* () {
        const cas = yield* ContentStore
        const ledger = yield* Ledger
        const captures = yield* readCaptures()
        const body = yield* cas.read(captures[0]?.observation.body ?? "", first.manifest.size)
        return { captures, body, head: yield* ledger.head }
      }).pipe(Effect.provide(kernelServices(directory))),
    )
    assertEquals(restarted.head.seq, 1)
    assertEquals(restarted.captures[0]?.record.record_hash, first.published.record.record_hash)
    assertEquals(new TextDecoder().decode(restarted.body), '{"subject":"hello"}')
  })
})

Deno.test("a projection rebuild from the committed rows is digest-identical", async () => {
  await withTempDir("observation", async (directory) => {
    const programs = Effect.gen(function* () {
      const ledger = yield* Ledger
      yield* publishCapture({ ...capture, bytes: new TextEncoder().encode("one") })
      yield* publishCapture({
        ...capture,
        sequence: 2,
        resourceKey: "message/2",
        bytes: new TextEncoder().encode("two"),
      })
      const records = yield* ledger.records
      const first = buildProjection(records, records.length)
      const second = buildProjection(records, records.length)
      const building = buildProjection(records, 1)
      return {
        records,
        first,
        second,
        building,
        digest: yield* projectionDigest(first),
        rebuiltDigest: yield* projectionDigest(second),
      }
    }).pipe(Effect.provide(kernelServices(directory)))
    const result = await Effect.runPromise(programs)
    assertEquals(result.first.coverage.state, "current")
    assertEquals(result.first.coverage.pending, 0)
    assertEquals(result.first.captures.length, 2)
    assertEquals(result.digest, result.rebuiltDigest)
    assertEquals(result.building.coverage.state, "building")
    assertEquals(result.building.coverage.pending, 1)

    const after = await Effect.runPromise(
      Effect.gen(function* () {
        const ledger = yield* Ledger
        const records = yield* ledger.records
        return yield* projectionDigest(buildProjection(records, records.length))
      }).pipe(Effect.provide(kernelServices(directory))),
    )
    assertEquals(after, result.digest)
  })
})

const reference: FixtureReference = {
  id: "append-v1",
  version: "1.0.0",
  canonicalHead: { seq: 1, hash: `sha256:${"a".repeat(64)}` },
  sequenceRange: [1, 2],
}

Deno.test("reconciliation emits an evidence envelope for I-02, I-08, and ZT-09", async () => {
  await withTempDir("reconciliation", async (directory) => {
    const dependencies = Layer.mergeAll(
      fileArtifactStoreLayer(`${directory}/artifacts`),
      fixtureStoreLayer(Deno.cwd()),
      fixedEnvironmentProbeLayer({
        hostFingerprint: `sha256:${"b".repeat(64)}`,
        kernel: "linux-test",
        runtime: "deno@test",
        sandboxPosture: "test",
        brokerIdentity: "test",
      }),
      kernelServices(directory),
    ).pipe(Layer.provide(DenoServices.layer))

    const envelope = await Effect.runPromise(
      runEvidence({
        testId: "T35-RECONCILIATION",
        testClass: "conformance",
        invariantIds: ["I-02", "I-08", "ZT-09"],
        fixture: reference,
        seeds: [509],
        body: (fixture, captureStore) =>
          Effect.gen(function* () {
            yield* captureStore.putArtifactText(`fixture:${fixture.treeHash}`)
            const published = yield* publishCapture({
              ...capture,
              bytes: new TextEncoder().encode(fixture.files["fixture.json"] ?? "missing"),
            })
            const ledger = yield* Ledger
            const verified = yield* ledger.verify
            const captures = yield* readCaptures()
            return {
              seq: published.record.seq,
              head: verified.seq,
              captures: captures.length,
            }
          }),
        metrics: { records: 1 },
      }).pipe(Effect.provide(dependencies)),
    )

    assertEquals(envelope.outcome, "pass")
    assertEquals(envelope.invariantIds, ["I-02", "I-08", "ZT-09"])
    assertEquals(envelope.fixtureVersion, "append-v1@1.0.0")
    assertEquals(envelope.metrics?.records, 1)
  })
})
