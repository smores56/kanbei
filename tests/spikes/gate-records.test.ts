import { assertEquals } from "@std/assert"
import { Schema } from "effect"
import { GateRecord } from "../../src/spikes/gates/gate-record.ts"

Deno.test("all pre-registered A5 gate records are valid and open", async () => {
  const raw = await Deno.readTextFile(
    new URL("../../spikes/gates/records.json", import.meta.url),
  )
  const records = Schema.decodeUnknownSync(Schema.Array(GateRecord))(JSON.parse(raw))
  assertEquals(records.map((record) => record.gate), ["DG-1", "DG-2", "DG-3", "DG-4"])
  assertEquals(records.every((record) => record.status === "open"), true)
  assertEquals(records.every((record) => record.signedBy === undefined), true)
})
