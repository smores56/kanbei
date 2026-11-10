import { assertEquals } from "@std/assert"
import { Schema } from "effect"
import { GateRecord } from "../../src/spikes/gates/gate-record.ts"

Deno.test("A5 gate records capture the signed and blocked decisions", async () => {
  const raw = await Deno.readTextFile(
    new URL("../../spikes/gates/records.json", import.meta.url),
  )
  const records = Schema.decodeUnknownSync(Schema.Array(GateRecord))(JSON.parse(raw))
  assertEquals(records.map((record) => record.gate), ["DG-1", "DG-2", "DG-3", "DG-4"])
  assertEquals(records.map((record) => record.status), ["blocked", "closed", "closed", "blocked"])
  assertEquals(records.find((record) => record.gate === "DG-2")?.selected, "deno@2.9.2")
  assertEquals(
    records.find((record) => record.gate === "DG-3")?.selected,
    "variant-E-rootless-sandbox",
  )
  assertEquals(records.find((record) => record.gate === "DG-2")?.signedBy, "human")
  assertEquals(records.find((record) => record.gate === "DG-3")?.signedBy, "human")
  assertEquals(records.find((record) => record.gate === "DG-1")?.signedBy, undefined)
  assertEquals(records.find((record) => record.gate === "DG-3")?.blocks, ["E1", "E2", "H7"])
})
