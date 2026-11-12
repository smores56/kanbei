import { Effect } from "effect"
import type { CorrectnessCheck } from "./schema.ts"
import type { DatabaseFactory, DatabaseLike } from "./sqlite.ts"
import { readNumber, readString } from "./sqlite.ts"

export interface SingleWriterResult {
  readonly concurrentWriterCommitted: boolean
  readonly failClosed: boolean
  readonly recoveryOk: boolean
  readonly busyBlockedEventLoop: boolean
  readonly busyBlockMs: number
}

export interface CorrectnessSuite {
  readonly checks: ReadonlyArray<CorrectnessCheck>
  readonly singleWriter: SingleWriterResult
}

const message = (error: unknown): string => (error instanceof Error ? error.message : String(error))

const withDatabase = <A>(
  factory: DatabaseFactory,
  path: string,
  body: (database: DatabaseLike) => A,
  options?: { readonly timeoutMs?: number },
): A => {
  const database = factory(path, options)
  try {
    return body(database)
  } finally {
    database.close()
  }
}

const schema = (database: DatabaseLike): void => {
  database.exec("PRAGMA journal_mode=WAL")
  database.exec("PRAGMA synchronous=FULL")
  database.exec("PRAGMA foreign_keys=ON")
  database.exec(
    "CREATE TABLE IF NOT EXISTS records (" +
      "seq INTEGER PRIMARY KEY AUTOINCREMENT, " +
      "hash TEXT NOT NULL, " +
      "body TEXT NOT NULL)",
  )
}

const check = (
  id: string,
  body: () => { readonly pass: boolean; readonly detail: string },
): CorrectnessCheck => {
  try {
    const result = body()
    return { id, pass: result.pass, detail: result.detail }
  } catch (error) {
    return { id, pass: false, detail: `threw: ${message(error)}` }
  }
}

const walAndSynchronous = (factory: DatabaseFactory, path: string): CorrectnessCheck =>
  check("wal-and-synchronous-full", () =>
    withDatabase(factory, path, (database) => {
      schema(database)
      const mode = readString(database, "PRAGMA journal_mode")
      const synchronous = readNumber(database, "PRAGMA synchronous")
      const foreignKeys = readNumber(database, "PRAGMA foreign_keys")
      const pass = mode === "wal" && synchronous === 2 && foreignKeys === 1
      return {
        pass,
        detail: `journal_mode=${mode} synchronous=${synchronous} foreign_keys=${foreignKeys}`,
      }
    }))

const foreignKeyEnforcement = (factory: DatabaseFactory, path: string): CorrectnessCheck =>
  check("foreign-key-enforcement", () =>
    withDatabase(factory, path, (database) => {
      database.exec("PRAGMA foreign_keys=ON")
      database.exec("DROP TABLE IF EXISTS child")
      database.exec("DROP TABLE IF EXISTS parent")
      database.exec("CREATE TABLE parent (id INTEGER PRIMARY KEY)")
      database.exec(
        "CREATE TABLE child (id INTEGER PRIMARY KEY, parent_id INTEGER REFERENCES parent(id))",
      )
      try {
        database.prepare("INSERT INTO child (parent_id) VALUES (?)").run(99)
        return { pass: false, detail: "orphan child row was accepted" }
      } catch (error) {
        return { pass: true, detail: `rejected: ${message(error)}` }
      }
    }))

const transactionRollback = (factory: DatabaseFactory, path: string): CorrectnessCheck =>
  check("transaction-rollback", () =>
    withDatabase(factory, path, (database) => {
      schema(database)
      database.exec("BEGIN IMMEDIATE")
      database.prepare("INSERT INTO records (hash, body) VALUES (?, ?)").run("sha256:00", "{}")
      database.exec("ROLLBACK")
      const count = readNumber(database, "SELECT count(*) FROM records")
      return { pass: count === 0, detail: `rows after rollback=${count}` }
    }))

const integerPkMonotonic = (factory: DatabaseFactory, path: string): CorrectnessCheck =>
  check("integer-pk-monotonic", () =>
    withDatabase(factory, path, (database) => {
      schema(database)
      const insert = database.prepare("INSERT INTO records (hash, body) VALUES (?, ?)")
      const sequences = Array.from({ length: 5 }, (_, index) => {
        insert.run(`sha256:0${index}`, `{"index":${index}}`)
        return readNumber(database, "SELECT last_insert_rowid()")
      })
      const pass = sequences.every((value, index) => value === index + 1)
      return { pass, detail: `sequences=${sequences.join(",")}` }
    }))

const readYourWrites = (factory: DatabaseFactory, path: string): CorrectnessCheck =>
  check("read-your-writes", () =>
    withDatabase(factory, path, (database) => {
      schema(database)
      database.prepare("INSERT INTO records (hash, body) VALUES (?, ?)").run("sha256:aa", '{"v":1}')
      const body = readString(database, "SELECT body FROM records ORDER BY seq DESC LIMIT 1")
      return { pass: body === '{"v":1}', detail: `read back ${body}` }
    }))

const walVisibleAcrossConnections = (factory: DatabaseFactory, path: string): CorrectnessCheck =>
  check("wal-visible-across-connections", () => {
    withDatabase(factory, path, (writer) => {
      schema(writer)
      writer.prepare("INSERT INTO records (hash, body) VALUES (?, ?)").run("sha256:bb", '{"v":2}')
    })
    return withDatabase(factory, path, (reader) => {
      const body = readString(reader, "SELECT body FROM records ORDER BY seq DESC LIMIT 1")
      return { pass: body === '{"v":2}', detail: `second connection read ${body}` }
    })
  })

