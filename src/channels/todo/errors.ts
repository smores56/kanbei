import { Data } from "effect"

export type TodoErrorCode =
  | "todo_cap_exceeded"
  | "todo_line_limit"
  | "todo_char_limit"
  | "todo_path_invalid"
  | "todo_symlink"
  | "todo_conflict"
  | "todo_expectation_required"
  | "todo_io"

export class TodoError extends Data.TaggedError("TodoError")<{
  readonly code: TodoErrorCode
  readonly path: string
  readonly detail: string
}> {}
