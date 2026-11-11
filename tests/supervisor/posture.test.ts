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

Deno.test("a writable image, vault, inbox, mutable_code or runtime mount refuses", async () => {
  const names = ["image", "vault", "inbox", "mutable_code", "runtime"] as const
  const outcomes = await Promise.all(
    names.map((name) =>
      refusalFor(posture(), {
        ...manifest,
        mounts: [
          ...manifest.mounts,
          { path: `/host-${name}`, access: "rw" as const, class: name },
        ],
        writableRoots: [...manifest.writableRoots, `/host-${name}`],
      })
    ),
  )
  for (const [index, outcome] of outcomes.entries()) {
    assertEquals(outcome.failure.code, "manifest_invalid", names[index])
  }
})

Deno.test("a manifest without the todo mount refuses", async () => {
  const outcome = await refusalFor(posture(), {
    ...manifest,
    mounts: manifest.mounts.filter((mount) => mount.class !== "todo"),
    writableRoots: ["/scratch"],
  })
  assertEquals(outcome.failure.code, "manifest_invalid")
})

Deno.test("an observed writable root beyond the declaration refuses", async () => {
  const outcome = await refusalFor(posture({ writableRoots: ["/todo", "/scratch", "/var/lib"] }))
  assertEquals(outcome.failure.code, "undeclared_writable_root")
  assertEquals(outcome.refusals[0]?.receiptCode, "sandbox_posture_failed")
})

Deno.test("a declared writable root missing from the observed posture refuses", async () => {
  const outcome = await refusalFor(posture({ writableRoots: ["/todo"] }))
  assertEquals(outcome.failure.code, "writable_root_missing")
})

Deno.test("a matching declared and observed writable-root set starts", async () => {
  const started = await Effect.runPromise(
    Effect.gen(function* () {
      const supervisor = yield* Supervisor
      return yield* supervisor.start(manifest, posture())
    }).pipe(Effect.provide(supervisorLayer())),
  )
  assertEquals(started.channels, ["inbox", "vault", "todo"])
})
