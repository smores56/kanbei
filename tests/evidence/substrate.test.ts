import { assertEquals, assertRejects } from "@std/assert"
import { Effect } from "effect"
import {
  makeSyntheticNetwork,
  SyntheticNetworkError,
} from "../../src/evidence/synthetic-network.ts"
import { makeVirtualClock } from "../../src/evidence/virtual-clock.ts"

Deno.test("virtual clock advances and sets deterministic time", async () => {
  const result = await Effect.runPromise(
    Effect.gen(function* () {
      const clock = yield* makeVirtualClock(100)
      const start = yield* clock.nowMillis
      yield* clock.advanceMillis(25)
      const advanced = yield* clock.nowMillis
      yield* clock.setMillis(5)
      const reset = yield* clock.nowMillis
      return { start, advanced, reset }
    }),
  )
  assertEquals(result, { start: 100, advanced: 125, reset: 5 })
})

Deno.test("synthetic network serves recorded responses and records calls", async () => {
  const result = await Effect.runPromise(
    Effect.gen(function* () {
      const network = yield* makeSyntheticNetwork()
      yield* network.serve("https://example.test/data", {
        status: 200,
        headers: { "content-type": "application/json" },
        body: '{"ok":true}',
      })
      const response = yield* network.request({
        method: "GET",
        url: "https://example.test/data",
      })
      const calls = yield* network.calls
      return { response, calls }
    }),
  )

  assertEquals(result.response.status, 200)
  assertEquals(result.calls.length, 1)
  assertEquals(result.calls[0]?.request.method, "GET")
})

Deno.test("synthetic network fails closed on an unknown route", async () => {
  await assertRejects(
    () =>
      Effect.runPromise(
        Effect.gen(function* () {
          const network = yield* makeSyntheticNetwork()
          return yield* network.request({ method: "GET", url: "https://missing.test" })
        }),
      ),
    SyntheticNetworkError,
  )
})
