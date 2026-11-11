import { assert, assertEquals } from "@std/assert"
import { Effect, Layer, Option } from "effect"
import type { InboxConfig, InboxLimits, InboxReader } from "../../../src/channels/inbox/reader.ts"
import { InboxReader as InboxReaderTag } from "../../../src/channels/inbox/reader.ts"
import { BlobSink } from "../../../src/kernel/blob-sink.ts"
import { ContentStore } from "../../../src/kernel/cas.ts"
import { Ledger } from "../../../src/kernel/ledger.ts"
import {
  buildBatch,
  buildBatchFromFixture,
  inboxTestLayers,
  makeKey,
  testLimits,
  writeBatch,
} from "./support.ts"
import { hash, withTempDir } from "../../support.ts"
import { MAX_BATCH_BYTES } from "../../../src/channels/inbox/wire.ts"

const limits: InboxLimits = { ...testLimits, maxBatchesPerPoll: 10 }

interface Scenario {
  readonly directory: string
  readonly identityPath: string
  readonly recipient: string
  readonly layer: Layer.Layer<InboxReader | BlobSink | ContentStore | Ledger>
}

const scenario = <A>(
  run: (context: Scenario) => Promise<A>,
  overrides: Partial<InboxConfig> = {},
  fixedKey?: { readonly identityPath: string; readonly recipient: string },
): Promise<A> =>
  withTempDir("inbox", async (directory) => {
    const key = fixedKey ?? await makeKey(directory)
    const config: InboxConfig = {
      identityPath: key.identityPath,
      allowedOrigins: ["tuta-mail", "git"],
      limits,
      freshnessWindowMs: 3_600_000,
      ...overrides,
    }
    const layers = inboxTestLayers(directory, config)
    return run({
      directory,
      identityPath: key.identityPath,
      recipient: key.recipient,
      layer: layers.layer,
    })
  })

/** One build of the layer for the whole scenario: state must survive between calls. */
const withReader = <A, E>(
  context: Scenario,
  effect: Effect.Effect<A, E, InboxReader | BlobSink | ContentStore | Ledger>,
): Promise<A> => Effect.runPromise(effect.pipe(Effect.provide(context.layer), Effect.orDie))

const pollOnce = Effect.gen(function* () {
  const reader = yield* InboxReaderTag
  yield* reader.poll
  return yield* reader.state
})

const write = (
  directory: string,
  recipient: string,
  name: string,
  plaintext: string,
): Promise<void> => writeBatch(`${directory}/${name}`, plaintext, recipient)

Deno.test("a valid batch publishes blobs, observations, one coverage receipt, and a heartbeat", async () => {
  await scenario(async (context) => {
    const body = '{"subject":"hello"}'
    const digest = await hash(body)
    const fixture = new URL("../../../fixtures/inbox-v1", import.meta.url).pathname
    await write(
      context.directory,
      context.recipient,
      "batch-1.age",
      await buildBatchFromFixture(fixture, context.recipient),
    )

    const outcome = await withReader(
      context,
      Effect.gen(function* () {
        const reader = yield* InboxReaderTag
        yield* reader.poll
        const state = yield* reader.state
        const heartbeat = yield* reader.heartbeat("tuta-mail")
        const sink = yield* BlobSink
        return { state, heartbeat, stored: yield* sink.read(digest) }
      }),
    )
    assertEquals(outcome.state.observations.length, 1)
    assertEquals(outcome.state.observations[0]?.digest, digest)
    assertEquals(outcome.state.observations[0]?.resourceKey, "message/1")
    assertEquals(outcome.state.observations[0]?.bytes, body.length)
    assertEquals(outcome.state.receipts.length, 1)
    assertEquals(outcome.state.receipts[0]?.sequence, 1)
    assertEquals(outcome.state.receipts[0]?.entries, 1)
    assertEquals(outcome.state.quarantined.length, 0)
    assert(Option.isSome(outcome.heartbeat))
    if (Option.isSome(outcome.heartbeat)) {
      assertEquals(outcome.heartbeat.value.sequence, 1)
      assertEquals(outcome.heartbeat.value.state, "current")
      assertEquals(outcome.heartbeat.value.coverage.complete, true)
    }
    assert(Option.isSome(outcome.stored))
  })
})

