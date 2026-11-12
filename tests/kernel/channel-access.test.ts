import { assertEquals } from "@std/assert"
import { Effect } from "effect"
import type { ProcessIdentity, SandboxRecord } from "../../src/kernel/identity.ts"
import { ChannelGate, channelGateLayer } from "../../src/kernel/channel-access.ts"

const record: SandboxRecord = {
  sandboxId: "sbx-1",
  agentId: "agent-1",
  generation: "gen-1",
  cgroupPath: "/kanbei/agents/sbx-1",
  channels: ["inbox", "vault", "todo"],
  startToken: "sha256:token",
}

const caller = (overrides: Partial<ProcessIdentity> = {}): ProcessIdentity => ({
  component: "agent",
  instanceId: "instance-1",
  generation: "gen-1",
  cgroupPath: "/kanbei/agents/sbx-1",
  channels: ["inbox", "vault", "todo"],
  sandboxId: "sbx-1",
  startToken: "sha256:token",
  ...overrides,
})

const onGate = <A, E>(effect: Effect.Effect<A, E, ChannelGate>): Promise<A> =>
  Effect.runPromise(effect.pipe(Effect.provide(channelGateLayer), Effect.orDie))

Deno.test("a declared channel is admitted and leaves no denial receipt", async () => {
  const outcome = await onGate(
    Effect.gen(function* () {
      const gate = yield* ChannelGate
      const attribution = yield* gate.authorize(record, caller(), "todo")
      return { attribution, receipts: yield* gate.receipts }
    }),
  )
  assertEquals(outcome.attribution.channel, "todo")
  assertEquals(outcome.receipts.length, 0)
})

Deno.test("a call for a channel outside the manifest denies and is receipted", async () => {
  const outcome = await onGate(
    Effect.gen(function* () {
      const gate = yield* ChannelGate
      const failure = yield* Effect.flip(gate.authorize(record, caller(), "search"))
      return { failure, receipts: yield* gate.receipts }
    }),
  )
  assertEquals(outcome.failure.code, "channel_not_granted")
  assertEquals(outcome.receipts.length, 1)
  assertEquals(outcome.receipts[0]?.channel, "search")
  assertEquals(outcome.receipts[0]?.code, "channel_not_granted")
})

Deno.test("a forged start token and a call from a different cgroup both deny and receipt", async () => {
  const outcome = await onGate(
    Effect.gen(function* () {
      const gate = yield* ChannelGate
      const forged = yield* Effect.flip(
        gate.authorize(record, caller({ startToken: "sha256:forged" }), "inbox"),
      )
      const foreign = yield* Effect.flip(
        gate.authorize(record, caller({ cgroupPath: "/kanbei/agents/other" }), "inbox"),
      )
      return { forged, foreign, receipts: yield* gate.receipts }
    }),
  )
  assertEquals(outcome.forged.code, "token_mismatch")
  assertEquals(outcome.foreign.code, "cgroup_mismatch")
  assertEquals(outcome.receipts.map((receipt) => receipt.code), [
    "token_mismatch",
    "cgroup_mismatch",
  ])
})
