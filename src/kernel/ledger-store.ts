import { Context, Data } from "effect"
import type { Effect } from "effect"

export class LedgerStoreError extends Data.TaggedError("LedgerStoreError")<{
  readonly operation: "append" | "read" | "truncate" | "receipt" | "lock"
  readonly path: string
  readonly reason: string
}> {}

/**
 * What a reader sees on disk: only newline-terminated lines are committed. A
 * non-empty `tail` is a writer's partial line and is never canonical.
 */
export interface LedgerRead {
  readonly lines: ReadonlyArray<string>
  readonly tail: string
  /** Byte offset where the uncommitted tail begins. */
  readonly tailOffset: number
}

/**
 * The durable append surface beneath the ledger. Writes must be durable before
 * the append returns; `truncate` drops an uncommitted tail; `receipt` records a
 * recovery event beside the chain so the repair is auditable.
 */
export interface LedgerStore {
  readonly append: (line: string) => Effect.Effect<void, LedgerStoreError>
  readonly read: Effect.Effect<LedgerRead, LedgerStoreError>
  readonly truncate: (bytes: number) => Effect.Effect<void, LedgerStoreError>
  readonly receipt: (line: string) => Effect.Effect<void, LedgerStoreError>
}

export const LedgerStore = Context.Service<LedgerStore>("kanbei/kernel/LedgerStore")
