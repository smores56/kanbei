import { assertEquals } from "@std/assert"
import {
  cgroupMountOptions,
  delegatedRootOf,
  parseCgroupProcs,
  parseControllers,
  parseCpuUsageUsec,
  parseKeyedNumbers,
  parsePpid,
  parseSelfCgroup,
  parseStartTime,
  parseState,
} from "../../../src/spikes/isolation/supervisor.ts"

Deno.test("parseSelfCgroup returns the v2 path and ignores other hierarchies", () => {
  assertEquals(
    parseSelfCgroup("0::/user.slice/user-1000.slice/run-1.service/supervisor\n"),
    "/user.slice/user-1000.slice/run-1.service/supervisor",
  )
  assertEquals(parseSelfCgroup("1:cpu:/legacy\n"), null)
})

Deno.test("delegatedRootOf strips only the supervisor subgroup", () => {
  assertEquals(
    delegatedRootOf("/user.slice/run-1.service/supervisor"),
    "/user.slice/run-1.service",
  )
  assertEquals(delegatedRootOf("/user.slice/session-1.scope"), null)
  assertEquals(delegatedRootOf(null), null)
})

Deno.test("parseControllers splits the cgroup.controllers list", () => {
  assertEquals(parseControllers("cpu io memory pids\n"), ["cpu", "io", "memory", "pids"])
  assertEquals(parseControllers("  \n"), [])
})

Deno.test("parseKeyedNumbers reads event counters from memory, pids, and cgroup events", () => {
  const memory = parseKeyedNumbers("low 0\nhigh 0\nmax 0\noom 2\noom_kill 3\noom_group_kill 0\n")
  assertEquals(memory["oom_kill"], 3)
  assertEquals(parseKeyedNumbers("max 4\n")["max"], 4)
  assertEquals(parseKeyedNumbers("populated 1\nfrozen 0\n")["populated"], 1)
})

Deno.test("parseCpuUsageUsec reads usage_usec from cpu.stat", () => {
  assertEquals(parseCpuUsageUsec("usage_usec 123456\nuser_usec 100\nsystem_usec 23\n"), 123456)
  assertEquals(parseCpuUsageUsec(""), 0)
})

Deno.test("parseCgroupProcs ignores headers and blank lines", () => {
  assertEquals(parseCgroupProcs("123\n456\n\n"), [123, 456])
  assertEquals(parseCgroupProcs(""), [])
})

Deno.test("parsePpid survives a comm containing spaces and parentheses", () => {
  assertEquals(parsePpid("4242 (deno worker (x)) S 100 4242 4242 0 -1"), 100)
  assertEquals(parsePpid("4242 (sh) R 1 4242"), 1)
  assertEquals(parsePpid("garbage"), null)
})

Deno.test("cgroupMountOptions finds nsdelegate on the cgroup2 superblock", () => {
  const mountinfo =
    "30 1 0:26 / /sys/fs/cgroup rw,nosuid,nodev,noexec,relatime shared:16 - cgroup2 cgroup2 rw,nsdelegate,memory_recursiveprot\n"
  assertEquals(cgroupMountOptions(mountinfo).includes("nsdelegate"), true)
  assertEquals(cgroupMountOptions("").includes("nsdelegate"), false)
})

Deno.test("parseState reads the process state after a comm with spaces", () => {
  assertEquals(parseState("4242 (deno worker (x)) T 100 4242 4242 0 -1"), "T")
  assertEquals(parseState("4242 (sh) R 1 4242"), "R")
  assertEquals(parseState("garbage"), null)
})

Deno.test("parseStartTime reads field 22 for pid-reuse detection", () => {
  const fields = [
    "R",
    "1",
    "4242",
    "4242",
    "0",
    "-1",
    "4194304",
    "100",
    "0",
    "0",
    "0",
    "5",
    "6",
    "0",
    "0",
    "20",
    "0",
    "1",
    "0",
    "123456",
  ]
  assertEquals(parseStartTime(`4242 (deno x) ${fields.join(" ")} 999 1`), 123456)
  assertEquals(parseStartTime("garbage"), null)
})
