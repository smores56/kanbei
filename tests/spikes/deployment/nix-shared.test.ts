import { assertEquals } from "@std/assert"
import { Schema } from "effect"

const SharedNixResult = Schema.Struct({
  schemaVersion: Schema.Literal(1),
  task: Schema.Literal("A2c"),
  sharedStore: Schema.String,
  sandbox: Schema.Struct({
    processExit: Schema.Number,
    processSuccess: Schema.Boolean,
    stderr: Schema.String,
  }),
  result: Schema.Struct({
    abi: Schema.Number,
    build: Schema.Struct({
      success: Schema.Boolean,
      code: Schema.Number,
      stdout: Schema.String,
      stderr: Schema.String,
    }),
    shell: Schema.Struct({
      success: Schema.Boolean,
      code: Schema.Number,
      stdout: Schema.String,
      stderr: Schema.String,
    }),
    direct: Schema.Struct({
      success: Schema.Boolean,
      code: Schema.Number,
      stdout: Schema.String,
      stderr: Schema.String,
    }),
    hello: Schema.Boolean,
  }),
  checks: Schema.Struct({
    build: Schema.Boolean,
    direct: Schema.Boolean,
    shell: Schema.Boolean,
    hello: Schema.Boolean,
    hostStoreUnchanged: Schema.Boolean,
    sharedStoreHasPath: Schema.Boolean,
  }),
  verdict: Schema.Literals(["pass", "needs-fixes"]),
})

Deno.test("A2c shared store builds and executes without touching the host store", async () => {
  const raw = await Deno.readTextFile(
    new URL("../../../spikes/deployment/nix-shared-result.json", import.meta.url),
  )
  const result = Schema.decodeUnknownSync(SharedNixResult)(JSON.parse(raw))
  assertEquals(result.verdict, "pass")
  assertEquals(result.checks.build, true)
  assertEquals(result.checks.direct, true)
  assertEquals(result.checks.hello, true)
  assertEquals(result.checks.hostStoreUnchanged, true)
  assertEquals(result.checks.sharedStoreHasPath, true)
  assertEquals(result.checks.shell, false)
})
