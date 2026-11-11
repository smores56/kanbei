import { Data, Result, Schema } from "effect"
import { canonicalize } from "../evidence/canonical.ts"
import { DISCLOSURE_FIELDS, VIEW_BUDGET, ViewDocument } from "./model.ts"
import type { ViewAction, ViewDocument as ViewDocumentType } from "./model.ts"

export type ViewIssueCode =
  | "schema_invalid"
  | "budget_exceeded"
  | "unauthorized_source"
  | "disclosure_incomplete"
  | "disclosure_duplicate"
  | "disclosure_unknown"
  | "remote_reference"
  | "code_in_structural_field"
  | "anchor_removed"
  | "anchor_missing"
  | "duplicate_anchor"
  | "duplicate_row"
  | "consent_link_removed"
  | "consent_link_missing"
  | "decision_affordance_removed"
  | "decision_affordance_missing"
  | "freshness_inconsistent"
  | "disclosure_unsafe"
  | "budget_exceeded"

export class ViewIssue extends Data.TaggedClass("ViewIssue")<{
  readonly code: ViewIssueCode
  readonly detail: string
}> {}

export interface ValidationOptions {
  /** Source prefixes a view may cite; an empty list authorizes nothing. */
  readonly authorizedSources: ReadonlyArray<string>
  readonly base?: ViewDocumentType
}

const anchorId = /^anchor:[A-Za-z0-9][A-Za-z0-9._-]*$/u
const consentId = /^consent:[A-Za-z0-9][A-Za-z0-9._-]*$/u
const routeTarget = /^route:[a-z][a-z-]*$/u
const markup = /<[A-Za-z!/]|javascript\s*:|vbscript\s*:|on[a-z]+\s*=/iu
const remote = /^(?:[a-z][a-z0-9+.-]*:|\/\/)/iu

const issue = (code: ViewIssueCode, detail: string): ViewIssue => new ViewIssue({ code, detail })

export const parseViewDocument = (
  input: unknown,
): Result.Result<ViewDocumentType, ViewIssue> => {
  try {
    return Result.succeed(
      Schema.decodeUnknownSync(ViewDocument, { onExcessProperty: "error" })(input),
    )
  } catch (error) {
    return Result.fail(
      issue(
        "schema_invalid",
        error instanceof Error ? error.message.slice(0, 300) : String(error),
      ),
    )
  }
}

const budgetIssues = (document: ViewDocumentType): ReadonlyArray<ViewIssue> => {
  const issues: Array<ViewIssue> = []
  if (document.sections.length > VIEW_BUDGET.maxSections) {
    issues.push(issue("budget_exceeded", `${document.sections.length} sections`))
  }
  if (document.anchors.length > VIEW_BUDGET.maxAnchors) {
    issues.push(issue("budget_exceeded", `${document.anchors.length} anchors`))
  }
  const texts: ReadonlyArray<readonly [string, string]> = [
    ["title", document.title],
    ...document.sections.flatMap((section) => [
      [`${section.id}.title`, section.title] as const,
      ...section.rows.flatMap((row) => [
        [`${row.id}.text`, row.anchor] as const,
        ...row.cells.map((cell) => [`${row.id}.${cell.label}`, cell.text] as const),
        ...row.badges.map((badge) => [`${row.id}.badge`, badge.label] as const),
        ...row.actions.map((action) => [`${row.id}.action`, action.label] as const),
      ]),
    ]),
  ]
  for (const [field, text] of texts) {
    if (text.length > VIEW_BUDGET.maxTextChars) {
      issues.push(issue("budget_exceeded", `${field} is ${text.length} chars`))
    }
  }
  for (const section of document.sections) {
    if (section.rows.length > VIEW_BUDGET.maxRowsPerSection) {
      issues.push(issue("budget_exceeded", `${section.id} has ${section.rows.length} rows`))
    }
    for (const row of section.rows) {
      if (row.cells.length > VIEW_BUDGET.maxCellsPerRow) {
        issues.push(issue("budget_exceeded", `${row.id} has ${row.cells.length} cells`))
      }
    }
  }
  return issues
}

const sourceIssues = (
  document: ViewDocumentType,
  authorizedSources: ReadonlyArray<string>,
): ReadonlyArray<ViewIssue> =>
  document.sections.flatMap((section) =>
    section.rows.flatMap((row) => {
      const observation = row.provenance.observationId
      return authorizedSources.some((prefix) => observation.startsWith(prefix))
        ? []
        : [issue("unauthorized_source", `${row.id} cites ${observation}`)]
    })
  )

const disclosureIssues = (document: ViewDocumentType): ReadonlyArray<ViewIssue> => {
  const issues: Array<ViewIssue> = []
  for (const field of DISCLOSURE_FIELDS) {
    const matches = document.disclosure.filter((rule) => rule.field === field)
    if (matches.length === 0) {
      issues.push(issue("disclosure_incomplete", `${field} has no rule`))
    } else if (matches.length > 1) {
      issues.push(issue("disclosure_duplicate", `${field} has ${matches.length} rules`))
    }
  }
  for (const rule of document.disclosure) {
    if (!(DISCLOSURE_FIELDS as ReadonlyArray<string>).includes(rule.field)) {
      issues.push(issue("disclosure_unknown", `${rule.field} is not a disclosure field`))
    }
    // Hiding the ruling affordance or the currency signal would misrepresent the
    // view to the human; those fields may only be included.
    if (rule.mode === "hide" && (rule.field === "actions" || rule.field === "freshness")) {
      issues.push(
        issue("disclosure_unsafe", `${rule.field} cannot be hidden from the ruling surface`),
      )
    }
  }
  return issues
}

