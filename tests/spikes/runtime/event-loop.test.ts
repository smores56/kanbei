import { assert, assertEquals } from "@std/assert"
import { Clock, Effect } from "effect"
import { startLagSampler } from "../../../src/spikes/runtime/event-loop.ts"

Deno.test("lag sampler records a sample per interval and stops when asked", async () => {
  const samples = await Effect.runPromise(
    Effect.gen(function* () {
      const clock = yield* Clock.Clock
      const sampler = startLagSampler(clock, 1)
      yield* Effect.sleep(25)
      return sampler.stop()
    }),
  )
  assert(samples.length >= 3)
  assert(samples.every((value) => Number.isFinite(value)))
  assert(samples.every((value) => value > -50))
})

Deno.test("lag sampler reports a stall when the loop is blocked", async () => {
  const samples = await Effect.runPromise(
    Effect.gen(function* () {
      const clock = yield* Clock.Clock
      const sampler = startLagSampler(clock, 1)
      yield* Effect.sleep(5)
      yield* Effect.sync(() => {
        const until = clock.monotonicTimeNanosUnsafe() + 60_000_000n
        while (clock.monotonicTimeNanosUnsafe() < until) {
          // Block the loop the way a synchronous driver does.
        }
      })
      yield* Effect.sleep(5)
      return sampler.stop()
    }),
  )
  const largest = samples.reduce((max, value) => Math.max(max, value), 0)
  assert(largest >= 20, `expected a stall sample >= 20ms, got ${largest}`)
})

Deno.test("stopping the sampler stops rescheduling", async () => {
  const first = await Effect.runPromise(
    Effect.gen(function* () {
      const clock = yield* Clock.Clock
      const sampler = startLagSampler(clock, 1)
      yield* Effect.sleep(10)
      const samples = sampler.stop()
      yield* Effect.sleep(10)
      return { count: samples.length, after: sampler.stop().length }
    }),
  )
  assertEquals(first.count, first.after)
})
