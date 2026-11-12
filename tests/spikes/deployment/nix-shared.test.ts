import { assertEquals } from "@std/assert"
import { Schema } from "effect"

const Step = Schema.Struct({
  success: Schema.Boolean,
  code: Schema.Number,
  stdout: Schema.String,
  stderr: Schema.String,
})

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
    build: Step,
    shell: Step,
    direct: Step,
    run: Step,
    profile: Step,
    profileList: Step,
    profileExec: Step,
    hello: Schema.Boolean,
  }),
  checks: Schema.Struct({
    build: Schema.Boolean,
    direct: Schema.Boolean,
    shell: Schema.Boolean,
    run: Schema.Boolean,
    profile: Schema.Boolean,
    profileList: Schema.Boolean,
    profileExec: Schema.Boolean,
    hello: Schema.Boolean,
    hostStoreUnchanged: Schema.Boolean,
    sharedStoreHasPath: Schema.Boolean,
  }),
  verdict: Schema.Literals(["pass", "needs-fixes"]),
})

Deno.test("A2c shared store builds, shells, runs and installs without touching the host store", async () => {
  const raw = await Deno.readTextFile(
    new URL("../../../spikes/deployment/nix-shared-result.json", import.meta.url),
  )
  const record = Schema.decodeUnknownSync(SharedNixResult)(JSON.parse(raw))
  const { checks, result } = record
  assertEquals(record.verdict, "pass")
  assertEquals(Object.values(checks).every(Boolean), true)
  assertEquals(result.hello, true)
  assertEquals(result.shell.stdout, "Hello, world!")
  assertEquals(result.run.stdout, "Hello, world!")
  assertEquals(result.profileExec.stdout, "Hello, world!")
  const profile = JSON.parse(result.profileList.stdout) as {
    readonly elements: Readonly<Record<string, { readonly storePaths: ReadonlyArray<string> }>>
  }
  assertEquals(profile.elements.hello?.storePaths, [result.build.stdout])
})
