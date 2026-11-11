import { assertEquals } from "@std/assert"
import { Effect, Option, Ref } from "effect"
import type { CapabilityBinding } from "../../src/monty/capability-host.ts"
import { mediateCapabilities } from "../../src/monty/capability-host.ts"
import {
  channelCapabilityBindings,
  csrRenewBinding,
  inboxReadBinding,
  searchQueryBinding,
  todoWriteBinding,
  vaultReadBinding,
} from "../../src/monty/channel-capabilities.ts"
import { TodoError } from "../../src/channels/todo/errors.ts"
import type { TodoWriter } from "../../src/channels/todo/writer.ts"
import type { InboxReader } from "../../src/channels/inbox/reader.ts"
import type { VaultReader } from "../../src/channels/vault/reader.ts"
import type { SearchClient } from "../../src/channels/search/client.ts"
import type { CsrClient } from "../../src/channels/csr/client.ts"
import { CsrError } from "../../src/channels/csr/client.ts"

const digest = "sha256:" + "a".repeat(64)

const runBinding = (
  binding: CapabilityBinding,
  bindings: ReadonlyArray<CapabilityBinding>,
  args: ReadonlyArray<unknown>,
) =>
  Effect.runPromise(
    Effect.scoped(
      Effect.gen(function* () {
        const session = yield* mediateCapabilities({
          manifest: {
            agentId: "agent-a",
            sandboxId: "sbx-a",
            capabilities: bindings.map((entry) => entry.capability),
          },
          bindings,
        })
        const value = yield* session.invoke(binding.capability, args)
        return { value, receipts: yield* session.receipts }
      }),
    ).pipe(Effect.orDie),
  )

const vault: VaultReader = {
  read: (path) =>
    Effect.succeed({
      path,
      pathClass: "vault",
      digest,
      bytes: 2,
      content: new TextEncoder().encode("hi"),
      modifiedAt: "2026-11-11T00:00:00.000Z",
    }),
  receipts: Effect.succeed([]),
}

const inbox: InboxReader = {
  poll: Effect.succeed({ processed: 0, accepted: 0, quarantined: 0, duplicates: 0 }),
  state: Effect.succeed({
    observations: [],
    receipts: [],
    heartbeats: [{
      origin: "tuta-mail",
      sequence: 3,
      producedAt: "2026-11-11T00:00:00.000Z",
      receivedAt: "2026-11-11T00:00:01.000Z",
      coverage: { complete: true, gaps: 0, suppressed: 0 },
      state: "current",
    }],
    gaps: [],
    quarantined: [],
    securityEvents: [],
    duplicates: 0,
  }),
  sourceState: () => Effect.succeed("current"),
  heartbeat: () => Effect.succeed(Option.none()),
}

const search: SearchClient = {
  query: () =>
    Effect.succeed({
      verdict: "blocked",
      hits: [],
      reason: "dlp",
      receipt: {
        queryDigest: digest,
        queryChars: 4,
        verdict: "blocked",
        reason: Option.some("dlp"),
        resultDigest: digest,
        hits: 0,
        at: "2026-11-11T00:00:00.000Z",
      },
    }),
  receipts: Effect.succeed([]),
}

const csrFailure: CsrClient = {
  state: Effect.succeed("refused"),
  due: Effect.succeed(false),
  renew: Effect.fail(new CsrError({ code: "csr_not_due", detail: "outside window" })),
  reload: Effect.fail(new CsrError({ code: "csr_reload_failed", detail: "nothing staged" })),
  receipts: Effect.succeed([]),
  alerts: Effect.succeed([]),
}

Deno.test("the vault binding is a closure over the reader and returns a citation", async () => {
  const binding = vaultReadBinding(vault)
  const { value, receipts } = await runBinding(binding, [binding], [{ path: "notes/a.md" }])
  assertEquals(value, {
    path: "notes/a.md",
    digest,
    bytes: 2,
    modifiedAt: "2026-11-11T00:00:00.000Z",
    contentBase64: "aGk=",
  })
  assertEquals(receipts.map((receipt) => receipt.outcome), ["ok"])
})

