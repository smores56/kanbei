import { Effect, Layer, Option, Ref, Schema, Semaphore } from "effect"
import { PointerError, PointerStore, ScopePointer } from "../candidates/pointer.ts"

const encoder = new TextEncoder()

/**
 * Injective scope file names. Sanitizing characters would map `a/b` and `a_b`
 * to the same pointer; base64url of the UTF-8 scope cannot collide.
 */
const fileName = (scope: string): string =>
  encoder.encode(scope).toBase64({ alphabet: "base64url", omitPadding: true }) || "_"

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
  const temporary = `${target}.${crypto.randomUUID()}.tmp`
  const file = Deno.openSync(temporary, { write: true, createNew: true, mode: 0o600 })
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

const pointerError = (
  operation: PointerError["operation"],
  scope: string,
  error: unknown,
): PointerError =>
  new PointerError({
    operation,
    scope,
    reason: error instanceof Error ? error.message : String(error),
  })

/** File-backed pointer store: one writer per scope, write-temp, fsync, rename. */
export const pointerFileLayer = (root: string): Layer.Layer<PointerStore> =>
  Layer.effect(
    PointerStore,
    Effect.gen(function* () {
      const locks = yield* Ref.make<ReadonlyMap<string, Semaphore.Semaphore>>(new Map())
      const lockFor = (scope: string): Effect.Effect<Semaphore.Semaphore> =>
        Ref.modify(locks, (current) => {
          const existing = current.get(scope)
          if (existing !== undefined) return [existing, current]
          const created = Semaphore.makeUnsafe(1)
          return [created, new Map([...current, [scope, created]])]
        })
      return {
        read: (scope) =>
          Effect.try({
            try: () => readSync(root, scope),
            catch: (error) => pointerError("read", scope, error),
          }),
        publish: (pointer) =>
          lockFor(pointer.scope).pipe(
            Effect.flatMap((permit) =>
              permit.withPermits(1)(
                Effect.try({
                  try: () => publishSync(root, pointer),
                  catch: (error) => pointerError("publish", pointer.scope, error),
                }),
              )
            ),
          ),
        clear: (scope) =>
          lockFor(scope).pipe(
            Effect.flatMap((permit) =>
              permit.withPermits(1)(
                Effect.try({
                  try: () => {
                    try {
                      Deno.removeSync(pathFor(root, scope))
                    } catch (error) {
                      if (!(error instanceof Deno.errors.NotFound)) throw error
                    }
                    syncDirectory(root)
                  },
                  catch: (error) => pointerError("publish", scope, error),
                }),
              )
            ),
          ),
      }
    }),
  )
