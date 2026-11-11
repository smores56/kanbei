import { assertEquals } from "@std/assert"
import { Effect, Exit, Layer } from "effect"
import { fileLedgerStoreLayer } from "../../src/adapters/ledger-file.ts"
import { Ledger, LedgerError, ledgerLayer } from "../../src/kernel/ledger.ts"
import { withTempDir } from "../support.ts"

const EPOCH = "epoch-1"

const withLedger = <A, E>(
  directory: string,
  effect: Effect.Effect<A, E, Ledger>,
): Promise<A> =>
  Effect.runPromise(
    effect.pipe(
      Effect.provide(ledgerLayer(EPOCH).pipe(Layer.provide(fileLedgerStoreLayer(directory)))),
    ),
  )

const failure = <A, E extends LedgerError>(
  directory: string,
  effect: Effect.Effect<A, E, Ledger>,
): Promise<LedgerError> =>
  Effect.runPromise(
    effect.pipe(
      Effect.provide(ledgerLayer(EPOCH).pipe(Layer.provide(fileLedgerStoreLayer(directory)))),
      Effect.flip,
    ),
  )

const appendObserved = (value: number) =>
  Effect.gen(function* () {
    const ledger = yield* Ledger
    return yield* ledger.append({
      components: [{ type: "kb.state/observed@1.0.0", payload: { value } }],
      appender: "agent",
      principal: "agent-1",
      authorityClass: "observation",
    })
  })

Deno.test("append assigns contiguous sequences linked by predecessor hashes", async () => {
  await withTempDir("ledger", async (directory) => {
    const result = await withLedger(
      `${directory}/ledger.jsonl`,
      Effect.gen(function* () {
        const ledger = yield* Ledger
        const first = yield* appendObserved(1)
        const second = yield* appendObserved(2)
        return { first, second, head: yield* ledger.head }
      }),
    )
    assertEquals(result.first.record.seq, 1)
    assertEquals(result.second.record.seq, 2)
    assertEquals(result.second.record.prev_hash, result.first.record.record_hash)
    assertEquals(result.head.seq, 2)
    assertEquals(result.head.hash, result.second.record.record_hash)
  })
})

Deno.test("an agent cannot append an activation receipt", async () => {
  await withTempDir("ledger", async (directory) => {
    const error = await failure(
      `${directory}/ledger.jsonl`,
      Effect.gen(function* () {
        const ledger = yield* Ledger
        return yield* ledger.append({
          components: [{ type: "kb.activation/health@1.0.0", payload: { ok: true } }],
          appender: "agent",
          principal: "agent-1",
          authorityClass: "activation_receipt",
        })
      }),
    )
    assertEquals(error.code, "authority_denied")
  })
})

Deno.test("an agent cannot append the frozen consent display", async () => {
  await withTempDir("ledger", async (directory) => {
    const error = await failure(
      `${directory}/ledger.jsonl`,
      Effect.gen(function* () {
        const ledger = yield* Ledger
        return yield* ledger.append({
          components: [{ type: "kb.work/consent_display@1.0.0", payload: { forged: true } }],
          appender: "agent",
          principal: "agent-1",
          authorityClass: "coordination",
        })
      }),
    )
    assertEquals(error.code, "authority_denied")
  })
})

Deno.test("a repeated idempotency key returns the existing record", async () => {
  await withTempDir("ledger", async (directory) => {
    const result = await withLedger(
      `${directory}/ledger.jsonl`,
      Effect.gen(function* () {
        const ledger = yield* Ledger
        const request = {
          components: [{ type: "kb.state/observed@1.0.0", payload: { value: 1 } }],
          appender: "frozen" as const,
          principal: "ingest",
          authorityClass: "observation" as const,
          idempotencyKey: "observation|1",
        }
        const first = yield* ledger.append(request)
        const second = yield* ledger.append(request)
        return { first, second, head: yield* ledger.head }
      }),
    )
    assertEquals(result.first.duplicate, false)
    assertEquals(result.second.duplicate, true)
    assertEquals(result.second.record.record_hash, result.first.record.record_hash)
    assertEquals(result.head.seq, 1)
  })
})

