import { assertEquals } from "@std/assert"
import { buildModelMatrix } from "../../../src/spikes/language/model-arm.ts"
import type { ModelManifest } from "../../../src/spikes/language/model-arm.ts"
import { TASKS } from "../../../src/spikes/language/tasks.ts"
import { LATENCY_PROFILES_MS, TRIALS_PER_CELL } from "../../../src/spikes/language/types.ts"

const blocked = buildModelMatrix({
  manifest: undefined,
  credentials: { NEURALWATT_API_KEY: false },
  sandboxFrozen: false,
  registeredDatasets: { "cost-v1@1.0.0": true, "security-v1@1.0.0": false },
})

Deno.test("blocked model matrix enumerates every cell with zero trials and no metrics", () => {
  assertEquals(blocked.status, "blocked")
  assertEquals(blocked.plannedCells, 5 * 2 * 10 * 3)
  assertEquals(blocked.plannedTrials, blocked.plannedCells * TRIALS_PER_CELL)
  assertEquals(blocked.cells.length, blocked.plannedCells)
  assertEquals(blocked.cells.every((cell) => cell.status === "blocked"), true)
  assertEquals(blocked.cells.every((cell) => cell.trials === 0), true)
  assertEquals(blocked.cells.every((cell) => Object.keys(cell.metrics).length === 0), true)
  assertEquals(blocked.cells.every((cell) => cell.plannedTrials === TRIALS_PER_CELL), true)
  assertEquals(
    new Set(blocked.cells.map((cell) => cell.taskId)).size,
    TASKS.length,
  )
  assertEquals(
    [...new Set(blocked.cells.map((cell) => cell.latencyMs))].toSorted((a, b) => a - b),
    [...LATENCY_PROFILES_MS],
  )
})

Deno.test("blocked model matrix names the exact missing inputs", () => {
  const ids = blocked.missingInputs.map((input) => input.id)
  assertEquals(ids.includes("MI-01"), true)
  assertEquals(ids.includes("MI-CRED-NEURALWATT"), true)
  assertEquals(ids.includes("MI-04"), true)
  assertEquals(ids.includes("MI-06"), true)
  assertEquals(ids.includes("MI-05"), false)
  const idSet = new Set(ids)
  for (const cell of blocked.cells) {
    assertEquals(cell.blockedBy.length > 0, true)
    for (const id of cell.blockedBy) {
      assertEquals(idSet.has(id), true)
    }
  }
  assertEquals(blocked.slots.every((slot) => slot.modelId === null), true)
})

Deno.test("a frozen manifest and credentials make cells ready but never invent results", () => {
  const manifest: ModelManifest = {
    schemaVersion: 1,
    providers: [
      { provider: "neuralwatt", primary: "frozen-primary", smaller: "frozen-smaller" },
    ],
  }
  const ready = buildModelMatrix({
    manifest,
    credentials: { NEURALWATT_API_KEY: true },
    sandboxFrozen: true,
    registeredDatasets: { "cost-v1@1.0.0": true, "security-v1@1.0.0": true },
  })
  assertEquals(ready.status, "ready")
  assertEquals(ready.missingInputs, [])
  assertEquals(ready.cells.every((cell) => cell.status === "ready"), true)
  assertEquals(ready.cells.every((cell) => cell.blockedBy.length === 0), true)
  assertEquals(ready.cells.every((cell) => cell.trials === 0), true)
  assertEquals(ready.slots.filter((slot) => slot.modelId?.startsWith("frozen-")).length, 2)
})
