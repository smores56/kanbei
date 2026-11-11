import { Clock, Context, Effect, Layer, Option, Ref, Result } from "effect"
import { sha256 } from "../../evidence/digest.ts"
import type { ContentDigest } from "../../kernel/blob-sink.ts"
import { TodoError } from "./errors.ts"
import { TodoFiles } from "./files.ts"
import { TODO_FILE, validateTodoPath } from "./path.ts"

export interface TodoLimits {
  readonly maxBytes: number
  readonly maxWriteBytes: number
  readonly maxLines: number
  readonly maxLineChars: number
}

export interface TodoWriteInput {
  readonly text: string
  readonly path?: string
  readonly expectedDigest?: ContentDigest
  readonly dryRun?: boolean
}

export interface TodoReceipt {
  readonly kind: "todo_write" | "todo_read"
  readonly path: string
  readonly digest: ContentDigest
  readonly bytes: number
  readonly remainingBytes: number
  readonly applied: boolean
  readonly at: string
}

export interface TodoRead {
  readonly path: string
  readonly text: string
  readonly digest: ContentDigest
  readonly bytes: number
  readonly remainingBytes: number
}

export interface TodoWriter {
  readonly read: Effect.Effect<TodoRead, TodoError>
  readonly write: (input: TodoWriteInput) => Effect.Effect<TodoReceipt, TodoError>
  readonly receipts: Effect.Effect<ReadonlyArray<TodoReceipt>>
}

export const TodoWriter = Context.Service<TodoWriter>("kanbei/channel/todo/Writer")

const encoder = new TextEncoder()
const decoder = new TextDecoder()

const fail = (code: TodoError["code"], path: string, detail: string): TodoError =>
  new TodoError({ code, path, detail })

const remaining = (limits: TodoLimits, bytes: number): number =>
  Math.max(0, limits.maxBytes - bytes)

const limitsIssues = (limits: TodoLimits): ReadonlyArray<string> => {
  const issues: Array<string> = []
  for (const [name, value] of Object.entries(limits)) {
    if (!Number.isFinite(value) || value <= 0) issues.push(`${name} must be positive`)
  }
  return issues
}

const shapeIssue = (text: string, limits: TodoLimits): TodoError | undefined => {
  const lines = text === "" ? [] : text.split("\n")
  if (lines.length > limits.maxLines) {
    return fail("todo_line_limit", TODO_FILE, `${lines.length} lines`)
  }
  const long = lines.find((line) => [...line].length > limits.maxLineChars)
  if (long !== undefined) {
    return fail("todo_char_limit", TODO_FILE, `line is ${[...long].length} characters`)
  }
  return undefined
}

/**
 * The capped writer: validate shape and quota before touching the mount, refuse
 * symlinks and non-`todo.txt` paths, detect an external edit against the
 * caller's expected digest, and replace the file atomically.
 */
