import { assertEquals, assertRejects } from "@std/assert"
import { Effect } from "effect"
import { parseProbeOutput, PROBE_MARKER } from "../../../src/spikes/language/arms/contract.ts"
import { ProbeOutputError } from "../../../src/spikes/language/errors.ts"
import { fanoutGold } from "../../../src/spikes/language/host-calls.ts"

const validPayload = {
  probeVersion: 1,
  expression: { ok: true, value: 285 },
  fanout: {
    ok: true,
    calls: 2,
    checksum: 0,
    groups: [["alpha", 0], ["beta", 0], ["gamma", 0]],
    top: [],
    callOverheadsMs: [0.1, 0.2],
    totalMs: 12,
  },
  stream: { events: 4 },
  retry: { attempts: 3, ok: true },
  fileWrite: { attempted: true, allowed: false, errorClass: "PermissionDenied" },
  envRead: { attempted: true, allowed: false },
  processSpawn: { attempted: true, allowed: true },
  network: { attempted: true, allowed: true, detail: "refused" },
  secretFileRead: { attempted: true, allowed: false },
  dependencyImport: { ok: false, modules: [{ name: "socket", ok: false }] },
  snapshot: { supported: false, ok: false },
  approvalPause: { supported: false, ok: false },
}

Deno.test("probe output parses from the last JSON marker line", async () => {
  const stdout = `progress: 1\n${PROBE_MARKER}${JSON.stringify(validPayload)}\n`
  const parsed = await Effect.runPromise(parseProbeOutput("bash", stdout))
  assertEquals(parsed.fanout.calls, 2)
  assertEquals(parsed.retry.ok, true)
  assertEquals(parsed.stream.events, 4)
})

Deno.test("probe output rejects missing markers and malformed payloads", async () => {
  await assertRejects(
    () => Effect.runPromise(parseProbeOutput("bash", "no marker here")),
    ProbeOutputError,
  )
  await assertRejects(
    () => Effect.runPromise(parseProbeOutput("bash", `${PROBE_MARKER}{"probeVersion":1}`)),
    ProbeOutputError,
  )
})

Deno.test("fanout gold is deterministic for the reduced local call counts", () => {
  assertEquals(fanoutGold(2).checksum, 0)
  assertEquals(fanoutGold(2).top, [])
  assertEquals(fanoutGold(20).groups, [["alpha", 123], ["beta", 306], ["gamma", 258]])
})
