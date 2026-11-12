import { Context, Data, Effect } from "effect"

export class InboxDirectoryError extends Data.TaggedError("InboxDirectoryError")<{
  readonly operation: "list" | "read"
  readonly name: string
  readonly reason: string
}> {}

export interface InboxFile {
  readonly name: string
  readonly size: number
}

/** The read-only Inbox mount. The host writer owns the directory. */
export interface InboxDirectory {
  readonly list: Effect.Effect<ReadonlyArray<InboxFile>, InboxDirectoryError>
  readonly read: (name: string) => Effect.Effect<Uint8Array, InboxDirectoryError>
}

export const InboxDirectory = Context.Service<InboxDirectory>("kanbei/channel/inbox/Directory")
