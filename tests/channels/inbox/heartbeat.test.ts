import { assert, assertEquals } from "@std/assert"
import { Clock, Effect } from "effect"
import type { InboxConfig, InboxReader } from "../../../src/channels/inbox/reader.ts"
import type { ContentStore } from "../../../src/kernel/cas.ts"
import type { Ledger } from "../../../src/kernel/ledger.ts"
import { InboxReader as InboxReaderTag } from "../../../src/channels/inbox/reader.ts"
import { buildBatch, encryptFile, inboxTestLayers, testLimits } from "./support.ts"
import { withTempDir } from "../../support.ts"

const fixtureRoot = new URL("../../../fixtures/ingest-v1", import.meta.url).pathname
const identityPath = `${fixtureRoot}/keys/identity.txt`
const recipient = "age1gyzseyvd0cumhe3svg3ey6x54lwfhmqvp2wf9dre8d0sycs5e3qscwpgqa"
const wireRoot = new URL("../../../fixtures/inbox-v1/wire", import.meta.url).pathname

const NOW = Date.parse("2026-11-11T00:00:00.000Z")
const allowed = ["tuta-mail", "finance", "cluster-digest"] as const

const clockAt = (millis: number): Clock.Clock => ({
  currentTimeMillisUnsafe: () => millis,
  currentTimeMillis: Effect.succeed(millis),
  currentTimeNanosUnsafe: () => BigInt(millis) * 1_000_000n,
  currentTimeNanos: Effect.succeed(BigInt(millis) * 1_000_000n),
  monotonicTimeNanosUnsafe: () => 0n,
  monotonicTimeNanos: Effect.succeed(0n),
  sleep: () => Effect.void,
})

const config: InboxConfig = {
  identityPath,
  allowedOrigins: [...allowed],
  limits: { ...testLimits, maxBatchesPerPoll: 10 },
  freshnessWindowMs: 3_600_000,
}

const pollAll = Effect.gen(function* () {
  const reader = yield* InboxReaderTag
  const poll = yield* reader.poll
  const state = yield* reader.state
  const mail = yield* reader.sourceState("tuta-mail")
  const finance = yield* reader.sourceState("finance")
  const digest = yield* reader.sourceState("cluster-digest")
  return { poll, state, mail, finance, digest }
})

const run = <A, E>(
  directory: string,
  effect: Effect.Effect<A, E, InboxReader | ContentStore | Ledger>,
): Promise<A> =>
  Effect.runPromise(
    effect.pipe(
      Effect.provide(inboxTestLayers(directory, config).layer),
      Effect.provideService(Clock.Clock, clockAt(NOW)),
      Effect.orDie,
    ),
  )

const heartbeatJson = (sources: Record<string, unknown>, hostSources: string[] = []): string =>
  JSON.stringify({
    v: 1,
    kind: "heartbeat",
    generated_at: "2026-11-11T00:00:00Z",
    sources,
    host_sources: hostSources,
  })

const source = (stale: boolean, windowSeconds: number, lastOk: string | null) => ({
  stale,
  host: false,
  window_s: windowSeconds,
  last_ok: lastOk,
  age_s: lastOk === null ? null : 300,
})

Deno.test("the sealed host heartbeat fixture feeds freshness per source", async () => {
  await withTempDir("heartbeat", async (directory) => {
    await Deno.copyFile(
      `${wireRoot}/host-heartbeat-1.age`,
      `${directory}/heartbeat-1.age`,
    )
    const outcome = await run(directory, pollAll)
    assertEquals(outcome.state.quarantined, [])
    assertEquals(outcome.poll.accepted, 1)
    assertEquals(outcome.state.heartbeats.length, 3)
    assertEquals(outcome.state.receipts.length, 0)
    assertEquals(outcome.state.observations.length, 0)
    // The host's own verdict is final: finance is stale, the others current.
    assertEquals(outcome.mail, "current")
    assertEquals(outcome.finance, "stale")
    assertEquals(outcome.digest, "current")
    const finance = outcome.state.heartbeats.find((entry) => entry.origin === "finance")
    assertEquals(finance?.kind, "host")
    assertEquals(finance?.windowMs, 86_400_000)
    assert(finance?.lastOk === "2026-11-09T00:00:00Z")
  })
})

