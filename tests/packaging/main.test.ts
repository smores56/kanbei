import { assert, assertEquals } from "@std/assert"
import { Effect } from "effect"
import { bootConfigFromEnv, BootError, inboxConfigFor, startKernel } from "../../src/main.ts"
import { supervisorLayer } from "../../src/supervisor/supervisor.ts"
import { withTempDir } from "../support.ts"

const env = (values: Record<string, string>) => (name: string) => values[name]

const base = {
  KANBEI_MANIFEST: "/run/kanbei/manifest.json",
  KANBEI_POSTURE: "/run/kanbei/posture.json",
  KANBEI_STATE_DIR: "/var/lib/kanbei",
  KANBEI_INBOX_DIR: "/var/lib/ingest/inbox",
  KANBEI_IDENTITY: "/var/lib/kanbei/age/identity.txt",
  KANBEI_EPOCH: "epoch-1",
  KANBEI_ORIGINS: "tuta-mail,git",
}

const manifest = {
  sandboxId: "sbx-1",
  agentId: "agent-1",
  generation: "gen-1",
  cgroupPath: "/kanbei/agents/sbx-1",
  enforcementRequired: "monty_wasm",
  mounts: [
    { path: "/inbox", access: "ro", class: "inbox" },
    { path: "/vault", access: "ro", class: "vault" },
    { path: "/todo", access: "rw", class: "todo" },
  ],
  writableRoots: ["/todo"],
  channels: ["inbox", "vault", "todo"],
  limits: {
    wallMs: 60_000,
    memoryBytes: 10_000_000,
    pidsMax: 32,
    cpuQuota: 1,
    openFiles: 128,
    todoBytes: 65_536,
  },
}

const posture = {
  enforcement: "monty_wasm",
  generation: "gen-1",
  addresses: ["lo"],
  channelSockets: ["inbox", "vault", "todo"],
  mounts: manifest.mounts,
  writableRoots: ["/todo"],
}

Deno.test("a missing required setting refuses the boot", async () => {
  const error = await Effect.runPromise(
    Effect.flip(bootConfigFromEnv(env({ ...base, KANBEI_MANIFEST: "" }))),
  )
  assertEquals(error instanceof BootError, true)
  assertEquals(error.variable, "KANBEI_MANIFEST")
})

Deno.test("the boot config is explicit, absolute, and origin-checked", async () => {
  const config = await Effect.runPromise(bootConfigFromEnv(env(base)))
  assertEquals(config.stateDir, "/var/lib/kanbei")
  assertEquals(config.allowedOrigins, ["tuta-mail", "git"])
  assertEquals(inboxConfigFor(config).limits.maxBatchBytes, 1 << 20)
  const relative = await Effect.runPromise(
    Effect.flip(bootConfigFromEnv(env({ ...base, KANBEI_STATE_DIR: "relative/path" }))),
  )
  assertEquals(relative.variable, "KANBEI_STATE_DIR")
  const unknown = await Effect.runPromise(
    Effect.flip(bootConfigFromEnv(env({ ...base, KANBEI_ORIGINS: "tuta-mail,nope" }))),
  )
  assertEquals(unknown.variable, "KANBEI_ORIGINS")
})

Deno.test("startKernel verifies the observed posture before any channel exists", async () => {
  await withTempDir("boot", async (directory) => {
    await Deno.writeTextFile(`${directory}/manifest.json`, JSON.stringify(manifest))
    await Deno.writeTextFile(`${directory}/posture.json`, JSON.stringify(posture))
    const config = await Effect.runPromise(
      bootConfigFromEnv(env({
        ...base,
        KANBEI_MANIFEST: `${directory}/manifest.json`,
        KANBEI_POSTURE: `${directory}/posture.json`,
      })),
    )
    const started = await Effect.runPromise(
      startKernel(config).pipe(Effect.provide(supervisorLayer())),
    )
    assertEquals(started.sandboxId, "sbx-1")

    await Deno.writeTextFile(
      `${directory}/posture.json`,
      JSON.stringify({ ...posture, writableRoots: ["/todo", "/var/lib"] }),
    )
    const refused = await Effect.runPromise(
      Effect.flip(startKernel(config).pipe(Effect.provide(supervisorLayer()))),
    )
    assertEquals(refused.variable, "KANBEI_POSTURE")
    assert(refused.reason.includes("undeclared_writable_root"))
  })
})

Deno.test("the wrapper keeps --no-remote, scopes writes, and drops broad powers", () => {
  const flake = Deno.readTextFileSync(new URL("../../flake.nix", import.meta.url))
  assert(flake.includes("--no-remote"))
  assert(flake.includes("--allow-write=/var/lib/kanbei,/run/kanbei"))
  assert(!flake.includes("--allow-run"))
  assert(!flake.includes("--allow-sys"))
  assert(!flake.includes(" --allow-net"))
  assert(flake.includes("deno task typecheck"))
  assert(flake.includes("deno task journey:check"))
  assert(flake.includes("deno task redteam:check"))
})

Deno.test("the incompatible frame API is marked experimental", () => {
  for (const file of ["frame.ts", "stream.ts", "client.ts", "server.ts", "schema.ts"]) {
    const source = Deno.readTextFileSync(
      new URL(`../../src/channels/chat/${file}`, import.meta.url),
    )
    assert(source.includes("EXPERIMENTAL"), file)
  }
})