const structuralIssues = (document: ViewDocumentType): ReadonlyArray<ViewIssue> => {
  const issues: Array<ViewIssue> = []
  const anchors = [
    ...document.anchors.map((anchor) => ["anchors", anchor] as const),
    [
      "consentLink",
      document.consentLink,
    ] as const,
  ]
  for (const [field, value] of anchors) {
    const allowed = field === "consentLink" ? consentId.test(value) : anchorId.test(value)
    if (allowed) continue
    if (markup.test(value)) {
      issues.push(issue("code_in_structural_field", `${field}: ${value.slice(0, 80)}`))
    } else if (remote.test(value) || value.startsWith("//")) {
      issues.push(issue("remote_reference", `${field}: ${value.slice(0, 80)}`))
    } else {
      issues.push(issue("anchor_missing", `${field}: ${value.slice(0, 80)}`))
    }
  }
  const actions = document.sections.flatMap((section) => section.rows.flatMap((row) => row.actions))
  for (const action of actions) {
    if (routeTarget.test(action.target) || anchorId.test(action.target)) continue
    if (markup.test(action.target)) {
      issues.push(issue("code_in_structural_field", `action ${action.id}: ${action.target}`))
    } else if (remote.test(action.target) || action.target.startsWith("//")) {
      issues.push(issue("remote_reference", `action ${action.id}: ${action.target}`))
    } else {
      issues.push(issue("anchor_missing", `action ${action.id}: ${action.target}`))
    }
  }
  if (new Set(document.anchors).size !== document.anchors.length) {
    issues.push(issue("duplicate_anchor", "anchors repeat"))
  }
  const rowIds = document.sections.flatMap((section) => section.rows.map((row) => row.id))
  if (new Set(rowIds).size !== rowIds.length) {
    issues.push(issue("duplicate_row", "row ids repeat"))
  }
  return issues
}

const affordanceIssues = (
  document: ViewDocumentType,
  base: ViewDocumentType | undefined,
): ReadonlyArray<ViewIssue> => {
  const issues: Array<ViewIssue> = []
  const actions: ReadonlyArray<ViewAction> = document.sections.flatMap((section) =>
    section.rows.flatMap((row) => row.actions)
  )
  const decision = actions.find((action) => action.id === document.decisionAffordance)
  if (decision === undefined) {
    issues.push(issue("decision_affordance_missing", document.decisionAffordance))
  } else if (decision.kind !== "rule") {
    issues.push(issue("decision_affordance_missing", "the affordance is not a ruling gesture"))
  }
  if (!actions.some((action) => action.kind === "link-consent")) {
    issues.push(issue("consent_link_missing", document.consentLink))
  }
  if (base !== undefined) {
    const missing = base.anchors.filter((anchor) => !document.anchors.includes(anchor))
    for (const anchor of missing) issues.push(issue("anchor_removed", anchor))
    if (base.consentLink !== document.consentLink) {
      issues.push(issue("consent_link_removed", base.consentLink))
    }
    const previous = base.sections.flatMap((section) => section.rows.flatMap((row) => row.actions))
      .find((action) => action.id === base.decisionAffordance)
    const retained = previous === undefined
      ? true
      : actions.some((action) => action.id === previous.id)
    if (!retained) issues.push(issue("decision_affordance_removed", base.decisionAffordance))
  }
  return issues
}

const freshnessIssues = (document: ViewDocumentType): ReadonlyArray<ViewIssue> =>
  document.freshness.state === "current" && document.freshness.pending > 0
    ? [
      issue(
        "freshness_inconsistent",
        `current with ${document.freshness.pending} pending`,
      ),
    ]
    : []

/**
 * Declarative validation (`07 §4.8.5`). A view may change presentation data but
 * cannot add an origin or code, hide a field implicitly, or drop an anchor, the
 * decision affordance, or the consent link.
 */
export const validateViewDocument = (
  document: ViewDocumentType,
  options: ValidationOptions,
): Result.Result<ViewDocumentType, ReadonlyArray<ViewIssue>> => {
  const issues = [
    ...budgetIssues(document),
    ...sourceIssues(document, options.authorizedSources),
    ...disclosureIssues(document),
    ...structuralIssues(document),
    ...affordanceIssues(document, options.base),
    ...freshnessIssues(document),
  ]
  return issues.length === 0 ? Result.succeed(document) : Result.fail(issues)
}

export const issueCodes = (issues: ReadonlyArray<ViewIssue>): ReadonlyArray<ViewIssueCode> =>
  issues.map((entry) => entry.code)

/** The document as the model sees it: hidden provenance and badges are stripped. */
export const applyDisclosure = (document: ViewDocumentType): ViewDocumentType => {
  const hidden = new Set(
    document.disclosure.filter((rule) => rule.mode === "hide").map((rule) => rule.field),
  )
  return {
    ...document,
    sections: document.sections.map((section) => ({
      ...section,
      rows: section.rows.map((row) => ({
        ...row,
        provenance: hidden.has("provenance")
          ? { observationId: "", span: "", derivation: "", scope: "" }
          : row.provenance,
        badges: hidden.has("badges") ? [] : row.badges,
      })),
    })),
  }
}

/** Canonical byte size of a staged document; the caller bounds it before storing. */
export const documentBytes = (document: ViewDocumentType): number =>
  new TextEncoder().encode(canonicalize(Schema.decodeUnknownSync(Schema.Json)(document))).length
