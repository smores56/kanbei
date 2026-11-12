import { assertEquals } from "@std/assert"
import {
  fullSchemaChars,
  progressiveChars,
  progressiveDisclosureRows,
  TOOL_COUNTS,
} from "../../../src/spikes/language/progressive-disclosure.ts"

Deno.test("progressive disclosure reports every pre-registered catalog size", () => {
  const rows = progressiveDisclosureRows()
  assertEquals(rows.map((row) => row.tools), [...TOOL_COUNTS])
  assertEquals(rows.every((row) => row.fullSchemaChars > 0), true)
  assertEquals(rows.every((row) => row.progressiveChars > 0), true)
  assertEquals(rows.every((row) => row.tokenMeasurement === "blocked"), true)
  assertEquals(rows.every((row) => row.blockedBy.includes("MI-07")), true)
})

Deno.test("full-schema exposure grows faster than progressive disclosure", () => {
  assertEquals(fullSchemaChars(1000) > fullSchemaChars(200), true)
  assertEquals(progressiveChars(1000) > progressiveChars(200), true)
  const large = progressiveDisclosureRows().find((row) => row.tools === 1000)
  assertEquals((large?.savingsRatio ?? 0) > 0.5, true)
})

Deno.test("character counts are exact and repeatable", () => {
  assertEquals(fullSchemaChars(10), 11371)
  assertEquals(progressiveChars(10), 11475)
})
