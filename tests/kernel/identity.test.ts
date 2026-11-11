import { assertEquals } from "@std/assert"
import { Effect, Result } from "effect"
import { verifyCaller } from "../../src/kernel/identity.ts"
import type { ProcessIdentity, SandboxRecord } from "../../src/kernel/identity.ts"
import { manifest, posture } from "../supervisor/support.ts"
import { Supervisor, supervisorLayer } from "../../src/supervisor/supervisor.ts"

const record: SandboxRecord = {
  sandboxId: "sbx-1",
  agentId: "agent-1",
  generation: "gen-1",
  cgroupPath: "/kanbei/agents/sbx-1",
  channels: ["inbox", "vault"],
  startToken: "sha256:token",
}

const caller = (overrides: Partial<ProcessIdentity> = {}): ProcessIdentity => ({
  component: "agent",
  instanceId: "instance-1",
  generation: "gen-1",
  cgroupPath: "/kanbei/agents/sbx-1",
  channels: ["inbox", "vault"],
  sandboxId: "sbx-1",
  startToken: "sha256:token",
  ...overrides,
})

Deno.test("a matching caller is attributed to its sandbox", () => {
  const verdict = verifyCaller(record, caller(), "inbox")
  assertEquals(Result.isSuccess(verdict), true)
  if (Result.isSuccess(verdict)) assertEquals(verdict.success.agentId, "agent-1")
})

Deno.test("a wrong token, cgroup, generation, or sandbox is refused", () => {
  const cases = [
    [caller({ startToken: "sha256:other" }), "token_mismatch"],
    [caller({ cgroupPath: "/kanbei/agents/other" }), "cgroup_mismatch"],
    [caller({ generation: "gen-old" }), "generation_mismatch"],
    [caller({ sandboxId: "sbx-2" }), "sandbox_mismatch"],
  ] as const
  for (const [presented, code] of cases) {
    const verdict = verifyCaller(record, presented, "inbox")
    assertEquals(Result.isFailure(verdict), true)
    if (Result.isFailure(verdict)) assertEquals(verdict.failure.code, code)
  }
})

Deno.test("a channel outside the sandbox grant is refused", () => {
  const verdict = verifyCaller(record, caller(), "chat")
  assertEquals(Result.isFailure(verdict), true)
  if (Result.isFailure(verdict)) assertEquals(verdict.failure.code, "channel_not_granted")
})

Deno.test("a trusted-core component cannot borrow an agent sandbox identity", () => {
  const verdict = verifyCaller(record, caller({ component: "core" }), "inbox")
  assertEquals(Result.isFailure(verdict), true)
  if (Result.isFailure(verdict)) assertEquals(verdict.failure.code, "component_not_agent")
})

Deno.test("the supervisor-issued token authenticates the registered sandbox", async () => {
  const started = await Effect.runPromise(
    Effect.gen(function* () {
      const supervisor = yield* Supervisor
      return yield* supervisor.start(manifest, posture())
    }).pipe(Effect.provide(supervisorLayer())),
  )
  const registered: SandboxRecord = {
    sandboxId: started.sandboxId,
    agentId: manifest.agentId,
    generation: manifest.generation,
    cgroupPath: "/kanbei/agents/sbx-1",
    channels: started.channels,
    startToken: started.startToken,
  }
  const verdict = verifyCaller(
    registered,
    {
      component: "agent",
      instanceId: "instance-1",
      generation: manifest.generation,
      cgroupPath: "/kanbei/agents/sbx-1",
      channels: started.channels,
      sandboxId: started.sandboxId,
      startToken: started.startToken,
    },
    "todo",
  )
  assertEquals(Result.isSuccess(verdict), true)
})
