import { Result } from "effect"
import { TodoError } from "./errors.ts"

export const TODO_FILE = "todo.txt"

const invalid = (path: string, detail: string): TodoError =>
  new TodoError({ code: "todo_path_invalid", path, detail })

/** The mount is capped and single-purpose: only `todo.txt` is addressable. */
export const validateTodoPath = (path: string): Result.Result<string, TodoError> => {
  if (path === "") return Result.fail(invalid(path, "path is empty"))
  if (path.startsWith("/")) return Result.fail(invalid(path, "absolute path"))
  if (path.includes("\u0000")) return Result.fail(invalid(path, "NUL in path"))
  if (path.split("/").some((segment) => segment === ".." || segment === ".")) {
    return Result.fail(invalid(path, "parent or current segment"))
  }
  if (path !== TODO_FILE) {
    return Result.fail(invalid(path, `only ${TODO_FILE} is writable`))
  }
  return Result.succeed(TODO_FILE)
}
