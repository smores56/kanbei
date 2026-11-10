import { assertEquals } from "@std/assert"
import { mean, percentile, round, summarize } from "../../../src/spikes/isolation/stats.ts"

Deno.test("percentile uses nearest rank on an ascending array", () => {
  assertEquals(percentile([], 95), 0)
  assertEquals(percentile([1], 95), 1)
  assertEquals(percentile([1, 2, 3, 4, 5], 50), 3)
  assertEquals(percentile([1, 2, 3, 4, 5], 95), 5)
})

Deno.test("summarize reports p50, p95, max, and sample count", () => {
  assertEquals(summarize([5, 1, 4, 2, 3]), { p50Ms: 3, p95Ms: 5, maxMs: 5, samples: 5 })
})

Deno.test("summarize of no samples is all zeros", () => {
  assertEquals(summarize([]), { p50Ms: 0, p95Ms: 0, maxMs: 0, samples: 0 })
})

Deno.test("mean and round keep measured values finite and comparable", () => {
  assertEquals(mean([2, 4, 6]), 4)
  assertEquals(mean([]), 0)
  assertEquals(round(1.23456, 2), 1.23)
})
