import { assertEquals } from "@std/assert"
import {
  fanoutGold,
  hostItem,
  STREAM_CHECKSUM_20,
} from "../../../src/spikes/language/host-calls.ts"
import {
  adversarialTasks,
  TASK_SUITE,
  taskById,
  TASKS,
} from "../../../src/spikes/language/tasks.ts"

Deno.test("task suite contains ten distinct tasks covering every required category", () => {
  assertEquals(TASK_SUITE.suiteId, "lang-v1")
  assertEquals(TASKS.length, 10)
  assertEquals(new Set(TASKS.map((task) => task.id)).size, 10)
  assertEquals(new Set(TASKS.map((task) => task.category)).size, 10)
  assertEquals(TASKS.map((task) => task.ordinal), [1, 2, 3, 4, 5, 6, 7, 8, 9, 10])
})

Deno.test("every task carries a gold output and a multi-criterion rubric", () => {
  for (const task of TASKS) {
    assertEquals(task.goldOutputJson.length > 2, true)
    assertEquals(task.rubric.length >= 2, true)
    assertEquals(task.requiredCapabilities.length > 0, true)
  }
})

Deno.test("deterministic gold outputs match the shared host-call dataset", () => {
  const fanoutTask = taskById("lang-task-01")
  assertEquals(fanoutTask?.goldOutputJson, JSON.stringify(fanoutGold(50)))
  assertEquals(hostItem(0).amount, 1)
  assertEquals(hostItem(27).amount, 100)
  const streamTask = taskById("lang-task-08")
  assertEquals(
    streamTask?.goldOutputJson,
    JSON.stringify({ events: 4, checksum: STREAM_CHECKSUM_20 }),
  )
})

Deno.test("the adversarial task names its canary and no other task is adversarial", () => {
  assertEquals(adversarialTasks.length, 1)
  assertEquals(adversarialTasks[0]?.id, "lang-task-10")
  assertEquals(adversarialTasks[0]?.secretCanary, "KANBEI_BAKEOFF_CANARY")
})
