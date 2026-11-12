import { assertEquals } from "@std/assert"
import { Effect, Layer } from "effect"
import { todoFilesLayer } from "../../../src/adapters/todo-files.ts"
import type { TodoLimits, TodoReceipt, TodoWriter } from "../../../src/channels/todo/writer.ts"
import { TodoWriter as TodoWriterTag, todoWriterLayer } from "../../../src/channels/todo/writer.ts"
import type { TodoError } from "../../../src/channels/todo/errors.ts"
import { hash, withTempDir } from "../../support.ts"

const limits: TodoLimits = {
  maxBytes: 1_000,
  maxWriteBytes: 1_000,
  maxLines: 20,
  maxLineChars: 120,
}

interface Scenario {
  readonly root: string
  readonly writer: Layer.Layer<TodoWriter>
}

const scenario = <A>(
  run: (context: Scenario) => Promise<A>,
  overrides: Partial<TodoLimits> = {},
): Promise<A> =>
  withTempDir("todo", (root) => {
    const writer = todoWriterLayer({ ...limits, ...overrides }).pipe(
      Layer.provide(todoFilesLayer(root)),
    )
    return run({ root, writer })
  })

const onWriter = <A, E>(
  context: Scenario,
  effect: Effect.Effect<A, E, TodoWriter>,
): Promise<A> => Effect.runPromise(effect.pipe(Effect.provide(context.writer), Effect.orDie))

const write = (context: Scenario, input: Parameters<TodoWriter["write"]>[0]) =>
  onWriter(
    context,
    Effect.gen(function* () {
      const writer = yield* TodoWriterTag
      return yield* writer.write(input)
    }),
  )

const writeFailure = (
  context: Scenario,
  input: Parameters<TodoWriter["write"]>[0],
): Promise<TodoError> =>
  onWriter(
    context,
    Effect.gen(function* () {
      const writer = yield* TodoWriterTag
      return yield* Effect.flip(writer.write(input))
    }),
  )

const receipts = (context: Scenario): Promise<ReadonlyArray<TodoReceipt>> =>
  onWriter(
    context,
    Effect.gen(function* () {
      const writer = yield* TodoWriterTag
      return yield* writer.receipts
    }),
  )

Deno.test("a write and its read-back produce matching digests and two receipts", async () => {
  await scenario(async (context) => {
    const text = "first\nsecond\n"
    const empty = await hash("")
    const outcome = await onWriter(
      context,
      Effect.gen(function* () {
        const writer = yield* TodoWriterTag
        const receipt = yield* writer.write({ text, expectedDigest: empty })
        const read = yield* writer.read
        return { receipt, read, receipts: yield* writer.receipts }
      }),
    )
    assertEquals(await Deno.readTextFile(`${context.root}/todo.txt`), text)
    assertEquals(outcome.receipt.digest, await hash(text))
    assertEquals(outcome.read.digest, outcome.receipt.digest)
    assertEquals(outcome.receipt.applied, true)
    assertEquals(outcome.receipts.map((receipt) => receipt.kind), ["todo_write", "todo_read"])
    assertEquals(outcome.read.remainingBytes, limits.maxBytes - text.length)
  })
})

const capRefusal = (overrides: Partial<TodoLimits>) =>
  scenario(async (context) => {
    const empty = await hash("")
    const receiptCount = await onWriter(
      context,
      Effect.gen(function* () {
        const writer = yield* TodoWriterTag
        yield* writer.write({ text: "original\n", expectedDigest: empty })
        const over = yield* Effect.flip(writer.write({ text: "x".repeat(450) }))
        assertEquals(over.code, "todo_cap_exceeded")
        const written = yield* writer.receipts
        return written.filter((receipt) => receipt.kind === "todo_write").length
      }),
    )
    assertEquals(receiptCount, 1)
    assertEquals(await Deno.readTextFile(`${context.root}/todo.txt`), "original\n")
  }, overrides)

Deno.test("a write beyond the per-write or per-mount cap leaves the file unchanged", async () => {
  await capRefusal({ maxWriteBytes: 400, maxBytes: 1_000, maxLines: 1_000, maxLineChars: 1_000 })
  await capRefusal({ maxWriteBytes: 1_000, maxBytes: 400, maxLines: 1_000, maxLineChars: 1_000 })
})

