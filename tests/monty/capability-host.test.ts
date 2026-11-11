import { assertEquals } from "@std/assert"
import { Effect, Layer, Ref } from "effect"
import type {
  CapabilityBinding,
  MediatedCapabilities,
  MediationRequest,
} from "../../src/monty/capability-host.ts"
import { CapabilityCallFailure, mediateCapabilities } from "../../src/monty/capability-host.ts"
import type { CapabilityManifest } from "../../src/kernel/capabilities.ts"
import { MontyHost, montyHostLayer, type ProcessLimits } from "../../src/monty/host.ts"
import { wasmEngineLayer } from "../../src/monty/wasm-engine.ts"

const manifest: CapabilityManifest = {
  agentId: "agent-a",
  sandboxId: "sbx-a",
  capabilities: ["channel.todo.read", "channel.vault.read"],
}

const binding = (
  capability: CapabilityBinding["capability"],
  run: CapabilityBinding["run"] = () => Effect.succeed("ok"),
): CapabilityBinding => ({ capability, run })

const withMediation = <A, E>(
  request: MediationRequest,
  body: (session: MediatedCapabilities) => Effect.Effect<A, E>,
): Promise<A> =>
  Effect.runPromise(
    Effect.scoped(
      Effect.gen(function* () {
        const session = yield* mediateCapabilities(request)
        return yield* body(session)
      }),
    ).pipe(Effect.orDie),
  )

const failureOf = <E>(effect: Effect.Effect<unknown, E>): Promise<E> =>
  Effect.runPromise(Effect.flip(effect))

Deno.test("a declared capability runs through its closure and leaves one receipt", async () => {
  const calls = await withMediation(
    { manifest, bindings: [binding("channel.todo.read"), binding("channel.vault.read")] },
    (session) =>
      Effect.gen(function* () {
        const value = yield* session.invoke("channel.todo.read", [{ path: "todo.txt" }])
        const receipts = yield* session.receipts
        return { value, receipts }
      }),
  )
  assertEquals(calls.value, "ok")
  assertEquals(
    calls.receipts.map((receipt) => `${receipt.capability}:${receipt.outcome}`),
    ["channel.todo.read:ok"],
  )
  assertEquals(calls.receipts[0]?.agentId, "agent-a")
  assertEquals(calls.receipts[0]?.requestDigest.startsWith("sha256:"), true)
})

Deno.test("a binding for an undeclared capability refuses the session", async () => {
  const refusal = await failureOf(
    Effect.scoped(
      mediateCapabilities({
        manifest,
        bindings: [
          binding("channel.todo.read"),
          binding("channel.vault.read"),
          binding("channel.search.query"),
        ],
      }),
    ),
  )
  assertEquals(refusal.code, "capability_undeclared")
})

Deno.test("a declared capability without a binding refuses the session", async () => {
  const refusal = await failureOf(
    Effect.scoped(
      mediateCapabilities({ manifest, bindings: [binding("channel.todo.read")] }),
    ),
  )
  assertEquals(refusal.code, "capability_unbound")
})

Deno.test("a call outside the manifest is denied and receipted", async () => {
  const outcome = await withMediation(
    { manifest, bindings: [binding("channel.todo.read"), binding("channel.vault.read")] },
    (session) =>
      Effect.gen(function* () {
        const failure = yield* Effect.flip(session.invoke("channel.search.query", ["q"]))
        return { failure, receipts: yield* session.receipts }
      }),
  )
  assertEquals(outcome.failure.code, "capability_denied")
  assertEquals(
    outcome.receipts.map((receipt) => `${receipt.capability}:${receipt.outcome}`),
    ["channel.search.query:denied"],
  )
})

Deno.test("the session end revokes every capability and receipts the late call", async () => {
  const captured = await Effect.runPromise(
    Effect.scoped(
      mediateCapabilities({
        manifest,
        bindings: [binding("channel.todo.read"), binding("channel.vault.read")],
      }),
    ).pipe(Effect.orDie),
  )
  assertEquals(await Effect.runPromise(captured.revoked), true)
  const failure = await failureOf(captured.invoke("channel.todo.read", []))
  assertEquals(failure.code, "capability_revoked")
  assertEquals(
    (await Effect.runPromise(captured.receipts)).map((receipt) => receipt.outcome),
    ["revoked"],
  )
})

Deno.test("the generated stubs are exactly the manifest under Python-safe names", async () => {
  const outcome = await withMediation(
    { manifest, bindings: [binding("channel.todo.read"), binding("channel.vault.read")] },
    (session) => Effect.succeed(session.stubs.map((entry) => entry.name)),
  )
  assertEquals(outcome, ["channel_todo_read", "channel_vault_read"])
})

Deno.test("a revoked stub is a Monty capability denial, not a program error", async () => {
  const captured = await Effect.runPromise(
    Effect.scoped(
      mediateCapabilities({
        manifest,
        bindings: [binding("channel.todo.read"), binding("channel.vault.read")],
      }),
    ).pipe(Effect.orDie),
  )
  const first = captured.stubs[0]
  const failure = await failureOf(first === undefined ? Effect.die("stub missing") : first.run([]))
  assertEquals(failure.reason, "capability_denied")
})

Deno.test("a failed binding is receipted as failed and never as success", async () => {
  const outcome = await withMediation(
    {
      manifest: { ...manifest, capabilities: ["channel.todo.read"] },
      bindings: [
        binding("channel.todo.read", () =>
          Effect.fail(
            new CapabilityCallFailure({
              code: "capability_failed",
              capability: "channel.todo.read",
              detail: "mount missing",
            }),
          )),
      ],
    },
    (session) =>
      Effect.gen(function* () {
        const failure = yield* Effect.flip(session.invoke("channel.todo.read", []))
        return { failure, receipts: yield* session.receipts }
      }),
  )
  assertEquals(outcome.failure.code, "capability_failed")
  assertEquals(outcome.receipts.map((receipt) => receipt.outcome), ["failed"])
})

Deno.test("mediated stubs run inside a Monty session with no other host function", async () => {
  const limits: ProcessLimits = {
    maxSessions: 2,
    maxWorkers: 1,
    totalMemoryBytes: 32_000_000,
    perSessionMemoryBytes: 12_000_000,
    perSessionFeedMs: 400,
    checkoutTimeoutMs: 1_000,
  }
  const counter = await Effect.runPromise(Ref.make(0))
  const hostLayer = montyHostLayer(limits).pipe(Layer.provide(wasmEngineLayer(limits)))
  const value = await Effect.runPromise(
    Effect.gen(function* () {
      const host = yield* MontyHost
      const session = yield* mediateCapabilities({
        manifest: { ...manifest, capabilities: ["channel.todo.read"] },
        bindings: [
          binding(
            "channel.todo.read",
            () => Ref.update(counter, (count) => count + 1).pipe(Effect.as("todo")),
          ),
        ],
      })
      const opened = yield* host.open({ agentId: "agent-a", stubs: session.stubs })
      const denied = yield* Effect.flip(opened.run("await channel_search_query('q')"))
      return { value: yield* opened.run("await channel_todo_read()"), denied }
    }).pipe(Effect.provide(hostLayer), Effect.scoped, Effect.orDie),
  )
  assertEquals(value.value, "todo")
  assertEquals(value.denied.reason, "program_error")
  assertEquals(await Effect.runPromise(Ref.get(counter)), 1)
})
