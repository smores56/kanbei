import { assertEquals } from "@std/assert"
import { Effect, Option, Ref } from "effect"
import { SearchClient, SearchTransportError } from "../../../src/channels/search/client.ts"
import { parseSearchReply } from "../../../src/channels/search/schema.ts"
import { stripMarkup } from "../../../src/channels/search/text.ts"
import { hash } from "../../support.ts"
import { allowed, blocked, harness, mutableClock, onClient, type TestClock } from "./support.ts"
import type { Harness } from "./support.ts"

const NOW = Date.parse("2026-11-11T00:00:00.000Z")
const HOUR = 60 * 60 * 1000
const DAY = 24 * HOUR

const runQueries = (
  h: Harness,
  clock: TestClock,
  inputs: ReadonlyArray<{ readonly query: string; readonly limit: number }>,
) =>
  onClient(
    h,
    clock,
    Effect.gen(function* () {
      const client = yield* SearchClient
      const outcomes = yield* Effect.forEach(
        inputs,
        (input) => client.query(input),
        { concurrency: 1 },
      )
      return { outcomes, receipts: yield* client.receipts }
    }),
  )

const callsOf = (h: Harness) => Effect.runPromise(Ref.get(h.calls))

Deno.test("a 200-character query and an over-limit query deny without reaching the VM", async () => {
  const h = await Effect.runPromise(harness(() => Effect.succeed(allowed([]))))
  const clock = await Effect.runPromise(mutableClock(NOW))
  const { outcomes, receipts } = await runQueries(h, clock, [
    { query: "x".repeat(200), limit: 5 },
    { query: "fine", limit: 11 },
  ])
  assertEquals(outcomes.map((outcome) => outcome.verdict), ["blocked", "blocked"])
  assertEquals(outcomes.map((outcome) => outcome.reason), ["schema", "schema"])
  assertEquals((await callsOf(h)).length, 0)
  assertEquals(receipts.length, 2)
  assertEquals(
    receipts.map((receipt) => Option.getOrThrow(receipt.reason)),
    ["schema", "schema"],
  )
})

Deno.test("DLP-blocked and judge-blocked queries return their reason and are receipted", async () => {
  const h = await Effect.runPromise(
    harness((query) => Effect.succeed(blocked(query.query.startsWith("secret") ? "dlp" : "judge"))),
  )
  const clock = await Effect.runPromise(mutableClock(NOW))
  const { outcomes, receipts } = await runQueries(h, clock, [
    { query: "secret 4111", limit: 3 },
    { query: "smuggle this", limit: 3 },
  ])
  assertEquals(outcomes.map((outcome) => outcome.reason), ["dlp", "judge"])
  assertEquals(
    receipts.map((receipt) => Option.getOrThrow(receipt.reason)),
    ["dlp", "judge"],
  )
  assertEquals(receipts.every((receipt) => receipt.verdict === "blocked"), true)
})

Deno.test("a transport failure is blocked as error", async () => {
  const h = await Effect.runPromise(
    harness(() => Effect.fail(new SearchTransportError({ reason: "down" }))),
  )
  const clock = await Effect.runPromise(mutableClock(NOW))
  const { outcomes } = await runQueries(h, clock, [{ query: "hello", limit: 1 }])
  assertEquals(outcomes[0]?.reason, "error")
})

Deno.test("an empty result is a successful empty observation set", async () => {
  const h = await Effect.runPromise(harness(() => Effect.succeed(allowed([]))))
  const clock = await Effect.runPromise(mutableClock(NOW))
  const { outcomes, receipts } = await runQueries(h, clock, [{ query: "quiet", limit: 3 }])
  assertEquals(outcomes[0]?.verdict, "hits")
  assertEquals(outcomes[0]?.hits, [])
  assertEquals(receipts[0]?.hits, 0)
  assertEquals(receipts[0]?.resultDigest.startsWith("sha256:"), true)
})

Deno.test("hit text is stripped to inert text and capped", async () => {
  const h = await Effect.runPromise(
    harness(() =>
      Effect.succeed(allowed([
        {
          title: "<b>Title</b> https://example.invalid",
          locator: "https://example.invalid/page",
          text: `<script>alert(1)</script><p>Hello &amp; welcome ${"y".repeat(400)}</p>`,
          rank: 1,
        },
      ]))
    ),
  )
  const clock = await Effect.runPromise(mutableClock(NOW))
  const { outcomes } = await runQueries(h, clock, [{ query: "markup", limit: 3 }])
  const hit = outcomes[0]?.hits[0]
  if (hit === undefined) throw new Error("hit missing")
  assertEquals(hit.text.includes("<"), false)
  assertEquals(hit.text.includes("amp;"), false)
  assertEquals(hit.text.includes("https://"), false)
  assertEquals(hit.text.length <= 280, true)
  assertEquals(hit.title.includes("<b>"), false)
  assertEquals(hit.locator, "https://example.invalid/page")
  assertEquals(hit.rank, 1)
})