Deno.test("a second file carrying the same batch is a duplicate, and neither file is re-read", async () => {
  await scenario(async (context) => {
    const plaintext = await buildBatch({ recipient: context.recipient })
    await write(context.directory, context.recipient, "batch-1.age", plaintext)
    await write(context.directory, context.recipient, "batch-2.age", plaintext)
    const outcome = await withReader(
      context,
      Effect.gen(function* () {
        const reader = yield* InboxReaderTag
        yield* reader.poll
        const first = yield* reader.state
        yield* reader.poll
        const second = yield* reader.state
        return { first, second }
      }),
    )
    assertEquals(outcome.first.observations.length, 1)
    assertEquals(outcome.first.receipts.length, 1)
    assertEquals(outcome.first.duplicates, 1)
    assertEquals(outcome.second.observations.length, 1)
    assertEquals(outcome.second.receipts.length, 1)
    assertEquals(outcome.second.duplicates, 1)
  })
})
Deno.test("a duplicate item inside one batch is deduplicated by item hash", async () => {
  await scenario(async (context) => {
    const body = '{"subject":"same"}'
    await write(
      context.directory,
      context.recipient,
      "batch-1.age",
      await buildBatch({
        recipient: context.recipient,
        bodies: [{ key: "message/1", text: body }, { key: "message/2", text: body }],
      }),
    )
    const state = await withReader(context, pollOnce)
    assertEquals(state.observations.length, 1)
    assertEquals(state.duplicates, 1)
    assertEquals(state.receipts[0]?.entries, 1)
  })
})

Deno.test("an empty accepted batch is current, while a source with no heartbeat is unknown", async () => {
  await scenario(async (context) => {
    await write(
      context.directory,
      context.recipient,
      "batch-1.age",
      await buildBatch({
        recipient: context.recipient,
        bodies: [],
      }),
    )
    const outcome = await withReader(
      context,
      Effect.gen(function* () {
        const reader = yield* InboxReaderTag
        yield* reader.poll
        const state = yield* reader.state
        return {
          state,
          current: yield* reader.sourceState("tuta-mail"),
          unknown: yield* reader.sourceState("git"),
          missing: yield* reader.heartbeat("git"),
        }
      }),
    )
    assertEquals(outcome.state.observations.length, 0)
    assertEquals(outcome.state.receipts.length, 1)
    assertEquals(outcome.current, "current")
    assertEquals(outcome.unknown, "unknown")
    assert(Option.isNone(outcome.missing))
  })
})

Deno.test("a gap and an out-of-order sequence are recorded, never silently skipped", async () => {
  await scenario(async (context) => {
    const second = await buildBatch({ recipient: context.recipient, sequence: 2 })
    await write(
      context.directory,
      context.recipient,
      "batch-3.age",
      await buildBatch({ recipient: context.recipient, sequence: 3 }),
    )
    const outcome = await withReader(
      context,
      Effect.gen(function* () {
        const reader = yield* InboxReaderTag
        yield* reader.poll
        const afterGap = yield* reader.state
        yield* Effect.promise(() =>
          write(context.directory, context.recipient, "batch-2.age", second)
        )
        yield* reader.poll
        return { afterGap, outOfOrder: yield* reader.state }
      }),
    )
    assertEquals(outcome.afterGap.gaps.map((gap) => `${gap.kind}:${gap.missing}`), ["gap:2"])
    assertEquals(outcome.afterGap.receipts[0]?.sequence, 3)
    assertEquals(outcome.outOfOrder.gaps.map((gap) => gap.kind), ["gap", "out_of_order"])
    assertEquals(outcome.outOfOrder.receipts.length, 2)
  })
})