Deno.test("unknown components round-trip unchanged", async () => {
  await withTempDir("ledger", async (directory) => {
    const payload = { z: 1, a: ["preserve", "byte-identical"], nested: { b: true } }
    const records = await withLedger(
      `${directory}/ledger.jsonl`,
      Effect.gen(function* () {
        const ledger = yield* Ledger
        yield* ledger.append({
          components: [{ type: "kb.plugin.demo/state@1.0.0", payload }],
          appender: "plugin",
          principal: "plugin-demo",
        })
        return yield* ledger.records
      }),
    )
    assertEquals(records[0]?.components[0]?.payload, payload)
  })
})

const tamper = async (
  path: string,
  edit: (row: Record<string, unknown>) => unknown,
): Promise<void> => {
  const lines = (await Deno.readTextFile(path)).split("\n").filter((line) => line !== "")
  const first = JSON.parse(lines[0] ?? "{}") as Record<string, unknown>
  const tampered = [JSON.stringify(edit(first)), ...lines.slice(1)].join("\n") + "\n"
  await Deno.writeTextFile(path, tampered)
}

Deno.test("a payload row edited on disk refuses the ledger at restart", async () => {
  await withTempDir("ledger", async (directory) => {
    const path = `${directory}/ledger.jsonl`
    await withLedger(
      path,
      Effect.gen(function* () {
        yield* appendObserved(1)
        yield* appendObserved(2)
      }),
    )
    await tamper(path, (row) => ({
      ...row,
      components: [{ type: "kb.state/observed@1.0.0", payload: { value: 99 } }],
    }))
    const error = await failure(
      path,
      Effect.gen(function* () {
        const ledger = yield* Ledger
        return yield* ledger.records
      }),
    )
    assertEquals(error.code, "hash_mismatch")
  })
})

Deno.test("an unparsable row refuses the ledger at restart", async () => {
  await withTempDir("ledger", async (directory) => {
    const path = `${directory}/ledger.jsonl`
    await withLedger(path, appendObserved(1))
    await Deno.writeTextFile(path, `${await Deno.readTextFile(path)}{"junk":true}\n`)
    const error = await failure(
      path,
      Effect.gen(function* () {
        const ledger = yield* Ledger
        return yield* ledger.head
      }),
    )
    assertEquals(error.code, "tampered_row")
  })
})

Deno.test("a sequence removed from the middle refuses as a sequence gap", async () => {
  await withTempDir("ledger", async (directory) => {
    const path = `${directory}/ledger.jsonl`
    await withLedger(
      path,
      Effect.gen(function* () {
        yield* appendObserved(1)
        yield* appendObserved(2)
        yield* appendObserved(3)
      }),
    )
    const lines = (await Deno.readTextFile(path)).split("\n").filter((line) => line !== "")
    await Deno.writeTextFile(path, `${[lines[0], lines[2]].join("\n")}\n`)
    const error = await failure(
      path,
      Effect.gen(function* () {
        const ledger = yield* Ledger
        return yield* ledger.verify
      }),
    )
    assertEquals(error.code, "sequence_gap")
  })
})

Deno.test("verify refuses a row tampered after the ledger was opened", async () => {
  await withTempDir("ledger", async (directory) => {
    const path = `${directory}/ledger.jsonl`
    await withLedger(path, appendObserved(1))
    const program = Effect.gen(function* () {
      const ledger = yield* Ledger
      const clean = yield* ledger.head
      yield* Effect.promise(() => tamper(path, (row) => ({ ...row, recorded_at_us: 42 })))
      const verified = yield* Effect.exit(ledger.verify)
      return { clean, verified }
    })
    const result = await withLedger(path, program)
    assertEquals(result.clean.seq, 1)
    assertEquals(Exit.isFailure(result.verified), true)
  })
})
