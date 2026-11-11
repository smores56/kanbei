import { Context, Data, Effect, Layer, Ref } from "effect"

/**
 * Durable admit log for the search ceiling. The quotas belong to the VM, not to
 * a process: a restart or a second client loop must not see an empty window.
 */
export class SearchBudgetError extends Data.TaggedError("SearchBudgetError")<{
  readonly operation: "load" | "save"
  readonly reason: string
}> {}

export interface SearchBudgetStore {
  readonly load: Effect.Effect<ReadonlyArray<number>, SearchBudgetError>
  readonly save: (admittedAtMs: ReadonlyArray<number>) => Effect.Effect<void, SearchBudgetError>
}

export const SearchBudgetStore = Context.Service<SearchBudgetStore>(
  "kanbei/channel/search/BudgetStore",
)

/** In-memory store for tests and single-process spikes; production uses the file. */
export const memorySearchBudgetLayer: Layer.Layer<SearchBudgetStore> = Layer.effect(
  SearchBudgetStore,
  Effect.gen(function* () {
    const log = yield* Ref.make<ReadonlyArray<number>>([])
    return {
      load: Ref.get(log),
      save: (stamps) => Ref.set(log, [...stamps]),
    }
  }),
)
