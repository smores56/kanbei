import { Cause, Clock, Effect, Schema } from "effect"
import type { DigestError } from "../../evidence/errors.ts"
import type { ArmHost } from "./adapters/types.ts"
import { startLagSampler } from "./event-loop.ts"
import { buildLedgerRecords, runLedger } from "./ledger.ts"
import { elapsedMs, emptySummary, summarize, toMilliseconds } from "./metrics.ts"
import type { ArmMeasurements, CorrectnessCheck } from "./schema.ts"
import { runCorrectnessSuite } from "./sqlite-checks.ts"
import type { DatabaseLike } from "./sqlite.ts"

export const MARKER_READY = "@@READY@@"
export const MARKER_DONE = "@@DONE@@"
export const MARKER_RESULT = "@@RESULT@@"
export const MARKER_PERMISSIONS = "@@PERMISSIONS@@"
export const MARKER_SIGNAL = "@@SIGNAL-HANDLED@@"

export const armModeSchema = Schema.Literals(["boot", "full", "signal", "permissions"])
export type ArmMode = typeof armModeSchema.Type

export const armConfigSchema = Schema.Struct({
  workdir: Schema.String,
  checksDir: Schema.String,
  dbPath: Schema.String,
  warmDbPath: Schema.String,
  payloadsPath: Schema.String,
  workerPath: Schema.String,
  appends: Schema.Number,
  appendChunk: Schema.Number,
  readsPerChunk: Schema.Number,
  warmStartIterations: Schema.Number,
  loopSamplerIntervalMs: Schema.Number,
  workerRuns: Schema.Number,
})
export type ArmConfig = typeof armConfigSchema.Type

const payloadsSchema = Schema.Array(Schema.String)

const decodePayloads = (text: string): ReadonlyArray<string> =>
  Schema.decodeUnknownSync(payloadsSchema)(JSON.parse(text))

const acquireDatabase = (host: ArmHost, path: string) =>
  Effect.acquireRelease(
    Effect.sync(() => host.factory(path)),
    (database: DatabaseLike) => Effect.sync(() => database.close()),
  )

const openLedger = (database: DatabaseLike): void => {
  database.exec("PRAGMA journal_mode=WAL")
  database.exec("PRAGMA synchronous=FULL")
  database.exec("PRAGMA foreign_keys=ON")
  database.exec(
    "CREATE TABLE IF NOT EXISTS records (" +
      "seq INTEGER PRIMARY KEY AUTOINCREMENT, hash TEXT NOT NULL, body TEXT NOT NULL)",
  )
}

const boot = (host: ArmHost, config: ArmConfig) =>
  Effect.gen(function* () {
    yield* Effect.scoped(
      Effect.gen(function* () {
        const database = yield* acquireDatabase(host, config.dbPath)
        yield* Effect.sync(() => {
          openLedger(database)
          database.prepare("INSERT INTO records (hash, body) VALUES (?, ?)").run(
            "sha256:boot",
            "{}",
          )
        })
      }),
    )
    host.print(MARKER_READY)
    host.print(MARKER_DONE)
  })

/** Repeated app-layer bring-up inside one process: the warm half of cold/warm start. */
const warmStart = (host: ArmHost, config: ArmConfig) =>
  Effect.gen(function* () {
    const clock = yield* Clock.Clock
    return yield* Effect.forEach(
      Array.from({ length: config.warmStartIterations }, (_, index) => index),
      () =>
        Effect.sync(() => {
          const startedAt = clock.monotonicTimeNanosUnsafe()
          const database = host.factory(config.warmDbPath)
          try {
            openLedger(database)
            database.prepare("INSERT INTO records (hash, body) VALUES (?, ?)").run(
              "sha256:warm",
              "{}",
            )
            database.prepare("SELECT body FROM records ORDER BY seq DESC LIMIT 1").get()
          } finally {
            database.close()
          }
          return elapsedMs(startedAt, clock.monotonicTimeNanosUnsafe())
        }),
      { concurrency: 1 },
    )
  })

