import { Schema } from "effect"

/** Fixed routes and anchors are the stability contract (`07 §4.11.7`). */
export const VIEW_ROUTES = ["graph", "timeline", "decisions", "settings"] as const
export const RouteId = Schema.Literals(VIEW_ROUTES)
export type RouteId = typeof RouteId.Type

export const SHELL_ROUTE_PATHS = ["/graph", "/timeline", "/decisions"] as const

export const FreshnessState = Schema.Literals(["current", "stale", "unknown"])
export type FreshnessState = typeof FreshnessState.Type

export const Freshness = Schema.Struct({
  state: FreshnessState,
  observedAt: Schema.String,
  windowMs: Schema.Number,
  pending: Schema.Number,
  watermarkSeq: Schema.Number,
})
export type Freshness = typeof Freshness.Type

export const BadgeKind = Schema.Literals([
  "contested",
  "retracted",
  "suspect",
  "stale",
  "current",
  "unknown",
  "pending",
])
export type BadgeKind = typeof BadgeKind.Type

export const Badge = Schema.Struct({
  kind: BadgeKind,
  label: Schema.String,
})
export type Badge = typeof Badge.Type

/** Passive gestures only; `rule` is the one that appends authority. */
export const ActionKind = Schema.Literals(["navigate", "expand", "filter", "rule", "link-consent"])
export type ActionKind = typeof ActionKind.Type

export const ViewAction = Schema.Struct({
  id: Schema.String,
  kind: ActionKind,
  label: Schema.String,
  /** An internal route or anchor id. A view can never name an origin. */
  target: Schema.String,
})
export type ViewAction = typeof ViewAction.Type

export const Provenance = Schema.Struct({
  observationId: Schema.String,
  span: Schema.String,
  derivation: Schema.String,
  scope: Schema.String,
})
export type Provenance = typeof Provenance.Type

export const Cell = Schema.Struct({
  label: Schema.String,
  text: Schema.String,
})
export type Cell = typeof Cell.Type

export const Row = Schema.Struct({
  id: Schema.String,
  anchor: Schema.String,
  cells: Schema.Array(Cell),
  badges: Schema.Array(Badge),
  actions: Schema.Array(ViewAction),
  provenance: Provenance,
})
export type Row = typeof Row.Type

export const Section = Schema.Struct({
  id: Schema.String,
  title: Schema.String,
  rows: Schema.Array(Row),
})
export type Section = typeof Section.Type

export const DisclosureMode = Schema.Literals(["include", "hide"])
export const DisclosureRule = Schema.Struct({
  field: Schema.String,
  mode: DisclosureMode,
})
export type DisclosureRule = typeof DisclosureRule.Type

export const ViewDocument = Schema.Struct({
  schemaVersion: Schema.Literal(1),
  viewId: Schema.String,
  revision: Schema.String,
  route: RouteId,
  title: Schema.String,
  sections: Schema.Array(Section),
  anchors: Schema.Array(Schema.String),
  consentLink: Schema.String,
  decisionAffordance: Schema.String,
  disclosure: Schema.Array(DisclosureRule),
  freshness: Freshness,
})
export type ViewDocument = typeof ViewDocument.Type

/** Every disclosure field must be decided explicitly; there is no fallback. */
export const DISCLOSURE_FIELDS = ["provenance", "badges", "actions", "freshness"] as const

export const VIEW_BUDGET = {
  maxSections: 8,
  maxRowsPerSection: 50,
  maxCellsPerRow: 8,
  maxTextChars: 2000,
  maxAnchors: 64,
  /** Canonical document bytes a staged revision may occupy. */
  maxDocumentBytes: 65_536,
} as const
