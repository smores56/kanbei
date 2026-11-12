import { assert, assertEquals } from "@std/assert"
import { Effect, Layer, Result } from "effect"
import { Scope } from "effect"
import { MontyHost, montyHostLayer, type ProcessLimits } from "../../src/monty/host.ts"
import { stub } from "../../src/monty/stubs.ts"
import { wasmEngineLayer } from "../../src/monty/wasm-engine.ts"

const limits: ProcessLimits = {
  maxSessions: 4,
  maxWorkers: 2,
  totalMemoryBytes: 64_000_000,
  perSessionMemoryBytes: 12_000_000,
  perSessionFeedMs: 400,
  checkoutTimeoutMs: 1_000,
}

const hostLayer = montyHostLayer(limits).pipe(Layer.provide(wasmEngineLayer(limits)))

const onHost = <A, E>(effect: Effect.Effect<A, E, MontyHost | Scope.Scope>): Promise<A> =>
  Effect.runPromise(effect.pipe(Effect.provide(hostLayer), Effect.scoped, Effect.orDie))

Deno.test("a Monty session computes arithmetic without ambient capability", async () => {
  const value = await onHost(
    Effect.gen(function* () {
      const host = yield* MontyHost
      const session = yield* host.open({ agentId: "agent-a", stubs: [] })
      return yield* session.run("6 * 7")
    }),
  )
  assertEquals(value, 42)
})

Deno.test("the engine boundary denies ambient filesystem reads", async () => {
  const failure = await onHost(
    Effect.gen(function* () {
      const host = yield* MontyHost
      const session = yield* host.open({ agentId: "agent-a", stubs: [] })
      return yield* Effect.flip(session.run("open('/etc/passwd').read()"))
    }),
  )
  assertEquals(failure.reason, "capability_denied")
  assertEquals(failure.exceptionType, "PermissionError")
})

Deno.test("the engine boundary denies urllib and subprocess imports", async () => {
  const failures = await Promise.all(
    ["import urllib.request", "import subprocess"].map((code) =>
      onHost(
        Effect.gen(function* () {
          const host = yield* MontyHost
          const session = yield* host.open({ agentId: "agent-a", stubs: [] })
          return yield* Effect.flip(session.run(code))
        }),
      )
    ),
  )
  for (const failure of failures) {
    assertEquals(failure.reason, "capability_denied")
    assertEquals(failure.exceptionType, "ModuleNotFoundError")
  }
})

Deno.test("a hang is bounded and a fresh session runs afterwards", async () => {
  const failure = await onHost(
    Effect.gen(function* () {
      const host = yield* MontyHost
      const session = yield* host.open({ agentId: "agent-a", stubs: [] })
      return yield* Effect.flip(session.run("while True:\n    pass"))
    }),
  )
  assertEquals(failure.reason, "timeout")

  const value = await onHost(
    Effect.gen(function* () {
      const host = yield* MontyHost
      const session = yield* host.open({ agentId: "agent-a", stubs: [] })
      return yield* session.run("1 + 1")
    }),
  )
  assertEquals(value, 2)
})

Deno.test("an allocation beyond the memory ceiling is bounded and reported", async () => {
  const failure = await onHost(
    Effect.gen(function* () {
      const host = yield* MontyHost
      const session = yield* host.open({ agentId: "agent-a", stubs: [] })
      return yield* Effect.flip(session.run("x = [0] * 50_000_000\nlen(x)"))
    }),
  )
  assertEquals(failure.reason, "memory_exhausted")
  assertEquals(failure.exceptionType, "MemoryError")
})

