import { assertEquals } from "@std/assert"
import { DenoServices } from "@effect/platform-deno"
import { Effect, Layer, Schema } from "effect"
import { fileArtifactStoreLayer } from "../../src/evidence/artifact-store.ts"
import { FixtureStore, fixtureStoreLayer } from "../../src/evidence/fixture-store.ts"
import { withTempDir } from "../support.ts"

const Case = Schema.Struct({
  caseId: Schema.String,
  file: Schema.String,
  origin: Schema.String,
  sequence: Schema.Number,
  expected: Schema.Literals(["accepted", "duplicate", "quarantined"]),
  reason: Schema.optional(Schema.String),
})

const Source = Schema.Struct({
  dataset: Schema.Literal("ingest-v1"),
  version: Schema.Literal("1.0.0"),
  batches: Schema.Array(Case),
})

const fixtureRoot = new URL("../../fixtures/ingest-v1", import.meta.url).pathname

const readSource = (): typeof Source.Type =>
  Schema.decodeUnknownSync(Source)(
    JSON.parse(Deno.readTextFileSync(`${fixtureRoot}/source.json`)),
  )

Deno.test("the sealed ingest-v1 dataset verifies its hashes at load", async () => {
  const snapshot = await withTempDir("ingest-fixture", (directory) =>
    Effect.runPromise(
      Effect.gen(function* () {
        const store = yield* FixtureStore
        return yield* store.load("ingest-v1", "1.0.0")
      }).pipe(
        Effect.provide(
          Layer.mergeAll(
            fixtureStoreLayer(Deno.cwd()),
            fileArtifactStoreLayer(directory),
          ).pipe(Layer.provide(DenoServices.layer)),
        ),
      ),
    ))
  assertEquals(snapshot.id, "ingest-v1")
  assertEquals(snapshot.files["source.json"] !== undefined, true)
  assertEquals(Object.keys(snapshot.files).length > 8, true)
})

Deno.test("ingest-v1 covers six sources plus cluster-digest and phone", () => {
  const source = readSource()
  const origins = new Set(
    source.batches.filter((entry) => entry.expected === "accepted").map((entry) => entry.origin),
  )
  assertEquals(
    [...origins].toSorted(),
    [
      "brave",
      "cluster-digest",
      "finance",
      "git",
      "immich",
      "phone",
      "tuta-calendar",
      "tuta-mail",
    ],
  )
  for (const entry of source.batches) {
    assertEquals(Deno.statSync(`${fixtureRoot}/${entry.file}`).size > 0, true, entry.caseId)
  }
})

Deno.test("ingest-v1 declares its adversarial cases with expected failures", () => {
  const source = readSource()
  const negatives = source.batches.filter((entry) => entry.expected === "quarantined")
  assertEquals(
    negatives.map((entry) => `${entry.caseId}:${entry.reason}`).toSorted(),
    [
      "brave-over-cap:too_many_entries",
      "finance-truncated:decrypt_failed",
      "unknown-origin-1:parse_failed",
    ],
  )
  assertEquals(
    source.batches.filter((entry) => entry.expected === "duplicate").length,
    1,
  )
  assertEquals(
    source.batches.some((entry) => entry.caseId === "tuta-mail-injected-prose"),
    true,
  )
})
