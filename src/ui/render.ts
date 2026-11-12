import { Data, Result } from "effect"
import type { Row, ViewDocument } from "./model.ts"

export const CELL_TEXT_LIMIT = 120
export const TOTAL_TEXT_LIMIT = 4000

export type RenderErrorCode =
  | "dangerous_scheme"
  | "remote_reference"
  | "invalid_target"

export class RenderError extends Data.TaggedError("RenderError")<{
  readonly code: RenderErrorCode
  readonly field: string
  readonly detail: string
}> {}

export interface ModelText {
  readonly text: string
  /** True when the source named a scheme the shell refuses to execute. */
  readonly neutralized: boolean
  readonly truncated: boolean
}

export interface RenderedView {
  /** Plain text has no parse path, so injected markup cannot execute. */
  readonly mediaType: "text/plain; charset=utf-8"
  readonly text: string
  readonly lines: ReadonlyArray<string>
  readonly neutralized: number
  readonly truncated: number
}

const dangerousScheme = /(?:javascript|vbscript|data|file|blob|about)\s*:/iu
const anyScheme = /^[a-z][a-z0-9+.-]*:/iu

const isControl = (code: number): boolean =>
  (code < 0x20 && code !== 0x09) || (code >= 0x7f && code <= 0x9f) ||
  (code >= 0x202a && code <= 0x202e)

export const sanitizeText = (raw: string): string =>
  [...raw]
    .map((character) => isControl(character.codePointAt(0) ?? 0) ? " " : character)
    .join("")
    .replace(/\s+/gu, " ")
    .trim()

export const truncate = (
  text: string,
  limit: number,
): { readonly text: string; readonly truncated: boolean } =>
  text.length <= limit ? { text, truncated: false } : {
    text: `${text.slice(0, limit)} …[truncated]`,
    truncated: true,
  }

/**
 * Model text never decodes entities, never autolinks, and never becomes markup;
 * a dangerous scheme stays literal text and is counted as neutralized.
 */
export const renderModelText = (raw: string, limit = CELL_TEXT_LIMIT): ModelText => {
  const sanitized = sanitizeText(raw)
  const result = truncate(sanitized, limit)
  return {
    text: result.text,
    neutralized: dangerousScheme.test(sanitized),
    truncated: result.truncated,
  }
}

const internalTarget = /^(?:route:[a-z][a-z-]*|anchor:[A-Za-z0-9][A-Za-z0-9._-]*)$/u
const anchorId = /^anchor:[A-Za-z0-9][A-Za-z0-9._-]*$/u
const consentId = /^consent:[A-Za-z0-9][A-Za-z0-9._-]*$/u

const checkTarget = (field: string, value: string): Result.Result<string, RenderError> => {
  if (internalTarget.test(value)) return Result.succeed(value)
  if (dangerousScheme.test(value)) {
    return Result.fail(
      new RenderError({ code: "dangerous_scheme", field, detail: value.slice(0, 80) }),
    )
  }
  if (anyScheme.test(value) || value.startsWith("//")) {
    return Result.fail(
      new RenderError({ code: "remote_reference", field, detail: value.slice(0, 80) }),
    )
  }
  return Result.fail(new RenderError({ code: "invalid_target", field, detail: value.slice(0, 80) }))
}

interface RenderState {
  readonly lines: ReadonlyArray<string>
  readonly neutralized: number
  readonly truncated: number
}

const append = (state: RenderState, raw: string, indent = ""): RenderState => {
  const text = renderModelText(raw)
  return {
    lines: [...state.lines, `${indent}${text.text}`],
    neutralized: state.neutralized + (text.neutralized ? 1 : 0),
    truncated: state.truncated + (text.truncated ? 1 : 0),
  }
}

const actionLine = (row: Row): string =>
  row.actions.map((action) => `[${action.kind}] ${action.label} (${action.id})`).join(" ")