const full = (host: ArmHost, config: ArmConfig) =>
  Effect.gen(function* () {
    const clock = yield* Clock.Clock
    const now = (): number => Number(clock.monotonicTimeNanosUnsafe()) / 1_000_000
    const payloads = yield* Effect.sync(() =>
      decodePayloads(host.readTextFile(config.payloadsPath))
    )
    const records = yield* buildLedgerRecords(payloads, config.appends)

    host.print(MARKER_READY)

    const rssSamples = yield* Effect.forEach(
      Array.from({ length: 3 }, (_, index) => index),
      () =>
        Effect.gen(function* () {
          yield* Effect.sleep(0)
          return toMilliseconds(host.rssBytes())
        }),
      { concurrency: 1 },
    )

    const warmStartMs = yield* warmStart(host, config)

    const ledger = yield* Effect.scoped(
      Effect.gen(function* () {
        const database = yield* acquireDatabase(host, config.dbPath)
        yield* Effect.sync(() => openLedger(database))
        const sampler = startLagSampler(clock, config.loopSamplerIntervalMs)
        const outcome = yield* runLedger(database, records, {
          appends: config.appends,
          appendChunk: config.appendChunk,
          readsPerChunk: config.readsPerChunk,
        })
        const lagSamples = sampler.stop()
        const integrity = yield* Effect.sync(() => {
          const row = database.prepare("PRAGMA integrity_check").get()
          return typeof row === "object" && row !== null ? String(Object.values(row)[0]) : "unknown"
        })
        const stored = yield* Effect.sync(() => {
          const row = database.prepare("SELECT count(*) AS total FROM records").get()
          const value = typeof row === "object" && row !== null ? Object.values(row)[0] : 0
          return typeof value === "number" ? value : Number(value)
        })
        return {
          outcome,
          lagSamples,
          integrity,
          stored,
          peakRss: host.peakRssBytes(),
          rss: host.rssBytes(),
        }
      }),
    )

    const correctness = yield* runCorrectnessSuite(host.factory, config.checksDir, now)

    const worker = yield* Effect.tryPromise(() =>
      host.runWorkerProbe(config.workerPath, config.workerRuns, now)
    )

    const integrityCheck: CorrectnessCheck = {
      id: "integrity-after-load",
      pass: ledger.integrity === "ok",
      detail: `integrity_check=${ledger.integrity} rows=${ledger.stored}`,
    }
    const dataPreserved: CorrectnessCheck = {
      id: "appends-preserved",
      pass: ledger.stored >= ledger.outcome.appends,
      detail: `issued=${ledger.outcome.appends} stored=${ledger.stored}`,
    }
    const checks = [...correctness.checks, integrityCheck, dataPreserved]

    const result: ArmMeasurements = {
      runtime: host.runtime,
      runtimeVersion: host.runtimeVersion,
      sqliteVersion: host.sqliteVersion,
      boot: {
        coldReady: emptySummary,
        coldStart: emptySummary,
        warmStart: summarize(warmStartMs),
      },
      memory: {
        rssAfterBoot: summarize(rssSamples),
        rssAfterLoadBytes: ledger.rss,
        peakRssBytes: ledger.peakRss,
      },
      eventLoop: summarize(ledger.lagSamples),
      ledger: {
        appends: ledger.outcome.appends,
        appendsPerSecond: ledger.outcome.appendsPerSecond,
        append: summarize(ledger.outcome.appendMs),
        read: summarize(ledger.outcome.readMs),
        walMode: ledger.outcome.walMode,
        readYourWrites: ledger.outcome.readYourWrites,
        hashChainValid: ledger.outcome.hashChainValid,
      },
      correctness: checks,
      singleWriter: correctness.singleWriter,
      isolation: {
        workerStartupMs: worker.workerStartupMs,
        roundtrip: summarize(worker.roundtrip),
        crashObserved: worker.crashObserved,
        parentSurvivedCrash: worker.parentSurvivedCrash,
      },
    }

    host.print(`${MARKER_RESULT}${JSON.stringify(result)}`)
    host.print(MARKER_DONE)
  })

const signal = (host: ArmHost) =>
  Effect.gen(function* () {
    const clock = yield* Clock.Clock
    const now = (): number => Number(clock.monotonicTimeNanosUnsafe()) / 1_000_000
    host.installSigterm(() => {
      const handledAt = now()
      host.print(`${MARKER_SIGNAL}${JSON.stringify({ handledAt })}`)
      host.exit(0)
    })
    host.print(MARKER_READY)
    host.keepAlive()
  })

const permissions = (host: ArmHost, config: ArmConfig) =>
  Effect.gen(function* () {
    host.print(MARKER_READY)
    const probes = yield* Effect.tryPromise(() => host.permissionProbes(config.workdir))
    host.print(`${MARKER_PERMISSIONS}${JSON.stringify(probes)}`)
    host.print(MARKER_DONE)
  })

export type ArmFailure = DigestError | Cause.UnknownError

export const runArm = (
  host: ArmHost,
  mode: ArmMode,
  config: ArmConfig,
): Effect.Effect<void, ArmFailure> => {
  switch (mode) {
    case "boot":
      return boot(host, config)
    case "full":
      return full(host, config)
    case "signal":
      return signal(host)
    case "permissions":
      return permissions(host, config)
  }
}
