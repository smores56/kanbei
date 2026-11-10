import { Clock, Effect, Ref } from "effect"
import { sha256Text } from "../../evidence/digest.ts"
import type { DigestError } from "../../evidence/errors.ts"
import { elapsedMs } from "./metrics.ts"
import type { DatabaseLike } from "./sqlite.ts"

export interface LedgerConfig {
  readonly appends: number
  readonly appendChunk: number
  readonly readsPerChunk: number
}

export interface LedgerRecord {
  readonly hash: string
  readonly body: string
}

export interface LedgerOutcome {
  readonly appends: number
  readonly appendsPerSecond: number
  readonly appendMs: ReadonlyArray<number>
  readonly readMs: ReadonlyArray<number>
  readonly walMode: string
  readonly readYourWrites: boolean
  readonly hashChainValid: boolean
}

export const GENESIS = "sha256:" + "0".repeat(64)

/**
 * Builds the hash-chained payload set outside the measured region: the bake-off
 * compares durable append cost, not digest cost.
 */
export const buildLedgerRecords = (
  bodies: ReadonlyArray<string>,
  count: number,
): Effect.Effect<ReadonlyArray<LedgerRecord>, DigestError> =>
  Effect.gen(function* () {
    const previous = yield* Ref.make(GENESIS)
    return yield* Effect.forEach(
      Array.from({ length: count }, (_, index) => bodies[index % Math.max(bodies.length, 1)] ?? ""),
      (body) =>
        Effect.gen(function* () {
          const predecessor = yield* Ref.get(previous)
          const hash = yield* sha256Text(`${predecessor}${body}`)
          yield* Ref.set(previous, hash)
          return { hash, body }
        }),
      { concurrency: 1 },
    )
  })

const readField = (value: unknown, key: string): unknown =>
  typeof value === "object" && value !== null && key in value ? Reflect.get(value, key) : undefined

const verifyChain = (database: DatabaseLike): Effect.Effect<boolean, DigestError> =>
  Effect.gen(function* () {
    const rows = database.prepare("SELECT hash, body FROM records ORDER BY seq").all()
    const previous = yield* Ref.make(GENESIS)
    const results = yield* Effect.forEach(
      rows,
      (row) =>
        Effect.gen(function* () {
          const hash = readField(row, "hash")
          const body = readField(row, "body")
          const predecessor = yield* Ref.get(previous)
          const expected = yield* sha256Text(
            `${predecessor}${typeof body === "string" ? body : ""}`,
          )
          yield* Ref.set(previous, typeof hash === "string" ? hash : "")
          return expected === hash
        }),
      { concurrency: 1 },
    )
    return results.every((valid) => valid)
  })

export const runLedger = (
  database: DatabaseLike,
  records: ReadonlyArray<LedgerRecord>,
  config: LedgerConfig,
): Effect.Effect<LedgerOutcome, DigestError> =>
  Effect.gen(function* () {
    const clock = yield* Clock.Clock
    const insert = database.prepare("INSERT INTO records (hash, body) VALUES (?, ?)")
    const selectOne = database.prepare("SELECT body FROM records WHERE seq = ?")
    const selectLast = database.prepare("SELECT body FROM records ORDER BY seq DESC LIMIT 1")

    const chunkOffsets = Array.from(
      { length: Math.ceil(records.length / Math.max(config.appendChunk, 1)) },
      (_, index) => index * Math.max(config.appendChunk, 1),
    )

    const results = yield* Effect.forEach(
      chunkOffsets,
      (offset) =>
        Effect.gen(function* () {
          const timed = yield* Effect.sync(() => {
            const chunk = records.slice(offset, offset + config.appendChunk)
            const appendMs = chunk.map((record) => {
              const startedAt = clock.monotonicTimeNanosUnsafe()
              database.exec("BEGIN IMMEDIATE")
              insert.run(record.hash, record.body)
              database.exec("COMMIT")
              return elapsedMs(startedAt, clock.monotonicTimeNanosUnsafe())
            })
            const maxSeq = offset + chunk.length
            const readMs = Array.from({ length: config.readsPerChunk }, (_, index) => {
              const seq = ((index * 37) % Math.max(maxSeq, 1)) + 1
              const startedAt = clock.monotonicTimeNanosUnsafe()
              selectOne.get(seq)
              return elapsedMs(startedAt, clock.monotonicTimeNanosUnsafe())
            })
            const lastBody = selectLast.get()
            const expected = chunk[chunk.length - 1]?.body
            const readBack = readField(lastBody, "body")
            return { appendMs, readMs, readBack: readBack === expected }
          })
          // Yield to the timer queue so the lag sampler observes the loop rather
          // than only the gap between synchronous chunks.
          yield* Effect.sleep(0)
          return timed
        }),
      { concurrency: 1 },
    )

    const appendMs = results.flatMap((result) => result.appendMs)
    const readMs = results.flatMap((result) => result.readMs)
    const totalMs = appendMs.reduce((sum, value) => sum + value, 0)
    const walMode = (() => {
      const row = database.prepare("PRAGMA journal_mode").get()
      return typeof row === "object" && row !== null ? String(Object.values(row)[0]) : "unknown"
    })()

    return {
      appends: appendMs.length,
      appendsPerSecond: totalMs === 0 ? 0 : Math.round((appendMs.length / totalMs) * 1000),
      appendMs,
      readMs,
      walMode,
      readYourWrites: results.every((result) => result.readBack),
      hashChainValid: yield* verifyChain(database),
    }
  })