Deno.test("the todo write binding writes through the capped writer and rejects extra fields", async () => {
  const writes = await Effect.runPromise(Ref.make<ReadonlyArray<string>>([]))
  const todo: TodoWriter = {
    read: Effect.succeed({
      path: "todo.txt",
      text: "",
      digest,
      bytes: 0,
      remainingBytes: 1_000,
    }),
    write: (input) =>
      Ref.update(writes, (current) => [...current, input.text]).pipe(
        Effect.as({
          kind: "todo_write" as const,
          path: "todo.txt",
          digest,
          bytes: input.text.length,
          remainingBytes: 1_000,
          applied: true,
          at: "2026-11-11T00:00:00.000Z",
        }),
      ),
    receipts: Effect.succeed([]),
  }
  const binding = todoWriteBinding(todo)
  const ok = await runBinding(binding, [binding], [{ text: "buy milk" }])
  assertEquals(ok.receipts.map((receipt) => receipt.outcome), ["ok"])
  assertEquals(await Effect.runPromise(Ref.get(writes)), ["buy milk"])

  const denied = await Effect.runPromise(
    Effect.scoped(
      Effect.gen(function* () {
        const session = yield* mediateCapabilities({
          manifest: {
            agentId: "agent-a",
            sandboxId: "sbx-a",
            capabilities: ["channel.todo.write"],
          },
          bindings: [binding],
        })
        return yield* Effect.flip(session.invoke("channel.todo.write", [{ text: "x", cap: 1 }]))
      }),
    ).pipe(Effect.orDie),
  )
  assertEquals(denied.code, "capability_failed")
  assertEquals(await Effect.runPromise(Ref.get(writes)), ["buy milk"])
})

Deno.test("the search binding carries the block reason, never the provider key", async () => {
  const binding = searchQueryBinding(search)
  const { value } = await runBinding(binding, [binding], [{ query: "smuggle", limit: 3 }])
  assertEquals(value, { verdict: "blocked", reason: "dlp", hits: [] })
})

Deno.test("the csr binding maps a refusal into the capability error channel", async () => {
  const binding = csrRenewBinding(csrFailure)
  const failure = await Effect.runPromise(
    Effect.scoped(
      Effect.gen(function* () {
        const session = yield* mediateCapabilities({
          manifest: {
            agentId: "agent-a",
            sandboxId: "sbx-a",
            capabilities: ["channel.csr.renew"],
          },
          bindings: [binding],
        })
        return yield* Effect.flip(session.invoke("channel.csr.renew", []))
      }),
    ).pipe(Effect.orDie),
  )
  assertEquals(failure.code, "capability_failed")
  assertEquals(failure.detail.includes("csr_not_due"), true)
})

Deno.test("the inbox binding exposes heartbeats without raw batch bytes", async () => {
  const binding = inboxReadBinding(inbox)
  const { value } = await runBinding(binding, [binding], [])
  assertEquals(value, {
    observations: 0,
    receipts: 0,
    gaps: 0,
    heartbeats: [{ origin: "tuta-mail", sequence: 3, state: "current" }],
  })
})

Deno.test("the composed binding set includes only services that exist", () => {
  const full = channelCapabilityBindings({
    inbox,
    vault,
    todo: todoWriter(),
    search,
    csr: csrFailure,
  })
  assertEquals(full.map((binding) => binding.capability), [
    "channel.inbox.read",
    "channel.vault.read",
    "channel.todo.read",
    "channel.todo.write",
    "channel.search.query",
    "channel.csr.renew",
  ])
  const minimal = channelCapabilityBindings({ inbox, vault, todo: todoWriter() })
  assertEquals(minimal.length, 4)
})

const todoWriter = (): TodoWriter => ({
  read: Effect.succeed({
    path: "todo.txt",
    text: "",
    digest,
    bytes: 0,
    remainingBytes: 1_000,
  }),
  write: () =>
    Effect.fail(new TodoError({ code: "todo_cap_exceeded", path: "todo.txt", detail: "x" })),
  receipts: Effect.succeed([]),
})
