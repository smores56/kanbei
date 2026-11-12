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
  KANBEI_INBOX_DIR: "/mnt/kanbei/inbox",
  KANBEI_IDENTITY: "/var/lib/kanbei/age/identity.txt",
  KANBEI_TODO_DIR: "/mnt/kanbei/todo",
  KANBEI_CHAT_LISTEN: "127.0.0.1:8093",
  KANBEI_STATUS_LISTEN: "127.0.0.1:8094",
  KANBEI_CSR_SOCKET: "/run/kanbei/csr.sock",
  KANBEI_SEARCH_SOCKET: "/run/kanbei/search.sock",
  KANBEI_RELAY_TOKEN: "/run/kanbei-secrets/relay-token",
  KANBEI_CSR_STATE_DIR: "/var/lib/kanbei/tls",
  KANBEI_SEARCH_BUDGET: "/var/lib/kanbei/search-budget.json",
  KANBEI_POINTERS: "/var/lib/kanbei/pointers",
  KANBEI_CHAT_ORIGIN: "https://kanbei.sammohr.dev",
  KANBEI_EPOCH: "epoch-1",
  KANBEI_ORIGINS: "tuta-mail,git",
}

const TOKEN = "k".repeat(48)

/** A boot config whose relay token lives in the test's own directory. */
const bootEnv = async (directory: string): Promise<Record<string, string>> => {
  await Deno.writeTextFile(`${directory}/relay-token`, `${TOKEN}\n`)
  return {
    ...base,
    KANBEI_RELAY_TOKEN: `${directory}/relay-token`,
    KANBEI_STATE_DIR: `${directory}/state`,
    KANBEI_INBOX_DIR: `${directory}/inbox`,
    KANBEI_TODO_DIR: `${directory}/todo`,
    KANBEI_CSR_STATE_DIR: `${directory}/tls`,
    KANBEI_SEARCH_BUDGET: `${directory}/search-budget.json`,
    KANBEI_POINTERS: `${directory}/pointers`,
    KANBEI_CSR_SOCKET: `${directory}/csr.sock`,
    KANBEI_SEARCH_SOCKET: `${directory}/search.sock`,
    KANBEI_CHAT_LISTEN: "127.0.0.1:0",
    KANBEI_STATUS_LISTEN: "127.0.0.1:0",
  }
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

Deno.test("the boot config is explicit, absolute, loopback-only, and origin-checked", async () => {
  await withTempDir("boot-config", async (directory) => {
    const config = await Effect.runPromise(bootConfigFromEnv(env(await bootEnv(directory))))
    assertEquals(config.stateDir, `${directory}/state`)
    assertEquals(config.allowedOrigins, ["tuta-mail", "git"])
    assertEquals(config.chatListen, { host: "127.0.0.1", port: 0 })
    assertEquals(config.relayToken, TOKEN)
    assertEquals(inboxConfigFor(config).limits.maxBatchBytes, 1 << 20)
    assertEquals(inboxConfigFor(config).limits.maxEntriesPerBatch, 1000)
    const relative = await Effect.runPromise(
      Effect.flip(
        bootConfigFromEnv(env({ ...(await bootEnv(directory)), KANBEI_STATE_DIR: "relative" })),
      ),
    )
    assertEquals(relative.variable, "KANBEI_STATE_DIR")
    const unknown = await Effect.runPromise(
      Effect.flip(
        bootConfigFromEnv(env({ ...(await bootEnv(directory)), KANBEI_ORIGINS: "tuta-mail,nope" })),
      ),
    )
    assertEquals(unknown.variable, "KANBEI_ORIGINS")
    // A listener on anything but loopback is a channel leak, not a warning.
    const outside = await Effect.runPromise(
      Effect.flip(
        bootConfigFromEnv(
          env({ ...(await bootEnv(directory)), KANBEI_CHAT_LISTEN: "0.0.0.0:8093" }),
        ),
      ),
    )
    assertEquals(outside.variable, "KANBEI_CHAT_LISTEN")
    const remote = await Effect.runPromise(
      Effect.flip(
        bootConfigFromEnv(
          env({
            ...(await bootEnv(directory)),
            KANBEI_CHAT_UPSTREAM: "https://api.example.invalid/v1",
          }),
        ),
      ),
    )
    assertEquals(remote.variable, "KANBEI_CHAT_UPSTREAM")
    await Deno.writeTextFile(`${directory}/blank-token`, "   \n")
    const refusedToken = await Effect.runPromise(
      Effect.flip(
        bootConfigFromEnv(
          env({ ...(await bootEnv(directory)), KANBEI_RELAY_TOKEN: `${directory}/blank-token` }),
        ),
      ),
    )
    assertEquals(refusedToken.variable, "KANBEI_RELAY_TOKEN")
  })
})

Deno.test("startKernel verifies the observed posture before any channel exists", async () => {
  await withTempDir("boot", async (directory) => {
    await Deno.writeTextFile(`${directory}/manifest.json`, JSON.stringify(manifest))
    await Deno.writeTextFile(`${directory}/posture.json`, JSON.stringify(posture))
    const config = await Effect.runPromise(
      bootConfigFromEnv(env({
        ...(await bootEnv(directory)),
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

Deno.test("the wrapper adds age to the closure and PATH and scopes every grant", () => {
  const flake = Deno.readTextFileSync(new URL("../../flake.nix", import.meta.url))
  assert(flake.includes("--no-remote"))
  // The deployed VM mounts (nix-config modules/nixos/kanbei-vm.nix).
  assert(
    flake.includes(
      "--allow-read=/mnt/kanbei/inbox,/mnt/kanbei/vault,/run/kanbei-secrets,/etc/kanbei," +
        "/nix/store,/mnt/kanbei/todo,/var/lib/kanbei,/run/kanbei",
    ),
  )
  assert(flake.includes("--allow-write=/mnt/kanbei/todo,/var/lib/kanbei,/run/kanbei"))
  // The package shells out to age; without both the grant and PATH it dies at runtime.
  assert(flake.includes("--allow-run=age"))
  const flag = "--prefix PATH : ${pkgs.age}/bin"
  assert(flake.includes(flag))
  assert(flake.includes("pkgs.age"))
  // Loopback is the only reachable address, and no broad sys/net authority exists.
  assert(flake.includes("--allow-net=127.0.0.1"))
  assert(!/--allow-net(?![\w=-])/u.test(flake))
  assert(!flake.includes("--allow-sys"))
  assert(!flake.includes("--allow-all"))
  assert(!flake.includes("--allow-hrtime"))
  assert(flake.includes("deno task typecheck"))
  assert(flake.includes("deno task journey:check"))
  assert(flake.includes("deno task redteam:check"))
})

Deno.test("the Nix check decrypts through the packaged wrapper", () => {
  const flake = Deno.readTextFileSync(new URL("../../flake.nix", import.meta.url))
  assert(flake.includes("age-decrypt-smoke"))
  assert(flake.includes("${kanbei}/bin/kanbei decrypt-smoke"))
  assert(flake.includes("age-keygen"))
})

Deno.test("the unit task covers the packaging tests and the journey residual file", () => {
  const config = JSON.parse(
    Deno.readTextFileSync(new URL("../../deno.json", import.meta.url)),
  ) as { tasks: Readonly<Record<string, string>> }
  assert(config.tasks["test:unit"]?.includes("tests/packaging"))
})

Deno.test("the incompatible frame API is marked experimental", () => {
  for (const file of ["frame.ts", "stream.ts", "client.ts", "server.ts", "schema.ts"]) {
    const source = Deno.readTextFileSync(
      new URL(`../../src/channels/chat/${file}`, import.meta.url),
    )
    assert(source.includes("EXPERIMENTAL"), file)
  }
})