Deno.test("suppressions are visible in the heartbeat coverage", async () => {
  await scenario(async (context) => {
    await write(
      context.directory,
      context.recipient,
      "batch-1.age",
      await buildBatch({
        recipient: context.recipient,
        suppressed: 2,
        coverage: { complete: false, gaps: 1, suppressed: 2 },
      }),
    )
    const state = await withReader(context, pollOnce)
    const heartbeat = state.heartbeats.find((entry) => entry.origin === "tuta-mail")
    assertEquals(heartbeat?.coverage.suppressed, 2)
    assertEquals(heartbeat?.coverage.gaps, 1)
    assertEquals(state.receipts[0]?.coverage.complete, false)
  })
})

Deno.test("a planted credential is rejected as a security event with no observation", async () => {
  await scenario(async (context) => {
    await write(
      context.directory,
      context.recipient,
      "batch-1.age",
      await buildBatch({
        recipient: context.recipient,
        bodies: [{ key: "message/1", text: '{"api_key":"sk-abcdefghijklmnopqrstu"}' }],
      }),
    )
    const state = await withReader(context, pollOnce)
    assertEquals(state.observations.length, 0)
    assertEquals(state.receipts.length, 0)
    assertEquals(state.quarantined.map((entry) => entry.reason), ["credential_detected"])
    assertEquals(state.securityEvents.map((event) => event.kind), ["credential_detected"])
  })
})

Deno.test("an unknown field, wrong version, or unlisted origin is quarantined", async () => {
  await scenario(async (context) => {
    await write(
      context.directory,
      context.recipient,
      "unknown-field.age",
      await buildBatch({ recipient: context.recipient, extraEnvelope: { unexpected: true } }),
    )
    await write(
      context.directory,
      context.recipient,
      "wrong-version.age",
      await buildBatch({ recipient: context.recipient, envelopeVersion: 2 }),
    )
    await write(
      context.directory,
      context.recipient,
      "unlisted-origin.age",
      await buildBatch({ recipient: context.recipient, origin: "immich" }),
    )
    const state = await withReader(context, pollOnce)
    assertEquals(state.receipts.length, 0)
    assertEquals(
      state.quarantined.map((entry) => entry.reason).toSorted(),
      ["parse_failed", "parse_failed", "source_not_allowed"],
    )
  })
})

Deno.test("a decrypt failure is quarantined and leaves no partial observation", async () => {
  await scenario(async (context) => {
    await Deno.writeTextFile(`${context.directory}/broken.age`, "this is not age ciphertext")
    const state = await withReader(context, pollOnce)
    assertEquals(state.quarantined.map((entry) => entry.reason), ["decrypt_failed"])
    assertEquals(state.observations.length, 0)
    assertEquals(state.receipts.length, 0)
  })
})

Deno.test("entry count, entry size, and batch digest caps are enforced", async () => {
  await scenario(
    async (context) => {
      await write(
        context.directory,
        context.recipient,
        "too-many.age",
        await buildBatch({
          recipient: context.recipient,
          bodies: [{ key: "a", text: "a" }, { key: "b", text: "b" }],
        }),
      )
      await write(
        context.directory,
        context.recipient,
        "too-big.age",
        await buildBatch({
          recipient: context.recipient,
          bodies: [{ key: "a", text: "x".repeat(64) }],
        }),
      )
      await write(
        context.directory,
        context.recipient,
        "bad-digest.age",
        await buildBatch({
          recipient: context.recipient,
          batchDigestOverride: `sha256:${"0".repeat(64)}`,
        }),
      )
      const state = await withReader(context, pollOnce)
      assertEquals(
        state.quarantined.map((entry) => entry.reason).toSorted(),
        ["digest_mismatch", "entry_too_large", "too_many_entries"],
      )
    },
    { limits: { ...limits, maxEntriesPerBatch: 1, maxEntryBytes: 32 } },
  )
})

