import { Context, Data, Effect, Layer, Ref, Schema } from "effect"

export const QuarantineReason = Schema.Literals([
  "read_failed",
  "decrypt_failed",
  "parse_failed",
  "source_not_allowed",
  "invalid_sequence",
  "batch_too_large",
  "entry_too_large",
  "too_many_entries",
  "digest_mismatch",
  "credential_detected",
  "publish_failed",
])
export type QuarantineReason = typeof QuarantineReason.Type

/** Durable quarantine row; the detail is structural and never holds plaintext. */
export const QuarantineEntry = Schema.Struct({
  name: Schema.String,
  reason: QuarantineReason,
  detail: Schema.String,
  ciphertextDigest: Schema.optional(Schema.String),
})
export type QuarantineEntry = typeof QuarantineEntry.Type

export class InboxQuarantineError extends Data.TaggedError("InboxQuarantineError")<{
  readonly operation: "load" | "record"
  readonly reason: string
}> {}

/**
 * Quarantined names live beside the checkpoint, not in it: a lost or stale
 * checkpoint must not let an undecryptable file occupy the poll window again.
 */
export interface InboxQuarantineStore {
  readonly load: Effect.Effect<ReadonlyArray<QuarantineEntry>, InboxQuarantineError>
  readonly record: (entry: QuarantineEntry) => Effect.Effect<void, InboxQuarantineError>
}

export const InboxQuarantineStore = Context.Service<InboxQuarantineStore>(
  "kanbei/channel/inbox/QuarantineStore",
)

export const memoryInboxQuarantineLayer: Layer.Layer<InboxQuarantineStore> = Layer.effect(
  InboxQuarantineStore,
  Effect.gen(function* () {
    const entries = yield* Ref.make<ReadonlyArray<QuarantineEntry>>([])
    return {
      load: Ref.get(entries),
      record: (entry) => Ref.update(entries, (current) => [...current, entry]),
    }
  }),
)
