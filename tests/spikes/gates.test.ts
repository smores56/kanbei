import { assertEquals } from "@std/assert"
import { Schema } from "effect"
import { GateRecord, openGate } from "../../src/spikes/gates/gate-record.ts"

Deno.test("gate records start open and human-owned", () => {
  const decoded = Schema.decodeUnknownSync(GateRecord)(
    openGate("DG-2", "Node versus Deno", "A1"),
  )
  assertEquals(decoded.status, "open")
  assertEquals(decoded.owner, "human")
  assertEquals(decoded.signedBy, undefined)
})

Deno.test("gate records reject a non-human owner", () => {
  const invalid = { ...openGate("DG-1", "Execution language", "A4"), owner: "agent" }
  assertEquals(Schema.is(GateRecord)(invalid), false)
})
