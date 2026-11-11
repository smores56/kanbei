import { assertEquals } from "@std/assert"
import { Result } from "effect"
import { Effect, Layer, Option } from "effect"
import { fileLedgerStoreLayer } from "../../src/adapters/ledger-file.ts"
import { ledgerLayer, parseRows } from "../../src/kernel/ledger.ts"
import { ViewActivation, viewActivationLayer } from "../../src/ui/activation.ts"
import { baseDocument, secondDocument } from "./support.ts"
import { withTempDir } from "../support.ts"

const services = (directory: string) =>
  viewActivationLayer(baseDocument()).pipe(
    Layer.provide(ledgerLayer("epoch-1").pipe(Layer.provide(fileLedgerStoreLayer(directory)))),
  )

Deno.test("a declarative change activates at the next turn and never mid-turn", async () => {
  await withTempDir("view-activation", async (directory) => {
    const result = await Effect.runPromise(
      Effect.gen(function* () {
        const activation = yield* ViewActivation
        const first = yield* activation.beginTurn()
        yield* activation.stage(secondDocument())
        const pinned = yield* activation.running
        const live = yield* activation.current
        const second = yield* activation.beginTurn()
        yield* activation.endTurn()
        return { first, pinned, live, second, receipts: yield* activation.receipts }
      }).pipe(Effect.provide(services(`${directory}/ledger.jsonl`))),
    )

    assertEquals(result.first.document.revision, "rev-1")
    assertEquals(Option.isSome(result.pinned), true)
    assertEquals(Option.getOrThrow(result.pinned).revision, "rev-1")
    assertEquals(result.live.revision, "rev-1")
    assertEquals(result.second.document.revision, "rev-2")
    assertEquals(Option.isSome(result.second.applied), true)
    assertEquals(Option.getOrThrow(result.second.applied).visibility.added, ["row-2"])
    assertEquals(Option.getOrThrow(result.second.applied).visibility.changed, ["row-1"])
    assertEquals(Option.getOrThrow(result.second.applied).turn, 2)
    assertEquals(result.receipts.length, 1)
  })
})

Deno.test("an activation appends exactly one frozen activation receipt", async () => {
  await withTempDir("view-activation", async (directory) => {
    await Effect.runPromise(
      Effect.gen(function* () {
        const activation = yield* ViewActivation
        yield* activation.beginTurn()
        yield* activation.stage(secondDocument())
        yield* activation.beginTurn()
        yield* activation.beginTurn()
      }).pipe(Effect.provide(services(`${directory}/ledger.jsonl`))),
    )
    const stored = await Deno.readTextFile(`${directory}/ledger.jsonl`)
    const parsed = parseRows(stored.split("\n").filter((line) => line.trim() !== ""))
    if (Result.isFailure(parsed)) throw parsed.failure
    const records = parsed.success
    const receipts = records.filter((record) =>
      record.components.some((component) => component.type === "kb.activation/view@1.0.0")
    )
    assertEquals(receipts.length, 1)
    const authority = receipts[0]?.components.find((component) =>
      component.type === "kb.core/authority@1.0.0"
    )
    assertEquals(authority?.payload, { class: "activation_receipt", principal: "view-supervisor" })
  })
})

Deno.test("a staged change with no turn boundary appends no receipt", async () => {
  await withTempDir("view-activation", async (directory) => {
    const result = await Effect.runPromise(
      Effect.gen(function* () {
        const activation = yield* ViewActivation
        yield* activation.beginTurn()
        yield* activation.stage(secondDocument())
        return { receipts: yield* activation.receipts }
      }).pipe(Effect.provide(services(`${directory}/ledger.jsonl`))),
    )
    assertEquals(result.receipts.length, 0)
    assertEquals(await Deno.stat(`${directory}/ledger.jsonl`).catch(() => null), null)
  })
})
