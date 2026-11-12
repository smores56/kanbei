import { Context, Data, Schema } from "effect"
import type { Effect } from "effect"

/**
 * Cross-restart inbox state. `handled` and `quarantined` keep a processed file
 * from being selected again; the sequence/dedupe sets survive a crash between
 * the ledger append and the checkpoint write.
 */
export const InboxCheckpoint = Schema.Struct({
  version: Schema.Literal(1),
  handled: Schema.Array(Schema.String),
  quarantined: Schema.Record(Schema.String, Schema.String),
  seenBatches: Schema.Array(Schema.String),
  seenItems: Schema.Array(Schema.String),
  /** Optional so a checkpoint written before host heartbeats still loads. */
  seenHeartbeats: Schema.optional(Schema.Array(Schema.String)),
  lastSequence: Schema.Record(Schema.String, Schema.Number),
})
export type InboxCheckpoint = typeof InboxCheckpoint.Type

export const emptyCheckpoint: InboxCheckpoint = {
  version: 1,
  handled: [],
  quarantined: {},
  seenBatches: [],
  seenItems: [],
  seenHeartbeats: [],
  lastSequence: {},
}

export class InboxCheckpointError extends Data.TaggedError("InboxCheckpointError")<{
  readonly operation: "load" | "save"
  readonly reason: string
}> {}

export interface InboxCheckpointStore {
  readonly load: Effect.Effect<InboxCheckpoint, InboxCheckpointError>
  readonly save: (checkpoint: InboxCheckpoint) => Effect.Effect<void, InboxCheckpointError>
}

export const InboxCheckpointStore = Context.Service<InboxCheckpointStore>(
  "kanbei/channel/inbox/CheckpointStore",
)
