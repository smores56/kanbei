import { Clock, Context, Effect, Layer, Option, Ref, Schema } from "effect"
import { canonicalize } from "../evidence/canonical.ts"
import { Ledger } from "../kernel/ledger.ts"
import type { LedgerError } from "../kernel/ledger.ts"
import type { ViewDocument } from "./model.ts"

export const VisibilityDiff = Schema.Struct({
  added: Schema.Array(Schema.String),
  removed: Schema.Array(Schema.String),
  changed: Schema.Array(Schema.String),
})
export type VisibilityDiff = typeof VisibilityDiff.Type

export const ActivationReceipt = Schema.Struct({
  viewId: Schema.String,
  fromRevision: Schema.String,
  toRevision: Schema.String,
  turn: Schema.Number,
  appliedAt: Schema.String,
  visibility: VisibilityDiff,
})
export type ActivationReceipt = typeof ActivationReceipt.Type

export interface TurnStart {
  readonly turn: number
  readonly document: ViewDocument
  readonly applied: Option.Option<ActivationReceipt>
}

/**
 * Declarative activation is live at the next turn boundary. The running turn
 * pins the revision it started with; a staged document never swaps mid-turn.
 */
export interface ViewActivation {
  readonly stage: (document: ViewDocument) => Effect.Effect<void>
  readonly beginTurn: () => Effect.Effect<TurnStart, LedgerError>
  readonly endTurn: () => Effect.Effect<void>
  readonly current: Effect.Effect<ViewDocument>
  readonly running: Effect.Effect<Option.Option<ViewDocument>>
  readonly receipts: Effect.Effect<ReadonlyArray<ActivationReceipt>>
}

export const ViewActivation = Context.Service<ViewActivation>("kanbei/ui/ViewActivation")

interface ActivationState {
  readonly live: ViewDocument
  readonly staged: Option.Option<ViewDocument>
  readonly turn: number
  readonly running: Option.Option<ViewDocument>
  readonly receipts: ReadonlyArray<ActivationReceipt>
}

const rowSignatures = (document: ViewDocument): Readonly<Record<string, string>> =>
  Object.fromEntries(
    document.sections.flatMap((section) =>
      section.rows.map((row) =>
        [row.id, canonicalize(Schema.decodeUnknownSync(Schema.Json)(row))] as const
      )
    ),
  )

export const visibilityDiff = (previous: ViewDocument, next: ViewDocument): VisibilityDiff => {
  const before = rowSignatures(previous)
  const after = rowSignatures(next)
  return {
    added: Object.keys(after).filter((id) => before[id] === undefined),
    removed: Object.keys(before).filter((id) => after[id] === undefined),
    changed: Object.keys(after).filter((id) =>
      before[id] !== undefined && before[id] !== after[id]
    ),
  }
}

export const viewActivationLayer = (
  initial: ViewDocument,
): Layer.Layer<ViewActivation, never, Ledger> =>
  Layer.effect(
    ViewActivation,
    Effect.gen(function* () {
      const ledger = yield* Ledger
      const state = yield* Ref.make<ActivationState>({
        live: initial,
        staged: Option.none(),
        turn: 0,
        running: Option.none(),
        receipts: [],
      })

      const beginTurn: ViewActivation["beginTurn"] = () =>
        Effect.gen(function* () {
          const current = yield* Ref.get(state)
          const turn = current.turn + 1
          if (Option.isNone(current.staged)) {
            yield* Ref.set(state, {
              ...current,
              turn,
              running: Option.some(current.live),
            })
            return { turn, document: current.live, applied: Option.none() }
          }
          const staged = current.staged.value
          const now = yield* Clock.currentTimeMillis
          const receipt: ActivationReceipt = {
            viewId: staged.viewId,
            fromRevision: current.live.revision,
            toRevision: staged.revision,
            turn,
            appliedAt: new Date(now).toISOString(),
            visibility: visibilityDiff(current.live, staged),
          }
          yield* ledger.append({
            components: [{
              type: "kb.activation/view@1.0.0",
              payload: Schema.decodeUnknownSync(Schema.Json)(receipt),
            }],
            appender: "frozen",
            principal: "view-supervisor",
            authorityClass: "activation_receipt",
          })
          yield* Ref.set(state, {
            live: staged,
            staged: Option.none(),
            turn,
            running: Option.some(staged),
            receipts: [...current.receipts, receipt],
          })
          return { turn, document: staged, applied: Option.some(receipt) }
        })

      return {
        stage: (document) =>
          Ref.update(state, (current) => ({ ...current, staged: Option.some(document) })),
        beginTurn,
        endTurn: () => Ref.update(state, (current) => ({ ...current, running: Option.none() })),
        current: Ref.get(state).pipe(Effect.map((current) => current.live)),
        running: Ref.get(state).pipe(Effect.map((current) => current.running)),
        receipts: Ref.get(state).pipe(Effect.map((current) => current.receipts)),
      }
    }),
  )