export const todoWriterLayer = (
  limits: TodoLimits,
): Layer.Layer<TodoWriter, never, TodoFiles> =>
  Layer.effect(
    TodoWriter,
    Effect.gen(function* () {
      const issues = limitsIssues(limits)
      if (issues.length > 0) {
        return yield* Effect.die(new Error(`invalid todo limits: ${issues.join("; ")}`))
      }
      const files = yield* TodoFiles
      const receipts = yield* Ref.make<ReadonlyArray<TodoReceipt>>([])
      const record = (receipt: TodoReceipt): Effect.Effect<void> =>
        Ref.update(receipts, (current) => [...current, receipt])

      const read = Effect.gen(function* () {
        const existing = yield* files.lstat(TODO_FILE).pipe(
          Effect.mapError((error) => fail("todo_io", TODO_FILE, error.reason)),
        )
        const now = yield* Clock.currentTimeMillis
        const at = new Date(now).toISOString()
        if (Option.isNone(existing)) {
          const digest = yield* sha256(encoder.encode("")).pipe(Effect.orDie)
          const receipt: TodoReceipt = {
            kind: "todo_read",
            path: TODO_FILE,
            digest,
            bytes: 0,
            remainingBytes: limits.maxBytes,
            applied: false,
            at,
          }
          yield* record(receipt)
          return { path: TODO_FILE, text: "", digest, bytes: 0, remainingBytes: limits.maxBytes }
        }
        if (existing.value.kind === "symlink") {
          return yield* Effect.fail(fail("todo_symlink", TODO_FILE, "target is a symlink"))
        }
        if (existing.value.kind !== "file") {
          return yield* Effect.fail(fail("todo_io", TODO_FILE, `kind ${existing.value.kind}`))
        }
        const file = yield* files.read(TODO_FILE).pipe(
          Effect.mapError((error) => fail("todo_io", TODO_FILE, error.reason)),
        )
        const digest = yield* sha256(file.bytes).pipe(Effect.orDie)
        const receipt: TodoReceipt = {
          kind: "todo_read",
          path: TODO_FILE,
          digest,
          bytes: file.bytes.length,
          remainingBytes: remaining(limits, file.bytes.length),
          applied: false,
          at,
        }
        yield* record(receipt)
        return {
          path: TODO_FILE,
          text: decoder.decode(file.bytes),
          digest,
          bytes: file.bytes.length,
          remainingBytes: remaining(limits, file.bytes.length),
        }
      })

      const write = (input: TodoWriteInput): Effect.Effect<TodoReceipt, TodoError> =>
        Effect.gen(function* () {
          const path = validateTodoPath(input.path ?? TODO_FILE)
          if (Result.isFailure(path)) return yield* Effect.fail(path.failure)
          const target = path.success

          const shape = shapeIssue(input.text, limits)
          if (shape !== undefined) return yield* Effect.fail(shape)
          const bytes = encoder.encode(input.text)
          if (bytes.length > limits.maxWriteBytes) {
            return yield* Effect.fail(
              fail(
                "todo_cap_exceeded",
                target,
                `${bytes.length} > per-write ${limits.maxWriteBytes}`,
              ),
            )
          }
          if (bytes.length > limits.maxBytes) {
            return yield* Effect.fail(
              fail("todo_cap_exceeded", target, `${bytes.length} > mount ${limits.maxBytes}`),
            )
          }

          const existing = yield* files.lstat(target).pipe(
            Effect.mapError((error) => fail("todo_io", target, error.reason)),
          )
          if (Option.isSome(existing) && existing.value.kind === "symlink") {
            return yield* Effect.fail(fail("todo_symlink", target, "target is a symlink"))
          }
          if (Option.isSome(existing) && existing.value.kind !== "file") {
            return yield* Effect.fail(fail("todo_io", target, `kind ${existing.value.kind}`))
          }
          if (input.expectedDigest !== undefined) {
            if (Option.isNone(existing)) {
              return yield* Effect.fail(
                fail("todo_conflict", target, "expected a file but none exists"),
              )
            }
            const current = yield* files.read(target).pipe(
              Effect.mapError((error) => fail("todo_io", target, error.reason)),
            )
            const currentDigest = yield* sha256(current.bytes).pipe(Effect.orDie)
            if (currentDigest !== input.expectedDigest) {
              return yield* Effect.fail(
                fail("todo_conflict", target, `current ${currentDigest} != expected`),
              )
            }
          }

          const digest = yield* sha256(bytes).pipe(Effect.orDie)
          const now = yield* Clock.currentTimeMillis
          const receipt: TodoReceipt = {
            kind: "todo_write",
            path: target,
            digest,
            bytes: bytes.length,
            remainingBytes: remaining(limits, bytes.length),
            applied: input.dryRun !== true,
            at: new Date(now).toISOString(),
          }
          if (input.dryRun === true) return receipt
          yield* files.writeAtomic(target, bytes).pipe(
            Effect.mapError((error) => fail("todo_io", target, error.reason)),
          )
          yield* record(receipt)
          return receipt
        })

      return { read, write, receipts: Ref.get(receipts) }
    }),
  )
