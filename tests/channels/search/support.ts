import { Clock, Effect, Layer, Ref } from "effect"
import type { SearchConfig } from "../../../src/channels/search/client.ts"
import {
  SearchChannel,
  SearchClient,
  searchClientLayer,
} from "../../../src/channels/search/client.ts"
import type { SearchHit, SearchQuery, SearchReply } from "../../../src/channels/search/schema.ts"
import { SearchTransportError } from "../../../src/channels/search/client.ts"
import { memorySearchBudgetLayer } from "../../../src/channels/search/budget.ts"

export const config: SearchConfig = {
  maxQueryChars: 100,
  maxLimit: 5,
  maxTextChars: 280,
  queriesPerHour: 100,
  queriesPerDay: 1_000,
}

export interface FakeSearch {
  readonly layer: Layer.Layer<SearchChannel>
  readonly calls: Ref.Ref<ReadonlyArray<SearchQuery>>
}

export const fakeSearch = (
  handler: (query: SearchQuery) => Effect.Effect<SearchReply, SearchTransportError>,
): Effect.Effect<FakeSearch> =>
  Effect.gen(function* () {
    const calls = yield* Ref.make<ReadonlyArray<SearchQuery>>([])
    const layer = Layer.succeed(SearchChannel)({
      search: (query) =>
        Ref.update(calls, (current) => [...current, query]).pipe(
          Effect.flatMap(() => handler(query)),
        ),
    })
    return { layer, calls }
  })

export interface TestClock {
  readonly clock: Clock.Clock
  readonly advance: (millis: number) => Effect.Effect<void>
}

/** A clock the test moves explicitly, so rate windows are deterministic. */
export const mutableClock = (start: number): Effect.Effect<TestClock> =>
  Effect.gen(function* () {
    const now = yield* Ref.make(start)
    const millis = Ref.get(now)
    const clock: Clock.Clock = {
      currentTimeMillisUnsafe: () => 0,
      currentTimeMillis: millis,
      currentTimeNanosUnsafe: () => 0n,
      currentTimeNanos: millis.pipe(Effect.map((value) => BigInt(value) * 1_000_000n)),
      monotonicTimeNanosUnsafe: () => 0n,
      monotonicTimeNanos: Effect.succeed(0n),
      sleep: () => Effect.void,
    }
    return {
      clock,
      advance: (millisToAdd) => Ref.update(now, (value) => value + millisToAdd),
    }
  })

export interface Harness {
  readonly clients: Layer.Layer<SearchClient>
  readonly calls: Ref.Ref<ReadonlyArray<SearchQuery>>
}

export const harness = (
  handler: (query: SearchQuery) => Effect.Effect<SearchReply, SearchTransportError>,
  overrides: Partial<SearchConfig> = {},
): Effect.Effect<Harness> =>
  Effect.gen(function* () {
    const search = yield* fakeSearch(handler)
    return {
      clients: searchClientLayer({ ...config, ...overrides }).pipe(
        Layer.provide(Layer.mergeAll(search.layer, memorySearchBudgetLayer)),
      ),
      calls: search.calls,
    }
  })

export const allowed = (hits: ReadonlyArray<SearchHit>): SearchReply => ({
  verdict: "allowed",
  hits,
})

export const blocked = (reason: "dlp" | "judge" | "budget" | "rate" | "schema" | "error") => ({
  verdict: "blocked" as const,
  reason,
})

export const onClient = <A>(
  h: Harness,
  clock: TestClock,
  effect: Effect.Effect<A, never, SearchClient>,
): Promise<A> =>
  Effect.runPromise(
    effect.pipe(
      Effect.provide(h.clients),
      Effect.provideService(Clock.Clock, clock.clock),
      Effect.orDie,
    ),
  )