Deno.test("a duplicate heartbeat is deduped and stays durable across a restart", async () => {
  await withTempDir("heartbeat-duplicate", async (directory) => {
    const plaintext = heartbeatJson({
      "tuta-mail": source(false, 900, "2026-11-10T23:55:00Z"),
    })
    await encryptFile(plaintext, recipient, `${directory}/heartbeat-1.age`)
    await Deno.copyFile(`${directory}/heartbeat-1.age`, `${directory}/heartbeat-1-copy.age`)
    const first = await run(directory, pollAll)
    assertEquals(first.poll.accepted, 1)
    assertEquals(first.poll.duplicates, 1)
    assertEquals(first.state.heartbeats.length, 1)
    const restarted = await run(directory, pollAll)
    assertEquals(restarted.state.heartbeats.length, 1)
    assertEquals(restarted.state.heartbeats[0]?.origin, "tuta-mail")
    assertEquals(restarted.mail, "current")
  })
})

Deno.test("a heartbeat whose host verdict is stale never presents the source current", async () => {
  await withTempDir("heartbeat-stale", async (directory) => {
    await encryptFile(
      heartbeatJson({
        "tuta-mail": source(true, 900, "2026-11-09T00:00:00Z"),
        finance: source(false, 900, null),
      }),
      recipient,
      `${directory}/heartbeat-1.age`,
    )
    const outcome = await run(directory, pollAll)
    assertEquals(outcome.mail, "stale")
    // No producer time at all is unknown, never current.
    assertEquals(outcome.finance, "unknown")
  })
})

Deno.test("an unknown source in a heartbeat is ignored, not fatal to the envelope", async () => {
  await withTempDir("heartbeat-unknown", async (directory) => {
    await encryptFile(
      heartbeatJson({
        "tuta-mail": source(false, 900, "2026-11-10T23:55:00Z"),
        exfil: source(false, 900, "2026-11-10T23:55:00Z"),
      }),
      recipient,
      `${directory}/heartbeat-1.age`,
    )
    const outcome = await run(directory, pollAll)
    // One unknown key must not cost the fresher sources their verdict.
    assertEquals(outcome.state.quarantined, [])
    assertEquals(outcome.poll.accepted, 1)
    assertEquals(outcome.state.heartbeats.map((entry) => entry.origin), ["tuta-mail"])
    assertEquals(outcome.mail, "current")
  })
})

Deno.test("a heartbeat made only of unknown sources is accepted as a no-op", async () => {
  await withTempDir("heartbeat-all-unknown", async (directory) => {
    await encryptFile(
      heartbeatJson({ search: source(false, 900, "2026-11-10T23:55:00Z") }),
      recipient,
      `${directory}/heartbeat-1.age`,
    )
    const outcome = await run(directory, pollAll)
    assertEquals(outcome.state.quarantined, [])
    assertEquals(outcome.poll.accepted, 1)
    assertEquals(outcome.state.heartbeats, [])
    assertEquals(outcome.mail, "unknown")
  })
})

Deno.test("an unparseable producer time is unknown, not current", async () => {
  await withTempDir("produced-at", async (directory) => {
    // A rich manifest with a garbage producer time.
    await encryptFile(
      await buildBatch({
        recipient,
        extraManifest: { producedAt: "not-a-timestamp" },
      }),
      recipient,
      `${directory}/batch-1.age`,
    )
    // A host envelope with a garbage `sent_at` (the same value at the wire layer).
    await encryptFile(
      JSON.stringify({
        v: 1,
        source: "finance",
        kind: "batch",
        seq: 1,
        sent_at: "yesterday-ish",
        items: [{ id: "f-1", amount: 1 }],
      }),
      recipient,
      `${directory}/batch-2.age`,
    )
    const outcome = await run(directory, pollAll)
    assertEquals(outcome.state.quarantined, [])
    assertEquals(outcome.mail, "unknown")
    assertEquals(outcome.finance, "unknown")
  })
})

Deno.test("host-legal seq=0 is accepted and does not invent a gap", async () => {
  await withTempDir("seq-zero", async (directory) => {
    await encryptFile(
      JSON.stringify({
        v: 1,
        source: "tuta-mail",
        kind: "batch",
        seq: 0,
        sent_at: "2026-11-11T00:00:00Z",
        items: [{ id: "m-0", subject: "first" }],
      }),
      recipient,
      `${directory}/batch-0.age`,
    )
    const outcome = await run(directory, pollAll)
    assertEquals(outcome.state.quarantined, [])
    assertEquals(outcome.state.observations.length, 1)
    assertEquals(outcome.state.observations[0]?.sequence, 0)
    assertEquals(outcome.state.gaps, [])
    assertEquals(outcome.mail, "current")
  })
})
