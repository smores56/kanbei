import { Context, Data, Effect } from "effect"
import type { Option } from "effect"

export type ContentDigest = string

export class BlobSinkError extends Data.TaggedError("BlobSinkError")<{
  readonly operation: "write" | "read"
  readonly digest: string
  readonly reason: string
}> {}

/** Content-addressed storage; bytes are published before any ledger visibility. */
export interface BlobSink {
  readonly put: (bytes: Uint8Array) => Effect.Effect<ContentDigest, BlobSinkError>
  readonly read: (digest: ContentDigest) => Effect.Effect<Option.Option<Uint8Array>, BlobSinkError>
}

export const BlobSink = Context.Service<BlobSink>("kanbei/kernel/BlobSink")
