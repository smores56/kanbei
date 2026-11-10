import { assert, assertAlmostEquals, assertEquals } from "@std/assert"
import {
  elapsedMs,
  percentile,
  summarize,
  toMilliseconds,
} from "../../../src/spikes/runtime/metrics.ts"

Deno.test("percentile interpolates linearly between sample positions", () => {
  const sorted = [1, 2, 3, 4, 5]
  assertEquals(percentile(sorted, 0), 1)
  assertEquals(percentile(sorted, 0.5), 3)
  assertEquals(percentile(sorted, 1), 5)
  assertAlmostEquals(percentile(sorted, 0.25), 2)
  assertAlmostEquals(percentile(sorted, 0.95), 4.8, 1e-9)
})

Deno.test("summarize reports count, min, max, mean, p50 and p95", () => {
  const summary = summarize([4, 1, 3, 2])
  assertEquals(summary.count, 4)
  assertEquals(summary.min, 1)
  assertEquals(summary.max, 4)
  assertEquals(summary.mean, 2.5)
  assertEquals(summary.p50, 2.5)
  assertEquals(summary.p95, 3.85)
})

Deno.test("summarize of an empty sample is all zeroes and does not divide by zero", () => {
  assertEquals(summarize([]), { count: 0, min: 0, max: 0, mean: 0, p50: 0, p95: 0 })
})

Deno.test("elapsedMs converts monotonic nanoseconds to milliseconds", () => {
  assertAlmostEquals(elapsedMs(1_000_000n, 3_500_000n), 2.5, 1e-9)
})

Deno.test("toMilliseconds converts bytes to mebibytes", () => {
  assertAlmostEquals(toMilliseconds(1_048_576), 1, 1e-9)
  assert(toMilliseconds(0) === 0)
})
