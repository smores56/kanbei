import { Effect, Layer, Schema } from "effect"
import {
  InboxQuarantineError,
  InboxQuarantineStore,
  QuarantineEntry,
} from "../channels/inbox/quarantine.ts"

const StoredQuarantine = Schema.Struct({
  version: Schema.Literal(1),
  entries: Schema.Array(QuarantineEntry),
})

const encoder = new TextEncoder()

const loadSync = (path: string): ReadonlyArray<QuarantineEntry> => {
  try {
    const parsed = Schema.decodeUnknownSync(StoredQuarantine, { onExcessProperty: "error" })(
      JSON.parse(Deno.readTextFileSync(path)),
    )
    return parsed.entries
  } catch (error) {
    if (error instanceof Deno.errors.NotFound) return []
    throw error
  }
}

const syncDirectory = (directory: string): void => {
  try {
    const handle = Deno.openSync(directory, { read: true })
    try {
      handle.syncSync()
    } finally {
      handle.close()
    }
  } catch {
    // Not every filesystem exposes a directory handle; the rename is still atomic.
  }
}

const saveSync = (path: string, entries: ReadonlyArray<QuarantineEntry>): void => {
  const directory = path.slice(0, path.lastIndexOf("/"))
  if (directory !== "") Deno.mkdirSync(directory, { recursive: true })
  const temporary = `${path}.${crypto.randomUUID()}.tmp`
  const file = Deno.openSync(temporary, { write: true, createNew: true, mode: 0o600 })
  try {
    file.writeSync(
      encoder.encode(
        JSON.stringify(
          Schema.encodeSync(StoredQuarantine)({ version: 1, entries: [...entries] }),
        ),
      ),
    )
    file.syncSync()
  } finally {
    file.close()
  }
  Deno.renameSync(temporary, path)
  syncDirectory(directory)
}

const error = (operation: "load" | "record", failure: unknown): InboxQuarantineError =>
  new InboxQuarantineError({
    operation,
    reason: failure instanceof Error ? failure.message : String(failure),
  })

/**
 * Append-only quarantine sidecar next to the checkpoint. The whole file is
 * replaced atomically, so a crash cannot truncate the set of dead files.
 */
export const fileInboxQuarantineLayer = (
  path: string,
): Layer.Layer<InboxQuarantineStore> =>
  Layer.effect(
    InboxQuarantineStore,
    Effect.gen(function* () {
      // A corrupt sidecar fails boot: re-admitting buried files is the unsafe direction.
      const entries = yield* Effect.try({
        try: () => [...loadSync(path)],
        catch: (failure) => error("load", failure),
      }).pipe(Effect.orDie)
      return {
        load: Effect.sync(() => [...entries]),
        record: (entry) =>
          Effect.try({
            try: () => {
              entries.push(entry)
              saveSync(path, entries)
            },
            catch: (failure) => error("record", failure),
          }).pipe(Effect.orDie),
      }
    }),
  )
