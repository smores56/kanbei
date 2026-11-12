import { assertEquals } from "@std/assert"
import { Result, Schema } from "effect"
import {
  ALL_CAPABILITIES,
  capabilityManifestIssues,
  isCapabilityName,
  parseCapabilityManifest,
} from "../../src/kernel/capabilities.ts"

const fixture = Schema.decodeUnknownSync(
  Schema.Struct({ agents: Schema.Array(Schema.Unknown) }),
)(JSON.parse(Deno.readTextFileSync("fixtures/capabilities-v1/agents.json")))

Deno.test("the manifest fixture parses and every agent is bound to a declared capability", () => {
  const parsed = fixture.agents.map((agent) => parseCapabilityManifest(agent))
  assertEquals(parsed.every(Result.isSuccess), true)
  for (const result of parsed) {
    if (Result.isSuccess(result)) {
      assertEquals(capabilityManifestIssues(result.success), [])
    }
  }
})

Deno.test("an unknown capability is refused at the boundary", () => {
  const result = parseCapabilityManifest({
    agentId: "agent-a",
    sandboxId: "sbx-a",
    capabilities: ["channel.vault.read", "net.fetch"],
  })
  assertEquals(Result.isFailure(result), true)
})

Deno.test("there is no fs, net, or subprocess member in the vocabulary", () => {
  for (const ambient of ["fs.read", "net.fetch", "subprocess.run", "Deno.readFile", "http.get"]) {
    assertEquals(isCapabilityName(ambient), false)
  }
  assertEquals(ALL_CAPABILITIES.includes("channel.inbox.read"), true)
})

Deno.test("a duplicate capability and an extra field are refused", () => {
  const duplicate = parseCapabilityManifest({
    agentId: "agent-a",
    sandboxId: "sbx-a",
    capabilities: ["channel.vault.read", "channel.vault.read"],
  })
  assertEquals(Result.isFailure(duplicate), true)

  const extra = parseCapabilityManifest({
    agentId: "agent-a",
    sandboxId: "sbx-a",
    capabilities: ["channel.vault.read"],
    network: "present",
  })
  assertEquals(Result.isFailure(extra), true)
})

Deno.test("the capability vocabulary has no duplicates and only channel or lane names", () => {
  assertEquals(new Set(ALL_CAPABILITIES).size, ALL_CAPABILITIES.length)
  assertEquals(
    ALL_CAPABILITIES.every((name) => name.startsWith("channel.") || name === "lane.invoke"),
    true,
  )
})
