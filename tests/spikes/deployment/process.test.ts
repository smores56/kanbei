import { assert, assertEquals } from "@std/assert"
import { processProbes } from "../../../src/spikes/deployment/probes/process.ts"
import { runWithProbeEnv } from "./support.ts"

Deno.test("same-UID observations match the measured process boundary", async () => {
  const results = await runWithProbeEnv(processProbes)
  const byId = new Map(results.map((result) => [result.id, result]))

  // An unavailable probe must never be recorded as a pass.
  for (const result of results) {
    if (result.status === "blocked") {
      assert(
        result.observed.startsWith("unavailable:") ||
          result.observed.includes("no measurement"),
      )
    }
  }

  const metadata = byId.get("PR-01")
  assert(metadata !== undefined)
  assertEquals(metadata.status, "fail")
  assert(metadata.evidence.includes("environReadable"))

  const memory = byId.get("PR-02")
  assert(memory !== undefined)
  assertEquals(memory.status, "pass")
  assert(memory.evidence.includes('"opened":false'))

  const ptrace = byId.get("PR-03")
  assert(ptrace !== undefined)
  assertEquals(ptrace.status, "pass")
  assert(ptrace.evidence.includes("Operation not permitted"))

  const signal = byId.get("PR-04")
  assert(signal !== undefined)
  assertEquals(signal.status, "fail")
  assert(signal.evidence.includes('"delivered":true'))

  const descriptor = byId.get("PR-05")
  assert(descriptor !== undefined)
  assertEquals(descriptor.status, "fail")
  assert(descriptor.evidence.includes("contentMatchesCanary=true"))
})