Deno.test("host-function closures are scoped to one agent session", async () => {
  const outcome = await onHost(
    Effect.gen(function* () {
      const host = yield* MontyHost
      const a = yield* host.open({
        agentId: "agent-a",
        stubs: [stub("read_todo", () => Effect.succeed("agent-a todo"))],
      })
      const b = yield* host.open({
        agentId: "agent-b",
        stubs: [stub("read_todo", () => Effect.succeed("agent-b todo"))],
      })
      const aValue = yield* a.run("await read_todo()")
      const bValue = yield* b.run("await read_todo()")
      const leaked = yield* Effect.flip(a.run("only_b()"))
      const receipts = yield* host.receipts
      return { aValue, bValue, leaked, receipts }
    }),
  )
  assertEquals(outcome.aValue, "agent-a todo")
  assertEquals(outcome.bValue, "agent-b todo")
  assertEquals(outcome.leaked.reason, "program_error")
  assertEquals(
    outcome.receipts.filter((receipt) => receipt.outcome === "ok").map((receipt) =>
      `${receipt.agentId}:${receipt.stub}`
    ),
    ["agent-a:read_todo", "agent-b:read_todo"],
  )
  assert(
    outcome.receipts.some((receipt) =>
      receipt.agentId === "agent-a" && receipt.stub === "only_b" && receipt.outcome === "denied"
    ),
  )
})

Deno.test("an undeclared stub is denied and receipted without running", async () => {
  const outcome = await onHost(
    Effect.gen(function* () {
      const host = yield* MontyHost
      const session = yield* host.open({
        agentId: "agent-a",
        stubs: [stub("read_todo", () => Effect.succeed("ok"))],
      })
      const failure = yield* Effect.flip(session.run("write_todo('x')"))
      return { failure, receipts: yield* host.receipts }
    }),
  )
  assertEquals(outcome.failure.reason, "program_error")
  assertEquals(
    outcome.receipts.map((receipt) => `${receipt.stub}:${receipt.outcome}`),
    ["write_todo:denied"],
  )
})

Deno.test("the process-wide session budget fails closed and releases on close", async () => {
  const tight: ProcessLimits = { ...limits, maxSessions: 1 }
  const tightLayer = montyHostLayer(tight).pipe(Layer.provide(wasmEngineLayer(tight)))
  const runTight = <A, E>(effect: Effect.Effect<A, E, MontyHost | Scope.Scope>): Promise<A> =>
    Effect.runPromise(effect.pipe(Effect.provide(tightLayer), Effect.scoped, Effect.orDie))

  const result = await runTight(
    Effect.gen(function* () {
      const host = yield* MontyHost
      const blocked = yield* Effect.scoped(Effect.gen(function* () {
        yield* host.open({ agentId: "agent-a", stubs: [] })
        return yield* Effect.flip(host.open({ agentId: "agent-b", stubs: [] }))
      }))
      const afterRelease = yield* Effect.scoped(Effect.gen(function* () {
        const session = yield* host.open({ agentId: "agent-c", stubs: [] })
        return yield* session.run("1")
      }))
      return { blocked, afterRelease }
    }),
  )
  assertEquals(result.blocked.reason, "session_budget_exceeded")
  assertEquals(result.afterRelease, 1)
})

Deno.test("concurrent opens cannot exceed the process session budget", async () => {
  const tight: ProcessLimits = { ...limits, maxSessions: 1 }
  const tightLayer = montyHostLayer(tight).pipe(Layer.provide(wasmEngineLayer(tight)))
  const results = await Effect.runPromise(
    Effect.gen(function* () {
      const host = yield* MontyHost
      return yield* Effect.forEach(
        [1, 2, 3],
        (n) =>
          Effect.scoped(
            Effect.gen(function* () {
              const session = yield* host.open({ agentId: `agent-${n}`, stubs: [] })
              yield* Effect.sleep("20 millis")
              return session.agentId
            }),
          ).pipe(Effect.result),
        { concurrency: "unbounded" },
      )
    }).pipe(Effect.provide(tightLayer), Effect.orDie),
  )
  assertEquals(results.filter(Result.isSuccess).length, 1)
  assertEquals(results.filter(Result.isFailure).length, 2)
})
