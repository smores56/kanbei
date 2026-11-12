import { assertEquals } from "@std/assert"
import { Schema } from "effect"

const RootlessResult = Schema.Struct({
  schemaVersion: Schema.Literal(1),
  task: Schema.Literal("A2b"),
  sandbox: Schema.Struct({
    mechanism: Schema.String,
    uid: Schema.Number,
    brokerPid: Schema.Number,
    brokerSocket: Schema.String,
    processExit: Schema.Number,
    processSuccess: Schema.Boolean,
    stderr: Schema.String,
  }),
  checks: Schema.Struct({
    allowedRead: Schema.Boolean,
    deniedReadBlocked: Schema.Boolean,
    allowedWrite: Schema.Boolean,
    deniedWriteBlocked: Schema.Boolean,
    brokerFdBlocked: Schema.Boolean,
    brokerSignalBlocked: Schema.Boolean,
    brokerSocketReachable: Schema.Boolean,
    systemdControlBlocked: Schema.Boolean,
    procSelfRootBlocked: Schema.Boolean,
  }),
  verdict: Schema.Literals(["pass", "needs-fixes"]),
})

Deno.test("A2b composed rootless sandbox passes every probe", async () => {
  const raw = await Deno.readTextFile(
    new URL("../../../spikes/deployment/rootless-result.json", import.meta.url),
  )
  const result = Schema.decodeUnknownSync(RootlessResult)(JSON.parse(raw))
  assertEquals(result.verdict, "pass")
  assertEquals(Object.values(result.checks).every(Boolean), true)
  assertEquals(result.sandbox.processSuccess, true)
})