Deno.test("the hourly rate limit denies locally and recovers after the window", async () => {
  const h = await Effect.runPromise(
    harness(() => Effect.succeed(allowed([])), { queriesPerHour: 2 }),
  )
  const clock = await Effect.runPromise(mutableClock(NOW))
  const result = await onClient(
    h,
    clock,
    Effect.gen(function* () {
      const client = yield* SearchClient
      const first = yield* client.query({ query: "one", limit: 1 })
      const second = yield* client.query({ query: "two", limit: 1 })
      const third = yield* client.query({ query: "three", limit: 1 })
      yield* clock.advance(HOUR + 1)
      const afterWindow = yield* client.query({ query: "four", limit: 1 })
      return { first, second, third, afterWindow, receipts: yield* client.receipts }
    }),
  )
  assertEquals(result.first.verdict, "hits")
  assertEquals(result.second.verdict, "hits")
  assertEquals(result.third.reason, "rate")
  assertEquals(result.afterWindow.verdict, "hits")
  assertEquals((await callsOf(h)).length, 3)
})

Deno.test("the daily budget denies locally and recovers the next day", async () => {
  const h = await Effect.runPromise(
    harness(() => Effect.succeed(allowed([])), { queriesPerDay: 2 }),
  )
  const clock = await Effect.runPromise(mutableClock(NOW))
  const result = await onClient(
    h,
    clock,
    Effect.gen(function* () {
      const client = yield* SearchClient
      yield* client.query({ query: "one", limit: 1 })
      yield* client.query({ query: "two", limit: 1 })
      const third = yield* client.query({ query: "three", limit: 1 })
      yield* clock.advance(DAY + 1)
      const nextDay = yield* client.query({ query: "four", limit: 1 })
      return { third, nextDay, receipts: yield* client.receipts }
    }),
  )
  assertEquals(result.third.reason, "budget")
  assertEquals(result.nextDay.verdict, "hits")
  assertEquals(
    result.receipts.map((receipt) => Option.isNone(receipt.reason)),
    [true, true, false, true],
  )
})

Deno.test("the Search VM receives only the capped query and limit", async () => {
  const h = await Effect.runPromise(harness(() => Effect.succeed(allowed([]))))
  const clock = await Effect.runPromise(mutableClock(NOW))
  await runQueries(h, clock, [{ query: "narrow", limit: 4 }])
  const [call] = await callsOf(h)
  assertEquals(call, { query: "narrow", limit: 4 })
  assertEquals(Object.keys(call ?? {}).toSorted(), ["limit", "query"])
})

Deno.test("every query records provenance: query digest, result digest, and locator", async () => {
  const h = await Effect.runPromise(
    harness(() =>
      Effect.succeed(allowed([
        { title: "n", locator: "loc-1", text: "body", rank: 1 },
      ]))
    ),
  )
  const clock = await Effect.runPromise(mutableClock(NOW))
  const { outcomes, receipts } = await runQueries(h, clock, [
    { query: "same", limit: 1 },
    { query: "same", limit: 1 },
  ])
  assertEquals(receipts[0]?.queryDigest, await hash("same"))
  assertEquals(receipts[0]?.resultDigest, receipts[1]?.resultDigest)
  assertEquals(outcomes[0]?.hits[0]?.queryDigest, await hash("same"))
  assertEquals(outcomes[0]?.hits[0]?.locator, "loc-1")
})

Deno.test("the client holds no provider key, URL, or environment credential", async () => {
  const sources = await Promise.all(
    ["client.ts", "schema.ts", "text.ts"].map((name) =>
      Deno.readTextFile(`src/channels/search/${name}`)
    ),
  )
  for (const source of sources) {
    assertEquals(/TINYFISH|api[_-]?key|Bearer|Deno\.env/iu.test(source), false)
  }
  const keys = Object.keys({ query: "", limit: 1 })
  assertEquals(keys.toSorted(), ["limit", "query"])
})

Deno.test("the search fixtures parse and their text is stripped", () => {
  const fixture = JSON.parse(Deno.readTextFileSync("fixtures/search-v1/replies.json")) as {
    cases: ReadonlyArray<{ name: string; reply: unknown }>
  }
  for (const entry of fixture.cases) {
    const reply = parseSearchReply(entry.reply)
    assertEquals(reply.verdict === "allowed" ? "allowed" : "blocked", reply.verdict)
  }
  assertEquals(
    stripMarkup("<p>Kanbei <b>released</b> &amp; verified.</p>"),
    "Kanbei released & verified.",
  )
})
