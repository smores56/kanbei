import { Effect, Layer, Option, Schema } from "effect"
import { PointerError, PointerStore, ScopePointer } from "../candidates/pointer.ts"

const encoder = new TextEncoder()

const fileName = (scope: string): string => {
  const encoded = scope.replace(/[^A-Za-z0-9._-]/gu, "_")
  return encoded === "" ? "_" : encoded
}

const pathFor = (root: string, scope: string): string => `${root}/${fileName(scope)}.json`

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

const publishSync = (root: string, pointer: ScopePointer): void => {
  Deno.mkdirSync(root, { recursive: true })
  const target = pathFor(root, pointer.scope)
  const temporary = `${target}.tmp`
  const file = Deno.openSync(temporary, { write: true, create: true, truncate: true })
  try {
    file.writeSync(encoder.encode(JSON.stringify(Schema.encodeSync(ScopePointer)(pointer))))
    file.syncSync()
  } finally {
    file.close()
  }
  Deno.renameSync(temporary, target)
  syncDirectory(root)
}

const readSync = (root: string, scope: string): Option.Option<ScopePointer> => {
  try {
    const text = Deno.readTextFileSync(pathFor(root, scope))
    return Option.some(Schema.decodeUnknownSync(ScopePointer)(JSON.parse(text)))
  } catch (error) {
    if (error instanceof Deno.errors.NotFound) return Option.none()
    throw error
  }
}

/** File-backed pointer store with write-temp, fsync, rename publication. */
export const pointerFileLayer = (root: string): Layer.Layer<PointerStore> =>
  Layer.succeed(PointerStore, {
    read: (scope) =>
      Effect.try({
        try: () => readSync(root, scope),
        catch: (error) =>
          new PointerError({
            operation: "read",
            scope,
            reason: error instanceof Error ? error.message : String(error),
          }),
      }),
    publish: (pointer) =>
      Effect.try({
        try: () => publishSync(root, pointer),
        catch: (error) =>
          new PointerError({
            operation: "publish",
            scope: pointer.scope,
            reason: error instanceof Error ? error.message : String(error),
          }),
      }),
    clear: (scope) =>
      Effect.try({
        try: () => {
          try {
            Deno.removeSync(pathFor(root, scope))
          } catch (error) {
            if (!(error instanceof Deno.errors.NotFound)) throw error
          }
          syncDirectory(root)
        },
        catch: (error) =>
          new PointerError({
            operation: "publish",
            scope,
            reason: error instanceof Error ? error.message : String(error),
          }),
      }),
  })
