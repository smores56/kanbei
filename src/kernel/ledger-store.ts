import { Context, Data } from "effect"
import type { Effect } from "effect"

export class LedgerStoreError extends Data.TaggedError("LedgerStoreError")<{
  readonly operation: "append" | "read"
  readonly path: string
  readonly reason: string
}> {}

/**
 * The durable append surface beneath the ledger. Writes must be durable before
 * the append returns; reads return the stored lines in write order.
 */
export interface LedgerStore {
  readonly append: (line: string) => Effect.Effect<void, LedgerStoreError>
  readonly read: Effect.Effect<ReadonlyArray<string>, LedgerStoreError>
}

export const LedgerStore = Context.Service<LedgerStore>("kanbei/kernel/LedgerStore")