const textRoundTrip = (factory: DatabaseFactory, path: string): CorrectnessCheck =>
  check("text-roundtrip", () =>
    withDatabase(factory, path, (database) => {
      schema(database)
      const payload = '{"unicode":"héllo-世界","nested":{"a":[1,2,3]}}'
      database.prepare("INSERT INTO records (hash, body) VALUES (?, ?)").run("sha256:cc", payload)
      const body = readString(database, "SELECT body FROM records LIMIT 1")
      return { pass: body === payload, detail: `bytes=${body.length}` }
    }))

const checkpoint = (factory: DatabaseFactory, path: string): CorrectnessCheck =>
  check("wal-checkpoint", () =>
    withDatabase(factory, path, (database) => {
      schema(database)
      database.prepare("INSERT INTO records (hash, body) VALUES (?, ?)").run("sha256:dd", "{}")
      const row = database.prepare("PRAGMA wal_checkpoint(TRUNCATE)").get()
      const values = typeof row === "object" && row !== null ? Object.values(row) : []
      const busy = typeof values[0] === "number" ? values[0] : -1
      const log = typeof values[1] === "number" ? values[1] : -1
      const checkpointed = typeof values[2] === "number" ? values[2] : -1
      return {
        pass: busy === 0 && checkpointed === log,
        detail: `busy=${busy} log=${log} checkpointed=${checkpointed}`,
      }
    }))

const integrityCheck = (factory: DatabaseFactory, path: string): CorrectnessCheck =>
  check("integrity-check", () =>
    withDatabase(factory, path, (database) => {
      const value = readString(database, "PRAGMA integrity_check")
      return { pass: value === "ok", detail: `integrity_check=${value}` }
    }))

/**
 * Single-writer probe.
 *
 * `node:sqlite` is synchronous, so a busy handler blocks the thread that owns
 * the connection. The probe therefore measures three things separately:
 * fail-closed refusal with no busy handler, recovery after the holder commits,
 * and whether a busy timeout stalls the caller's own timers (a hazard for the
 * control plane, which must not park its loop on lock contention).
 */
const singleWriterProbe = (
  factory: DatabaseFactory,
  path: string,
  now: () => number,
): SingleWriterResult => {
  const holder = factory(path)
  const contender = factory(path)
  const recovery = factory(path)
  try {
    holder.exec("PRAGMA journal_mode=WAL")
    holder.exec(
      "CREATE TABLE IF NOT EXISTS records (seq INTEGER PRIMARY KEY AUTOINCREMENT, body TEXT NOT NULL)",
    )
    holder.exec("PRAGMA busy_timeout=0")

    holder.exec("BEGIN IMMEDIATE")
    holder.prepare("INSERT INTO records (body) VALUES (?)").run("holder")

    const committed = (() => {
      try {
        contender.exec("BEGIN IMMEDIATE")
        contender.prepare("INSERT INTO records (body) VALUES (?)").run("contender")
        contender.exec("COMMIT")
        return true
      } catch {
        return false
      }
    })()

    holder.exec("COMMIT")

    const rowsAfterRefusal = readNumber(holder, "SELECT count(*) FROM records")
    const failClosed = !committed && rowsAfterRefusal === 1

    recovery.exec("PRAGMA busy_timeout=0")
    recovery.prepare("INSERT INTO records (body) VALUES (?)").run("recovery")
    const rowsAfterRecovery = readNumber(holder, "SELECT count(*) FROM records")
    const recoveryOk = rowsAfterRecovery === 2

    const blocked = factory(path, { timeoutMs: 250 })
    holder.exec("BEGIN IMMEDIATE")
    holder.prepare("INSERT INTO records (body) VALUES (?)").run("holder-2")
    // Instrumentation state, not domain state: the timer either fires during the
    // synchronous lock wait or it does not, and that is the whole measurement.
    const timerFired: { value: boolean } = { value: false }
    const timer = setTimeout(() => {
      timerFired.value = true
    }, 40)
    const startedAt = now()
    try {
      blocked.exec("BEGIN IMMEDIATE")
      blocked.exec("ROLLBACK")
    } catch {
      // Expected: the 250 ms busy handler expires while the holder keeps the lock.
    }
    const busyBlockMs = Math.round(now() - startedAt)
    clearTimeout(timer)
    blocked.close()
    holder.exec("COMMIT")

    return {
      concurrentWriterCommitted: committed,
      failClosed,
      recoveryOk,
      busyBlockedEventLoop: !timerFired.value,
      busyBlockMs,
    }
  } finally {
    holder.close()
    contender.close()
    recovery.close()
  }
}

export const runCorrectnessSuite = (
  factory: DatabaseFactory,
  directory: string,
  now: () => number,
): Effect.Effect<CorrectnessSuite> =>
  Effect.sync(() => {
    const checks = [
      walAndSynchronous(factory, `${directory}/correctness-wal.db`),
      foreignKeyEnforcement(factory, `${directory}/correctness-fk.db`),
      transactionRollback(factory, `${directory}/correctness-rollback.db`),
      integerPkMonotonic(factory, `${directory}/correctness-pk.db`),
      readYourWrites(factory, `${directory}/correctness-ryw.db`),
      walVisibleAcrossConnections(factory, `${directory}/correctness-wal2.db`),
      textRoundTrip(factory, `${directory}/correctness-text.db`),
      checkpoint(factory, `${directory}/correctness-checkpoint.db`),
      integrityCheck(factory, `${directory}/correctness-integrity.db`),
    ]
    const singleWriter = singleWriterProbe(factory, `${directory}/correctness-writer.db`, now)
    return { checks, singleWriter }
  })
