import { Effect, Layer, Schema } from "effect"
import { SearchBudgetError, SearchBudgetStore } from "../channels/search/budget.ts"

const StoredBudget = Schema.Struct({
  version: Schema.Literal(1),
  admittedAtMs: Schema.Array(Schema.Number),
})

const encoder = new TextEncoder()

const error = (operation: SearchBudgetError["operation"], cause: unknown): SearchBudgetError =>
  new SearchBudgetError({
    operation,
    reason: cause instanceof Error ? cause.message : String(cause),
  })

const readOnce = async (path: string): Promise<ReadonlyArray<number>> => {
  try {
    const parsed = Schema.decodeUnknownSync(StoredBudget)(
      JSON.parse(await Deno.readTextFile(path)),
    )
    return parsed.admittedAtMs.filter((stamp) => Number.isFinite(stamp) && stamp > 0)
  } catch (cause) {
    if (cause instanceof Deno.errors.NotFound) return []
    // A corrupt budget must not silently reset to zero: fail closed.
    throw cause
  }
}

const writeOnce = async (path: string, stamps: ReadonlyArray<number>): Promise<void> => {
  const temporary = `${path}.${crypto.randomUUID()}.tmp`
  const file = await Deno.open(temporary, { write: true, createNew: true, mode: 0o600 })
  try {
    await file.write(
      encoder.encode(JSON.stringify(
        Schema.encodeSync(StoredBudget)({
          version: 1,
          admittedAtMs: [...stamps],
        }),
      )),
    )
    await file.sync()
  } finally {
    file.close()
  }
  await Deno.rename(temporary, path)
}

/** File-backed admit log: write-temp, fsync, rename, so a torn write cannot reset it. */
export const searchBudgetFileLayer = (path: string): Layer.Layer<SearchBudgetStore> =>
  Layer.succeed(SearchBudgetStore)({
    load: Effect.tryPromise({
      try: () => readOnce(path),
      catch: (cause) => error("load", cause),
    }),
    save: (stamps) =>
      Effect.tryPromise({
        try: () => writeOnce(path, stamps),
        catch: (cause) => error("save", cause),
      }),
  })
