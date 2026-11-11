import { Effect, Layer } from "effect"
import type { LedgerRead } from "../kernel/ledger-store.ts"
import { LedgerStore, LedgerStoreError } from "../kernel/ledger-store.ts"

const encoder = new TextEncoder()
const decoder = new TextDecoder()

const appendSync = (path: string, line: string): void => {
  const file = Deno.openSync(path, { append: true, create: true })
  try {
    file.writeSync(encoder.encode(`${line}\n`))
    // Durability before visibility: the append must survive a crash.
    file.syncSync()
  } finally {
    file.close()
  }
}

const readSync = (path: string): LedgerRead => {
  let bytes: Uint8Array
  try {
    bytes = Deno.readFileSync(path)
  } catch (error) {
    if (error instanceof Deno.errors.NotFound) {
      return { lines: [], tail: "", tailOffset: 0 }
    }
    throw error
  }
  const text = decoder.decode(bytes)
  const lastNewline = text.lastIndexOf("\n")
  if (lastNewline === -1) {
    return { lines: [], tail: text, tailOffset: 0 }
  }
  const committed = text.slice(0, lastNewline)
  return {
    lines: committed.split("\n"),
    tail: text.slice(lastNewline + 1),
    tailOffset: encoder.encode(committed).length + 1,
  }
}

const truncateSync = (path: string, bytes: number): void => {
  const file = Deno.openSync(path, { write: true, create: true })
  try {
    file.truncateSync(bytes)
    file.syncSync()
  } finally {
    file.close()
  }
}

/**
 * A single-writer lock. Deno exposes no `flock`, so the lock is an exclusive
 * creation: the second writer loses the race instead of interleaving appends.
 */
const acquireLockSync = (path: string): Deno.FsFile => {
  try {
    const file = Deno.openSync(path, { createNew: true, write: true })
    file.writeSync(encoder.encode(`${Deno.pid}\n`))
    file.syncSync()
    return file
  } catch (error) {
    if (error instanceof Deno.errors.AlreadyExists) {
      throw new LedgerStoreError({
        operation: "lock",
        path,
        reason: "another writer holds the ledger lock",
      })
    }
    throw error
  }
}

/** One append-only file plus its recovery receipt sidecar behind the ledger. */
export const fileLedgerStoreLayer = (path: string): Layer.Layer<LedgerStore> =>
  Layer.effect(
    LedgerStore,
    Effect.gen(function* () {
      yield* Effect.acquireRelease(
        Effect.try({
          try: () => acquireLockSync(`${path}.lock`),
          catch: (error) =>
            error instanceof LedgerStoreError ? error : new LedgerStoreError({
              operation: "lock",
              path,
              reason: error instanceof Error ? error.message : String(error),
            }),
        }).pipe(Effect.orDie),
        (held) =>
          Effect.sync(() => {
            held.close()
            try {
              Deno.removeSync(`${path}.lock`)
            } catch (error) {
              if (!(error instanceof Deno.errors.NotFound)) throw error
            }
          }),
      )
      return {
        append: (line) =>
          Effect.try({
            try: () => appendSync(path, line),
            catch: (error) =>
              new LedgerStoreError({
                operation: "append",
                path,
                reason: error instanceof Error ? error.message : String(error),
              }),
          }),
        read: Effect.try({
          try: () => readSync(path),
          catch: (error) =>
            new LedgerStoreError({
              operation: "read",
              path,
              reason: error instanceof Error ? error.message : String(error),
            }),
        }),
        truncate: (bytes) =>
          Effect.try({
            try: () => truncateSync(path, bytes),
            catch: (error) =>
              new LedgerStoreError({
                operation: "truncate",
                path,
                reason: error instanceof Error ? error.message : String(error),
              }),
          }),
        receipt: (line) =>
          Effect.try({
            try: () => appendSync(`${path}.recovery`, line),
            catch: (error) =>
              new LedgerStoreError({
                operation: "receipt",
                path,
                reason: error instanceof Error ? error.message : String(error),
              }),
          }),
      }
    }),
  )
