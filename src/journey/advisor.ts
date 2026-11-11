import { Schema } from "effect"

export type SourceState = "current" | "stale" | "unknown"
export type AdviceKind = "deadline" | "renewal"

export interface AdviceInput {
  readonly origin: string
  readonly resourceKey: string
  readonly text: string
  readonly state: SourceState
}

export interface Advice {
  /** `origin|resourceKey|kind`, the stable label key used by the sealed fixture. */
  readonly key: string
  readonly origin: string
  readonly resourceKey: string
  readonly kind: AdviceKind
  readonly subject: string
  readonly dueAt: string
  readonly state: SourceState
}

export const adviceKey = (origin: string, resourceKey: string, kind: AdviceKind): string =>
  `${origin}|${resourceKey}|${kind}`

const renewalWords = /renew|subscription|insurance|lease/iu
const dateInText = /\b(\d{4}-\d{2}-\d{2})(?:[T ][0-9:.]+Z?)?/u
const dateFields = ["deadline", "dueDate", "renewalDate", "start"] as const
const textFields = ["subject", "summary", "merchant", "text", "title"] as const

const asRecord = (value: unknown): Readonly<Record<string, unknown>> | undefined =>
  typeof value === "object" && value !== null && !Array.isArray(value)
    ? value as Readonly<Record<string, unknown>>
    : undefined

const stringField = (
  record: Readonly<Record<string, unknown>>,
  names: ReadonlyArray<string>,
): string | undefined => {
  for (const name of names) {
    const value = record[name]
    if (typeof value === "string" && value !== "") return value
  }
  return undefined
}

const parseBody = (text: string): Readonly<Record<string, unknown>> | undefined => {
  try {
    const decoded = Schema.decodeUnknownSync(Schema.Json)(JSON.parse(text))
    return asRecord(decoded)
  } catch {
    return undefined
  }
}

const dueAtOf = (
  record: Readonly<Record<string, unknown>>,
  text: string,
): string | undefined => {
  const declared = stringField(record, dateFields)
  if (declared !== undefined) return declared.slice(0, 10)
  return dateInText.exec(text)?.[1]
}

const withinHorizon = (dueAt: string, startIso: string, horizonDays: number): boolean => {
  const due = Date.parse(dueAt)
  const start = Date.parse(startIso)
  if (!Number.isFinite(due) || !Number.isFinite(start)) return false
  return due >= start && due <= start + horizonDays * 24 * 60 * 60 * 1000
}

/**
 * Deterministic life-admin advice: no model call, no fixture ids. Only mail,
 * calendar, finance, and phone material can become advice; photos and browser
 * history do not.
 */
export const deriveAdvice = (
  inputs: ReadonlyArray<AdviceInput>,
  options: { readonly startIso: string; readonly horizonDays: number },
): ReadonlyArray<Advice> =>
  inputs.flatMap((input) => {
    const record = parseBody(input.text)
    if (record === undefined) return []
    const subject = stringField(record, textFields)
    if (subject === undefined) return []
    const dueAt = dueAtOf(record, subject)
    if (dueAt === undefined || !withinHorizon(dueAt, options.startIso, options.horizonDays)) {
      return []
    }
    const kind: AdviceKind = renewalWords.test(subject) ? "renewal" : "deadline"
    return [{
      key: adviceKey(input.origin, input.resourceKey, kind),
      origin: input.origin,
      resourceKey: input.resourceKey,
      kind,
      subject,
      dueAt,
      state: input.state,
    }]
  }).toSorted((left, right) =>
    left.dueAt === right.dueAt
      ? left.key.localeCompare(right.key)
      : left.dueAt < right.dueAt
      ? -1
      : 1
  )

export const currentAdvice = (advice: ReadonlyArray<Advice>): ReadonlyArray<Advice> =>
  advice.filter((entry) => entry.state === "current")

export interface Agreement {
  readonly matched: number
  readonly expected: number
  readonly produced: number
  readonly precision: number
  readonly recall: number
}

/** Exact-set agreement against a hand-labelled slice; a miss is a defect. */
export const agreement = (
  expected: ReadonlySet<string>,
  produced: ReadonlySet<string>,
): Agreement => {
  const matched = [...expected].filter((key) => produced.has(key)).length
  return {
    matched,
    expected: expected.size,
    produced: produced.size,
    precision: produced.size === 0 ? (expected.size === 0 ? 1 : 0) : matched / produced.size,
    recall: expected.size === 0 ? 1 : matched / expected.size,
  }
}