Deno.test("line and character caps are refused before any write", async () => {
  await scenario(async (context) => {
    const tooManyLines = await writeFailure(context, {
      text: Array.from({ length: 25 }, (_, index) => `line ${index}`).join("\n"),
    })
    assertEquals(tooManyLines.code, "todo_line_limit")
    const tooLong = await writeFailure(context, { text: "x".repeat(130) })
    assertEquals(tooLong.code, "todo_char_limit")
    assertEquals((await receipts(context)).length, 0)
    const exists = await Deno.stat(`${context.root}/todo.txt`).catch(() => null)
    assertEquals(exists, null)
  })
})

Deno.test("a symlinked target is refused and the link target is untouched", async () => {
  await scenario(async (context) => {
    await Deno.writeTextFile(`${context.root}/outside.txt`, "outside\n")
    await Deno.symlink(`${context.root}/outside.txt`, `${context.root}/todo.txt`)
    const error = await writeFailure(context, { text: "new\n" })
    assertEquals(error.code, "todo_symlink")
    assertEquals(await Deno.readTextFile(`${context.root}/outside.txt`), "outside\n")
    assertEquals((await receipts(context)).length, 0)
  })
})

Deno.test("a path outside the todo mount is refused", async () => {
  await scenario(async (context) => {
    const errors = await Promise.all(
      ["../todo.txt", "/todo.txt", "notes/todo.txt", ""].map((path) =>
        writeFailure(context, { path, text: "new\n" })
      ),
    )
    for (const error of errors) assertEquals(error.code, "todo_path_invalid")
    assertEquals((await receipts(context)).length, 0)
  })
})

Deno.test("an external edit is detected against the expected digest", async () => {
  await scenario(async (context) => {
    await write(context, { text: "planned\n", expectedDigest: await hash("") })
    const original = await onWriter(
      context,
      Effect.gen(function* () {
        const writer = yield* TodoWriterTag
        return yield* writer.read
      }),
    )
    await Deno.writeTextFile(`${context.root}/todo.txt`, "owner edit\n")
    const error = await writeFailure(context, {
      text: "agent edit\n",
      expectedDigest: original.digest,
    })
    assertEquals(error.code, "todo_conflict")
    assertEquals(await Deno.readTextFile(`${context.root}/todo.txt`), "owner edit\n")
  })
})

Deno.test("a dry run reports the would-be digest without writing", async () => {
  await scenario(async (context) => {
    const receipt = await write(context, { text: "draft\n", dryRun: true })
    assertEquals(receipt.applied, false)
    assertEquals(receipt.digest, await hash("draft\n"))
    assertEquals(receipt.remainingBytes, limits.maxBytes - 6)
    const exists = await Deno.stat(`${context.root}/todo.txt`).catch(() => null)
    assertEquals(exists, null)
  })
})

Deno.test("an atomic replace leaves no temporary file behind", async () => {
  await scenario(async (context) => {
    const empty = await hash("")
    const one = await write(context, { text: "one\n", expectedDigest: empty })
    await write(context, { text: "two\n", expectedDigest: one.digest })
    const names = [...Deno.readDirSync(context.root)].map((entry) => entry.name).toSorted()
    assertEquals(names, ["todo.txt"])
    assertEquals(await Deno.readTextFile(`${context.root}/todo.txt`), "two\n")
  })
})

Deno.test("an applied write without an expected digest is refused", async () => {
  await scenario(async (context) => {
    const error = await writeFailure(context, { text: "blind\n" })
    assertEquals(error.code, "todo_expectation_required")
    const exists = await Deno.stat(`${context.root}/todo.txt`).catch(() => null)
    assertEquals(exists, null)
    assertEquals((await receipts(context)).length, 0)
  })
})

Deno.test("a create is allowed against the empty digest but not against a guess", async () => {
  await scenario(async (context) => {
    const empty = await hash("")
    const refused = await writeFailure(context, {
      text: "first\n",
      expectedDigest: await hash("something else"),
    })
    assertEquals(refused.code, "todo_conflict")
    const created = await write(context, { text: "first\n", expectedDigest: empty })
    assertEquals(created.applied, true)
    const replaced = await write(context, { text: "second\n", expectedDigest: created.digest })
    assertEquals(replaced.applied, true)
    assertEquals(await Deno.readTextFile(`${context.root}/todo.txt`), "second\n")
  })
})
