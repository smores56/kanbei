import { Effect, Layer, Schema } from "effect"
import {
  emptyCheckpoint,
  InboxCheckpoint,
  InboxCheckpointError,
  InboxCheckpointStore,
} from "../channels/inbox/checkpoint.ts"

const encoder = new TextEncoder()

const loadSync = (path: string): InboxCheckpoint => {
  let text: string
  try {
    text = Deno.readTextFileSync(path)
  } catch (error) {
    if (error instanceof Deno.errors.NotFound) return emptyCheckpoint
    throw error
  }
  return Schema.decodeUnknownSync(InboxCheckpoint, { onExcessProperty: "error" })(
    JSON.parse(text),
  )
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

const saveSync = (path: string, checkpoint: InboxCheckpoint): void => {
  const directory = path.slice(0, path.lastIndexOf("/"))
  if (directory !== "") Deno.mkdirSync(directory, { recursive: true })
  const temporary = `${path}.${crypto.randomUUID()}.tmp`
  const file = Deno.openSync(temporary, { write: true, createNew: true, mode: 0o600 })
  try {
    file.writeSync(encoder.encode(JSON.stringify(Schema.encodeSync(InboxCheckpoint)(checkpoint))))
    file.syncSync()
  } finally {
    file.close()
  }
  Deno.renameSync(temporary, path)
  syncDirectory(directory)
}

const error = (operation: "load" | "save", failure: unknown): InboxCheckpointError =>
  new InboxCheckpointError({
    operation,
    reason: failure instanceof Error ? failure.message : String(failure),
  })

/** One small JSON checkpoint, replaced atomically after every poll. */
export const fileInboxCheckpointLayer = (path: string): Layer.Layer<InboxCheckpointStore> =>
  Layer.succeed(InboxCheckpointStore)({
    load: Effect.try({ try: () => loadSync(path), catch: (failure) => error("load", failure) }),
    save: (checkpoint) =>
      Effect.try({
        try: () => saveSync(path, checkpoint),
        catch: (failure) => error("save", failure),
      }),
  })

/** An in-memory checkpoint for tests and dry runs. */
export const memoryInboxCheckpointLayer = (): Layer.Layer<InboxCheckpointStore> =>
  Layer.effect(
    InboxCheckpointStore,
    Effect.sync(() => {
      let current: InboxCheckpoint = emptyCheckpoint
      return {
        load: Effect.sync(() => current),
        save: (checkpoint) =>
          Effect.sync(() => {
            current = checkpoint
          }),
      }
    }),
  )
