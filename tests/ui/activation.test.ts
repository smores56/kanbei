import { assertEquals } from "@std/assert"
import { Result } from "effect"
import { Effect, Layer, Option } from "effect"
import { fileLedgerStoreLayer } from "../../src/adapters/ledger-file.ts"
import { ledgerLayer, parseRows } from "../../src/kernel/ledger.ts"
import { ViewActivation, viewActivationLayer } from "../../src/ui/activation.ts"
import { baseDocument, secondDocument } from "./support.ts"
import { withTempDir } from "../support.ts"

const services = (directory: string) =>
  viewActivationLayer(baseDocument(), { authorizedSources: ["obs:"] }).pipe(
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

Deno.test("stage refuses an invalid document and keeps the live revision", async () => {
  await withTempDir("view-stage", async (directory) => {
    const base = baseDocument()
    const live = await Effect.runPromise(
      Effect.gen(function* () {
        const activation = yield* ViewActivation
        yield* activation.beginTurn()
        const bad = secondDocument()
        const refused = yield* Effect.flip(
          activation.stage({ ...bad, consentLink: "https://evil.invalid" }),
        )
        const unauthorizedDoc = secondDocument()
        const unauthorized = yield* Effect.flip(
          activation.stage({
            ...unauthorizedDoc,
            sections: unauthorizedDoc.sections.map((section) => ({
              id: section.id,
              title: section.title,
              rows: section.rows.map((row) => ({
                id: row.id,
                anchor: row.anchor,
                cells: row.cells,
                badges: row.badges,
                actions: row.actions,
                provenance: {
                  observationId: "other:1",
                  span: row.provenance.span,
                  derivation: row.provenance.derivation,
                  scope: row.provenance.scope,
                },
              })),
            })),
          }),
        )
        return {
          refused: refused.issues.map((issue) => issue.code),
          unauthorized: unauthorized.issues.map((issue) => issue.code),
          current: yield* activation.current,
          staged: yield* activation.beginTurn(),
        }
      }).pipe(Effect.provide(services(`${directory}/ledger.jsonl`))),
    )
    assertEquals(
      live.refused.includes("anchor_missing") || live.refused.includes("remote_reference"),
      true,
    )
    assertEquals(
      live.unauthorized.every((code) => code === "unauthorized_source"),
      true,
    )
    assertEquals(live.current.revision, base.revision)
    assertEquals(live.staged.document.revision, base.revision)
  })
})

Deno.test("stage applies disclosure rules before the turn boundary", async () => {
  await withTempDir("view-disclosure", async (directory) => {
    const hidden = secondDocument()
    const document = baseDocument({
      revision: "rev-3",
      disclosure: [
        { field: "provenance", mode: "hide" },
        { field: "badges", mode: "hide" },
        { field: "actions", mode: "include" },
        { field: "freshness", mode: "include" },
      ],
      sections: hidden.sections,
      anchors: hidden.anchors,
    })
    const staged = await Effect.runPromise(
      Effect.gen(function* () {
        const activation = yield* ViewActivation
        yield* activation.beginTurn()
        yield* activation.stage(document)
        const next = yield* activation.beginTurn()
        return next.document
      }).pipe(Effect.provide(services(`${directory}/ledger.jsonl`))),
    )
    assertEquals(staged.revision, "rev-3")
    const rows = staged.sections.flatMap((section) => section.rows)
    assertEquals(rows.every((row) => row.provenance.observationId === ""), true)
    assertEquals(rows.every((row) => row.badges.length === 0), true)
  })
})

Deno.test("stage refuses a document beyond the staged size bound", async () => {
  await withTempDir("view-size", async (directory) => {
    const row = baseDocument().sections[0]?.rows[0]
    if (row === undefined) throw new Error("no row")
    const bulky = baseDocument({
      revision: "rev-4",
      sections: [{
        id: "queue",
        title: "Queue",
        rows: Array.from({ length: 45 }, (_, index) => ({
          ...row,
          id: `row-${index + 1}`,
          anchor: `anchor:row-${index + 1}`,
          cells: Array.from({ length: 8 }, (__, cell) => ({
            label: `cell-${cell}`,
            text: "x".repeat(1_900),
          })),
        })),
      }],
      anchors: [
        "anchor:row-1",
        "anchor:consent",
        ...Array.from({ length: 45 }, (_, index) => `anchor:row-${index + 2}`),
      ],
    })
    const refusal = await Effect.runPromise(
      Effect.gen(function* () {
        const activation = yield* ViewActivation
        yield* activation.beginTurn()
        return yield* Effect.flip(activation.stage(bulky))
      }).pipe(Effect.provide(services(`${directory}/ledger.jsonl`))),
    )
    assertEquals(refusal.issues.map((issue) => issue.code).includes("budget_exceeded"), true)
  })
})
