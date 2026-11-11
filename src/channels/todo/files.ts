import { Context, Data, Effect, Option } from "effect"

export class TodoIoError extends Data.TaggedError("TodoIoError")<{
  readonly operation: "lstat" | "read" | "write"
  readonly path: string
  readonly reason: string
}> {}

export interface TodoFileStat {
  readonly kind: "file" | "directory" | "symlink" | "other"
  readonly size: number
  readonly modifiedAtMs: number
  readonly dev: number
  readonly ino: number
}

export interface TodoFile {
  readonly bytes: Uint8Array
  readonly stat: TodoFileStat
}

/** The only writable human path; there is no general filesystem capability. */
export interface TodoFiles {
  readonly lstat: (relative: string) => Effect.Effect<Option.Option<TodoFileStat>, TodoIoError>
  readonly read: (relative: string) => Effect.Effect<TodoFile, TodoIoError>
  /** Writes a sibling temp file, fsyncs, and renames over the target. */
  readonly writeAtomic: (relative: string, bytes: Uint8Array) => Effect.Effect<void, TodoIoError>
}

export const TodoFiles = Context.Service<TodoFiles>("kanbei/channel/todo/Files")
