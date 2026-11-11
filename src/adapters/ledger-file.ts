import { Effect, Layer } from "effect"
import { LedgerStore, LedgerStoreError } from "../kernel/ledger-store.ts"

const encoder = new TextEncoder()

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

const readSync = (path: string): ReadonlyArray<string> => {
  let text: string
  try {
    text = Deno.readTextFileSync(path)
  } catch (error) {
    if (error instanceof Deno.errors.NotFound) return []
    throw error
  }
  return text.split("\n").filter((line) => line.trim() !== "")
}

/** One append-only file behind the ledger; the domain never opens it. */
export const fileLedgerStoreLayer = (path: string): Layer.Layer<LedgerStore> =>
  Layer.succeed(LedgerStore, {
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
  })
