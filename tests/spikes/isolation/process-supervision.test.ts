import { assertEquals } from "@std/assert"
import { sandboxChildPid } from "../../../src/spikes/isolation/arms/process-supervision.ts"
import type { ProcessStat } from "../../../src/spikes/isolation/arms/process-supervision.ts"

const stat = (pid: number, ppid: number, state: string): ProcessStat => ({ pid, ppid, state })

Deno.test("sandbox child is the waiter's direct child in the supervisor cgroup", () => {
  const stats = [
    stat(100, 1, "S"),
    stat(101, 100, "S"),
    stat(102, 100, "S"),
  ]
  assertEquals(sandboxChildPid([100, 101, 102], 100, stats), 101)
})

Deno.test("sandbox child discovery ignores foreign pids and the waiter itself", () => {
  const stats = [
    stat(100, 1, "S"),
    stat(102, 1, "S"),
  ]
  assertEquals(sandboxChildPid([100, 102], 100, stats), undefined)
})

Deno.test("sandbox child must be a member of the supervisor cgroup", () => {
  assertEquals(sandboxChildPid([100], 100, [stat(101, 100, "S")]), undefined)
})