Deno.test("a source past its freshness window reports stale", async () => {
  await scenario(
    async (context) => {
      await write(
        context.directory,
        context.recipient,
        "batch-1.age",
        await buildBatch({
          recipient: context.recipient,
        }),
      )
      const outcome = await withReader(
        context,
        Effect.gen(function* () {
          const reader = yield* InboxReaderTag
          yield* reader.poll
          yield* Effect.sleep("20 millis")
          return {
            state: yield* reader.sourceState("tuta-mail"),
            heartbeat: yield* reader.heartbeat("tuta-mail"),
          }
        }),
      )
      assertEquals(outcome.state, "stale")
      assert(Option.isSome(outcome.heartbeat))
      if (Option.isSome(outcome.heartbeat)) assertEquals(outcome.heartbeat.value.state, "stale")
    },
    { freshnessWindowMs: 1 },
  )
})

Deno.test("the sealed host wire envelope parses as observations without a reader-specific schema", async () => {
  await scenario(
    async (context) => {
      const fixture = new URL("../../../fixtures/inbox-v1/wire", import.meta.url).pathname
      await Deno.copyFile(`${fixture}/host-batch-1.age`, `${context.directory}/host-batch-1.age`)
      const outcome = await withReader(
        context,
        Effect.gen(function* () {
          const reader = yield* InboxReaderTag
          yield* reader.poll
          return yield* reader.state
        }),
      )
      assertEquals(outcome.quarantined, [])
      assertEquals(outcome.observations.length, 2)
      assertEquals(outcome.observations[0]?.origin, "tuta-mail")
      assertEquals(outcome.observations[0]?.sequence, 7)
      assertEquals(outcome.observations[0]?.resourceKey, "message-7")
      assertEquals(outcome.receipts[0]?.entries, 2)
      assertEquals(outcome.receipts[0]?.coverage.complete, true)
    },
    {},
    {
      identityPath:
        new URL("../../../fixtures/ingest-v1/keys/identity.txt", import.meta.url).pathname,
      recipient: "age1gyzseyvd0cumhe3svg3ey6x54lwfhmqvp2wf9dre8d0sycs5e3qscwpgqa",
    },
  )
})

Deno.test("handled files and the last sequence survive a restart without replay", async () => {
  await scenario(async (context) => {
    await write(
      context.directory,
      context.recipient,
      "batch-1.age",
      await buildBatch({ recipient: context.recipient }),
    )
    const first = await withReader(context, pollOnce)
    assertEquals(first.observations.length, 1)
    const second = await withReader(context, pollOnce)
    assertEquals(second.observations.length, 0)
    assertEquals(second.receipts.length, 0)
    const checkpoint = JSON.parse(
      await Deno.readTextFile(`${context.directory}/checkpoint.json`),
    ) as { handled: ReadonlyArray<string>; lastSequence: Readonly<Record<string, number>> }
    assertEquals(checkpoint.handled, ["batch-1.age"])
    assertEquals(checkpoint.lastSequence["tuta-mail"], 1)
  })
})

Deno.test("a poll window selects the next unprocessed files, not the first N names", async () => {
  await scenario(
    async (context) => {
      const names = ["a.age", "b.age", "c.age"]
      const plaintexts = await Promise.all(
        names.map((name) =>
          buildBatch({ recipient: context.recipient, sequence: name.charCodeAt(0) - 96 })
        ),
      )
      await Promise.all(
        names.map((name, index) =>
          write(context.directory, context.recipient, name, plaintexts[index] ?? "")
        ),
      )
      const outcome = await withReader(
        context,
        Effect.gen(function* () {
          const reader = yield* InboxReaderTag
          yield* reader.poll
          yield* reader.poll
          yield* reader.poll
          return yield* reader.state
        }),
      )
      assertEquals(outcome.receipts.length, 3)
      assertEquals(outcome.observations.length, 3)
    },
    { limits: { ...limits, maxBatchesPerPoll: 1 } },
  )
})

Deno.test("the inbox cap is the host's 1 MiB frame cap", () => {
  assertEquals(MAX_BATCH_BYTES, 1_048_576)
})