const renderRow = (state: RenderState, row: Row): RenderState => {
  const withHeader = append(state, `- ${row.id} (${row.anchor})`)
  const withCells = row.cells.reduce<RenderState>(
    (current, cell) => append(current, `${cell.label}: ${cell.text}`, "  "),
    withHeader,
  )
  const withBadges = row.badges.length === 0 ? withCells : append(
    withCells,
    `badges: ${row.badges.map((badge) => badge.label).join(", ")}`,
    "  ",
  )
  const withActions = row.actions.length === 0 ? withBadges : append(
    withBadges,
    `actions: ${actionLine(row)}`,
    "  ",
  )
  return append(
    withActions,
    `provenance: observation=${row.provenance.observationId} span=${row.provenance.span} ` +
      `derivation=${row.provenance.derivation} scope=${row.provenance.scope}`,
    "  ",
  )
}

const structuralCheck = (document: ViewDocument): Result.Result<ViewDocument, RenderError> => {
  const targets = [
    ...document.anchors.map((anchor) => {
      if (anchorId.test(anchor)) return Result.succeed(anchor)
      return dangerousScheme.test(anchor)
        ? Result.fail(
          new RenderError({ code: "dangerous_scheme", field: "anchors", detail: anchor }),
        )
        : Result.fail(
          new RenderError({ code: "invalid_target", field: "anchors", detail: anchor }),
        )
    }),
    consentId.test(document.consentLink) ? Result.succeed(document.consentLink) : Result.fail(
      new RenderError({
        code: dangerousScheme.test(document.consentLink) ? "dangerous_scheme" : "invalid_target",
        field: "consentLink",
        detail: document.consentLink,
      }),
    ),
    ...document.sections.flatMap((section) =>
      section.rows.flatMap((row) =>
        row.actions.map((action) => checkTarget("action.target", action.target))
      )
    ),
  ]
  return targets.reduce<Result.Result<ViewDocument, RenderError>>(
    (current, check) => Result.isFailure(current) ? current : Result.map(check, () => document),
    Result.succeed(document),
  )
}

/**
 * Renders a declarative view to inert text. The output is `text/plain`: there is
 * no HTML parse path, no autolinking, and no navigation (`ZT-16`).
 */
export const renderView = (document: ViewDocument): Result.Result<RenderedView, RenderError> => {
  const checked = structuralCheck(document)
  if (Result.isFailure(checked)) return Result.fail(checked.failure)

  const header = append(
    {
      lines: [
        `${document.route} ${document.viewId} rev ${document.revision}`,
        `freshness: ${document.freshness.state} observed ${document.freshness.observedAt} ` +
        `window ${document.freshness.windowMs}ms pending ${document.freshness.pending} ` +
        `watermark ${document.freshness.watermarkSeq}`,
      ],
      neutralized: 0,
      truncated: 0,
    },
    document.title,
  )
  const body = document.sections.reduce<RenderState>(
    (current, section) =>
      section.rows.reduce<RenderState>(
        (rows, row) => renderRow(rows, row),
        append(current, `== ${section.title} ==`),
      ),
    header,
  )
  const lines = [
    body.lines[0] ?? "",
    body.lines[1] ?? "",
    `neutralized: ${body.neutralized}`,
    ...body.lines.slice(2),
  ]
  const clipped = clip(lines)
  return Result.succeed({
    mediaType: "text/plain; charset=utf-8",
    text: clipped.lines.join("\n"),
    lines: clipped.lines,
    neutralized: body.neutralized,
    truncated: body.truncated + (clipped.clipped ? 1 : 0),
  })
}

const clip = (lines: ReadonlyArray<string>): { lines: ReadonlyArray<string>; clipped: boolean } => {
  const kept: Array<string> = []
  let size = 0
  for (const line of lines) {
    if (size + line.length > TOTAL_TEXT_LIMIT) {
      return { lines: [...kept, "…[truncated view]"], clipped: true }
    }
    kept.push(line)
    size += line.length + 1
  }
  return { lines: kept, clipped: false }
}
