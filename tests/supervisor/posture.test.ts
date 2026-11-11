import { assertEquals } from "@std/assert"
import { Effect } from "effect"
import type { ObservedPosture, SandboxManifest } from "../../src/kernel/manifest.ts"
import { Supervisor, supervisorLayer } from "../../src/supervisor/supervisor.ts"
import { manifest, posture } from "./support.ts"

const refusalFor = (observed: ObservedPosture, request: SandboxManifest = manifest) =>
  Effect.runPromise(
    Effect.gen(function* () {
      const supervisor = yield* Supervisor
      const failure = yield* Effect.flip(supervisor.start(request, observed))
      return { failure, refusals: yield* supervisor.refusals }
    }).pipe(Effect.provide(supervisorLayer())),
  )

Deno.test("an extra share refuses to start as sandbox_posture_failed", async () => {
  const outcome = await refusalFor(
    posture({
      mounts: [...manifest.mounts, { path: "/host", access: "ro", class: "image" }],
    }),
  )
  assertEquals(outcome.failure.code, "undeclared_mount")
  assertEquals(outcome.refusals[0]?.receiptCode, "sandbox_posture_failed")
})

Deno.test("an extra channel socket refuses to start as sandbox_posture_failed", async () => {
  const outcome = await refusalFor(
    posture({ channelSockets: ["inbox", "vault", "todo", "search"] }),
  )
  assertEquals(outcome.failure.code, "undeclared_channel")
  assertEquals(outcome.refusals[0]?.receiptCode, "sandbox_posture_failed")
})

Deno.test("a missing capability socket refuses to start as sandbox_posture_failed", async () => {
  const outcome = await refusalFor(posture({ channelSockets: ["inbox", "vault"] }))
  assertEquals(outcome.failure.code, "channel_missing")
  assertEquals(outcome.refusals[0]?.receiptCode, "sandbox_posture_failed")
})

Deno.test("a network interface beyond lo refuses to start as sandbox_posture_failed", async () => {
  const outcome = await refusalFor(posture({ addresses: ["lo", "eth0"] }))
  assertEquals(outcome.failure.code, "network_present")
  assertEquals(outcome.refusals[0]?.receiptCode, "sandbox_posture_failed")
})
