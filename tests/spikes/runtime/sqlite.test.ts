import { assert, assertEquals } from "@std/assert"
import { Effect } from "effect"
import { openSqlite } from "../../../src/spikes/runtime/adapters/sqlite-driver.ts"
import { buildLedgerRecords, runLedger } from "../../../src/spikes/runtime/ledger.ts"
import { runCorrectnessSuite } from "../../../src/spikes/runtime/sqlite-checks.ts"

const now = (): number => performance.now()

Deno.test("SQLite correctness suite passes against node:sqlite and refuses a concurrent writer", async () => {
  const directory = await Deno.makeTempDir({ prefix: "kanbei-spike-sqlite-" })
  try {
    const suite = await Effect.runPromise(runCorrectnessSuite(openSqlite, directory, now))
    const failed = suite.checks.filter((check) => !check.pass)
    assertEquals(failed, [])
    assertEquals(suite.checks.length, 9)
    assertEquals(suite.singleWriter.concurrentWriterCommitted, false)
    assertEquals(suite.singleWriter.failClosed, true)
    assertEquals(suite.singleWriter.recoveryOk, true)
  } finally {
    await Deno.remove(directory, { recursive: true })
  }
})

Deno.test("durable append workload preserves every record and the hash chain", async () => {
  const directory = await Deno.makeTempDir({ prefix: "kanbei-spike-ledger-" })
  const path = `${directory}/ledger.db`
  try {
    const records = await Effect.runPromise(buildLedgerRecords(['{"a":1}', '{"b":2}'], 25))
    const outcome = await Effect.runPromise(
      Effect.gen(function* () {
        const database = openSqlite(path)
        try {
          database.exec("PRAGMA journal_mode=WAL")
          database.exec("PRAGMA synchronous=FULL")
          database.exec(
            "CREATE TABLE records (seq INTEGER PRIMARY KEY AUTOINCREMENT, hash TEXT NOT NULL, body TEXT NOT NULL)",
          )
          return yield* runLedger(database, records, {
            appends: 25,
            appendChunk: 5,
            readsPerChunk: 2,
          })
        } finally {
          database.close()
        }
      }),
    )
    assertEquals(outcome.appends, 25)
    assertEquals(outcome.walMode, "wal")
    assertEquals(outcome.readYourWrites, true)
    assertEquals(outcome.hashChainValid, true)
    assert(outcome.appendMs.length === 25)
    assert(outcome.readMs.length === 10)
    assert(outcome.appendMs.every((value) => value >= 0))
  } finally {
    await Deno.remove(directory, { recursive: true })
  }
})

Deno.test("hash chain verification rejects a record whose body was replaced", async () => {
  const directory = await Deno.makeTempDir({ prefix: "kanbei-spike-chain-" })
  const path = `${directory}/chain.db`
  try {
    const records = await Effect.runPromise(buildLedgerRecords(['{"a":1}'], 3))
    const valid = await Effect.runPromise(
      Effect.gen(function* () {
        const database = openSqlite(path)
        try {
          database.exec(
            "CREATE TABLE records (seq INTEGER PRIMARY KEY AUTOINCREMENT, hash TEXT NOT NULL, body TEXT NOT NULL)",
          )
          return yield* runLedger(database, records, {
            appends: 3,
            appendChunk: 1,
            readsPerChunk: 1,
          })
        } finally {
          database.close()
        }
      }),
    )
    assertEquals(valid.hashChainValid, true)

    const tampered = await Effect.runPromise(
      Effect.gen(function* () {
        const database = openSqlite(path)
        try {
          database.prepare("UPDATE records SET body = ? WHERE seq = 2").run('{"a":9}')
          return yield* runLedger(database, [], { appends: 0, appendChunk: 1, readsPerChunk: 0 })
        } finally {
          database.close()
        }
      }),
    )
    assertEquals(tampered.hashChainValid, false)
  } finally {
    await Deno.remove(directory, { recursive: true })
  }
})

Deno.test("a busy timeout on a synchronous driver parks the caller's own timers", async () => {
  const directory = await Deno.makeTempDir({ prefix: "kanbei-spike-busy-" })
  try {
    const suite = await Effect.runPromise(runCorrectnessSuite(openSqlite, directory, now))
    assertEquals(suite.singleWriter.busyBlockedEventLoop, true)
    assert(suite.singleWriter.busyBlockMs >= 200)
  } finally {
    await Deno.remove(directory, { recursive: true })
  }
})
