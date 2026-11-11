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

Deno.test("concurrent appends serialize into one gap-free chain", async () => {
  await withTempDir("ledger", async (directory) => {
    const path = `${directory}/ledger.jsonl`
    const records = await withLedger(
      path,
      Effect.gen(function* () {
        const ledger = yield* Ledger
        yield* Effect.forEach(
          Array.from({ length: 25 }, (_, index) => index),
          (value) =>
            ledger.append({
              components: [{ type: "kb.state/observed@1.0.0", payload: { value } }],
              appender: "agent",
              principal: "agent-1",
              authorityClass: "observation",
            }),
          { concurrency: "unbounded" },
        )
        return yield* ledger.records
      }),
    )
    assertEquals(records.map((record) => record.seq), Array.from({ length: 25 }, (_, i) => i + 1))
    assertEquals(
      records.every((record, index) =>
        index === 0 || record.prev_hash === records[index - 1]?.record_hash
      ),
      true,
    )
  })
})

Deno.test("a torn final line is truncated on load and receipted instead of bricking", async () => {
  await withTempDir("ledger", async (directory) => {
    const path = `${directory}/ledger.jsonl`
    await withLedger(
      path,
      Effect.gen(function* () {
        yield* appendObserved(1)
        yield* appendObserved(2)
      }),
    )
    const complete = await Deno.readTextFile(path)
    await Deno.writeTextFile(path, `${complete}{"record_id":"torn","seq":3`)
    const result = await withLedger(
      path,
      Effect.gen(function* () {
        const ledger = yield* Ledger
        const head = yield* ledger.head
        const recovery = yield* ledger.recovery
        const appended = yield* appendObserved(3)
        return { head, recovery, appended, headAfter: yield* ledger.head }
      }),
    )
    assertEquals(result.head.seq, 2)
    assertEquals(result.recovery.length, 1)
    assertEquals(result.recovery[0]?.kind, "torn_final_line")
    assertEquals(await Deno.readTextFile(path).then((text) => text.endsWith("\n")), true)
    assertEquals(result.appended.record.seq, 3)
    assertEquals(result.headAfter.seq, 3)
    const receipts = await Deno.readTextFile(`${path}.recovery`)
    assertEquals(receipts.trim().endsWith("}"), true)
  })
})

Deno.test("a restarted ledger resumes the sequence from the durable file", async () => {
  await withTempDir("ledger", async (directory) => {
    const path = `${directory}/ledger.jsonl`
    await withLedger(path, appendObserved(1))
    const second = await withLedger(
      path,
      Effect.gen(function* () {
        const ledger = yield* Ledger
        const appended = yield* appendObserved(2)
        return { appended, head: yield* ledger.head }
      }),
    )
    assertEquals(second.appended.record.seq, 2)
    assertEquals(second.head.seq, 2)
  })
})

Deno.test("a replayed idempotency key with a different payload is refused", async () => {
  await withTempDir("ledger", async (directory) => {
    const error = await failure(
      `${directory}/ledger.jsonl`,
      Effect.gen(function* () {
        const ledger = yield* Ledger
        yield* ledger.append({
          components: [{ type: "kb.state/observed@1.0.0", payload: { value: 1 } }],
          appender: "frozen",
          principal: "ingest",
          authorityClass: "observation",
          idempotencyKey: "observation|1",
        })
        return yield* ledger.append({
          components: [{ type: "kb.state/observed@1.0.0", payload: { value: 2 } }],
          appender: "frozen",
          principal: "ingest",
          authorityClass: "observation",
          idempotencyKey: "observation|1",
        })
      }),
    )
    assertEquals(error.code, "idempotency_conflict")
  })
})

Deno.test("a second ledger on the same file refuses instead of interleaving", async () => {
  await withTempDir("ledger", async (directory) => {
    const path = `${directory}/ledger.jsonl`
    const first = await withLedger(
      path,
      Effect.gen(function* () {
        const ledger = yield* Ledger
        return yield* ledger.head
      }),
    )
    assertEquals(first.seq, 0)
    // The lock is scoped to the running ledger layer; a second concurrent layer
    // must refuse rather than share the file.
    const refused = await Effect.runPromise(
      Effect.scoped(
        Effect.gen(function* () {
          yield* Layer.build(fileLedgerStoreLayer(path))
          return yield* Effect.exit(Layer.build(fileLedgerStoreLayer(path)))
        }),
      ),
    )
    assertEquals(Exit.isFailure(refused), true)
  })
})
