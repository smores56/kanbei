import { assertEquals } from "@std/assert"
import { Effect, Layer, Ref } from "effect"
import { searchBudgetFileLayer } from "../../../src/adapters/search-budget-file.ts"
import { SearchBudgetStore } from "../../../src/channels/search/budget.ts"
import {
  SearchChannel,
  SearchClient,
  searchClientLayer,
} from "../../../src/channels/search/client.ts"
import type { SearchConfig } from "../../../src/channels/search/client.ts"
import type { SearchQuery, SearchReply } from "../../../src/channels/search/schema.ts"
import { mutableClock, onClient } from "./support.ts"
import { withTempDir } from "../../support.ts"

const start = Date.parse("2026-11-11T00:00:00.000Z")

const config: SearchConfig = {
  maxQueryChars: 100,
  maxLimit: 5,
  maxTextChars: 280,
  queriesPerHour: 100,
  queriesPerDay: 1_000,
}

const allowed = (): SearchReply => ({
  verdict: "allowed",
  hits: [{ title: "hit", locator: "https://example.invalid/h", text: "ok", rank: 0 }],
})

const channelLayer = (calls: Ref.Ref<ReadonlyArray<SearchQuery>>): Layer.Layer<SearchChannel> =>
  Layer.succeed(SearchChannel)({
    search: (query) =>
      Ref.update(calls, (current) => [...current, query]).pipe(Effect.as(allowed())),
  })

const clientOver = (path: string, overrides: Partial<SearchConfig>) =>
  Effect.gen(function* () {
    const calls = yield* Ref.make<ReadonlyArray<SearchQuery>>([])
    return {
      clients: searchClientLayer({ ...config, ...overrides }).pipe(
        Layer.provide(
          Layer.mergeAll(channelLayer(calls), searchBudgetFileLayer(path)),
        ),
      ),
      calls,
    }
  })

Deno.test("a restart cannot reset the daily search ceiling", async () => {
  await withTempDir("search-budget", async (directory) => {
    const path = `${directory}/budget.json`
    const clock = await Effect.runPromise(mutableClock(start))
    const first = await Effect.runPromise(clientOver(path, { queriesPerDay: 1 }))
    const before = await onClient(
      first,
      clock,
      Effect.gen(function* () {
        const client = yield* SearchClient
        return yield* client.query({ query: "water", limit: 1 })
      }),
    )
    assertEquals(before.verdict, "hits")

    // A new process loop over the same file must not see an empty window.
    const second = await Effect.runPromise(clientOver(path, { queriesPerDay: 1 }))
    const after = await onClient(
      second,
      clock,
      Effect.gen(function* () {
        const client = yield* SearchClient
        return yield* client.query({ query: "filter", limit: 1 })
      }),
    )
    assertEquals(after.verdict, "blocked")
    assertEquals(after.reason, "budget")
    const calls = await Effect.runPromise(Ref.get(second.calls))
    assertEquals(calls.length, 0)

    const stored = JSON.parse(await Deno.readTextFile(path)) as { admittedAtMs: number[] }
    assertEquals(stored.admittedAtMs.length, 1)
  })
})

Deno.test("a concurrent burst cannot exceed the hourly ceiling", async () => {
  await withTempDir("search-burst", async (directory) => {
    const path = `${directory}/budget.json`
    const clock = await Effect.runPromise(mutableClock(start))
    const harness = await Effect.runPromise(clientOver(path, { queriesPerHour: 3 }))
    const outcomes = await onClient(
      harness,
      clock,
      Effect.gen(function* () {
        const client = yield* SearchClient
        return yield* Effect.forEach(
          Array.from({ length: 10 }, (_, index) => `query-${index}`),
          (query) => client.query({ query, limit: 1 }),
          { concurrency: 10 },
        )
      }),
    )
    assertEquals(outcomes.filter((outcome) => outcome.verdict === "hits").length, 3)
    assertEquals(outcomes.filter((outcome) => outcome.reason === "rate").length, 7)
    const calls = await Effect.runPromise(Ref.get(harness.calls))
    assertEquals(calls.length, 3)
  })
})

Deno.test("the hourly window reopens after an hour and the daily window after a day", async () => {
  await withTempDir("search-windows", async (directory) => {
    const path = `${directory}/budget.json`
    const clock = await Effect.runPromise(mutableClock(start))
    const harness = await Effect.runPromise(
      clientOver(path, { queriesPerHour: 1, queriesPerDay: 2 }),
    )
    const outcomes = await onClient(
      harness,
      clock,
      Effect.gen(function* () {
        const client = yield* SearchClient
        const first = yield* client.query({ query: "water", limit: 1 })
        const blocked = yield* client.query({ query: "filter", limit: 1 })
        yield* clock.advance(60 * 60 * 1000)
        const nextHour = yield* client.query({ query: "cartridge", limit: 1 })
        yield* clock.advance(60 * 60 * 1000)
        const blockedDaily = yield* client.query({ query: "replacement", limit: 1 })
        yield* clock.advance(24 * 60 * 60 * 1000)
        const nextDay = yield* client.query({ query: "repeat", limit: 1 })
        return [
          first.verdict,
          blocked.reason,
          nextHour.verdict,
          blockedDaily.reason,
          nextDay.verdict,
        ]
      }),
    )
    assertEquals(outcomes, ["hits", "rate", "hits", "budget", "hits"])
  })
})

Deno.test("a corrupt budget file fails closed instead of resetting", async () => {
  await withTempDir("search-corrupt", async (directory) => {
    const path = `${directory}/budget.json`
    await Deno.writeTextFile(path, "{not json")
    const clock = await Effect.runPromise(mutableClock(start))
    const harness = await Effect.runPromise(clientOver(path, {}))
    const outcome = await onClient(
      harness,
      clock,
      Effect.gen(function* () {
        const client = yield* SearchClient
        return yield* client.query({ query: "water", limit: 1 })
      }),
    )
    assertEquals(outcome.verdict, "blocked")
    assertEquals(outcome.reason, "error")
    const calls = await Effect.runPromise(Ref.get(harness.calls))
    assertEquals(calls.length, 0)
  })
})

Deno.test("the file store persists and reloads admit stamps", async () => {
  await withTempDir("search-store", async (directory) => {
    const path = `${directory}/budget.json`
    const store = await Effect.runPromise(
      Effect.gen(function* () {
        const budget = yield* SearchBudgetStore
        yield* budget.save([1, 2, 3])
        return budget
      }).pipe(Effect.provide(searchBudgetFileLayer(path))),
    )
    const loaded = await Effect.runPromise(
      Effect.gen(function* () {
        const budget = yield* SearchBudgetStore
        return yield* budget.load
      }).pipe(Effect.provide(searchBudgetFileLayer(path))),
    )
    assertEquals(loaded, [1, 2, 3])
    assertEquals((await Effect.runPromise(store.load)).length, 3)
  })
})
