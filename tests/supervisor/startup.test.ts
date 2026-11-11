import { assertEquals } from "@std/assert"
import { Effect } from "effect"
import type { ObservedPosture, SandboxManifest } from "../../src/kernel/manifest.ts"
import { manifest, posture } from "./support.ts"
import { Supervisor, supervisorLayer } from "../../src/supervisor/supervisor.ts"

const start = (observed: ObservedPosture, request: SandboxManifest = manifest) =>
  Effect.runPromise(
    Effect.gen(function* () {
      const supervisor = yield* Supervisor
      return yield* Effect.flip(supervisor.start(request, observed))
    }).pipe(Effect.provide(supervisorLayer())),
  )

Deno.test("a declared posture starts and issues a generation-bound token", async () => {
  const started = await Effect.runPromise(
    Effect.gen(function* () {
      const supervisor = yield* Supervisor
      return yield* supervisor.start(manifest, posture())
    }).pipe(Effect.provide(supervisorLayer())),
  )
  assertEquals(started.sandboxId, "sbx-1")
  assertEquals(started.startToken.startsWith("sha256:"), true)
  assertEquals(started.channels, ["inbox", "vault", "todo"])

  const repeated = await Effect.runPromise(
    Effect.gen(function* () {
      const supervisor = yield* Supervisor
      return yield* supervisor.start(manifest, posture())
    }).pipe(Effect.provide(supervisorLayer())),
  )
  assertEquals(repeated.startToken, started.startToken)
})

Deno.test("a missing channel socket refuses to start", async () => {
  const refusal = await start(posture({ channelSockets: ["inbox", "vault"] }))
  assertEquals(refusal.code, "channel_missing")
})

Deno.test("an undeclared channel socket refuses to start", async () => {
  const refusal = await start(posture({ channelSockets: ["inbox", "vault", "todo", "chat"] }))
  assertEquals(refusal.code, "undeclared_channel")
})

Deno.test("an undeclared mount refuses to start", async () => {
  const refusal = await start(
    posture({
      mounts: [...manifest.mounts, { path: "/host", access: "ro", class: "image" }],
    }),
  )
  assertEquals(refusal.code, "undeclared_mount")
})

Deno.test("a weaker enforcement class refuses to start", async () => {
  const refusal = await start(posture({ enforcement: "none" }), {
    ...manifest,
    enforcementRequired: "process_cgroup",
  })
  assertEquals(refusal.code, "posture_weaker")
})

Deno.test("a reachable interface beyond lo refuses to start", async () => {
  const refusal = await start(posture({ addresses: ["lo", "eth0"] }))
  assertEquals(refusal.code, "network_present")
})

Deno.test("a generation mismatch refuses to start", async () => {
  const refusal = await start(posture({ generation: "gen-old" }))
  assertEquals(refusal.code, "generation_mismatch")
})

Deno.test("an invalid manifest refuses before any observation", async () => {
  const refusal = await start(posture(), {
    ...manifest,
    mounts: manifest.mounts.map((mount) =>
      mount.class === "inbox" ? { ...mount, access: "rw" as const } : mount
    ),
  })
  assertEquals(refusal.code, "manifest_invalid")
})

Deno.test("a refused start records a receipt and registers no sandbox", async () => {
  const state = await Effect.runPromise(
    Effect.gen(function* () {
      const supervisor = yield* Supervisor
      yield* Effect.flip(supervisor.start(manifest, posture({ addresses: ["lo", "eth0"] })))
      return { records: yield* supervisor.records, refusals: yield* supervisor.refusals }
    }).pipe(Effect.provide(supervisorLayer())),
  )
  assertEquals(state.records.length, 0)
  assertEquals(state.refusals.map((refusal) => refusal.code), ["network_present"])
  assertEquals(state.refusals[0]?.sandboxId, "sbx-1")
})
