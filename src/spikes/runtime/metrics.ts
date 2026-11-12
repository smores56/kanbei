/**
 * Distribution summaries for the S1 `NODE-DENO-1` bake-off.
 *
 * Pure functions only: the spike compares arms on exactly the same reduction so
 * that a difference in the table is a difference in the runtimes, never in the
 * statistics.
 */

export interface Summary {
  readonly count: number
  readonly min: number
  readonly max: number
  readonly mean: number
  readonly p50: number
  readonly p95: number
}

export const emptySummary: Summary = { count: 0, min: 0, max: 0, mean: 0, p50: 0, p95: 0 }

const round = (value: number, digits: number): number => {
  const factor = 10 ** digits
  return Math.round(value * factor) / factor
}

/** Linear-interpolation percentile over an ascending sample. */
export const percentile = (sorted: ReadonlyArray<number>, fraction: number): number => {
  if (sorted.length === 0) return 0
  const position = (sorted.length - 1) * Math.min(Math.max(fraction, 0), 1)
  const lower = Math.floor(position)
  const upper = Math.ceil(position)
  const lowerValue = sorted[lower] ?? 0
  const upperValue = sorted[upper] ?? lowerValue
  return lowerValue + (upperValue - lowerValue) * (position - lower)
}

export const summarize = (values: ReadonlyArray<number>, digits = 3): Summary => {
  if (values.length === 0) return emptySummary
  const sorted = [...values].toSorted((left, right) => left - right)
  const total = sorted.reduce((sum, value) => sum + value, 0)
  return {
    count: sorted.length,
    min: round(sorted[0] ?? 0, digits),
    max: round(sorted[sorted.length - 1] ?? 0, digits),
    mean: round(total / sorted.length, digits),
    p50: round(percentile(sorted, 0.5), digits),
    p95: round(percentile(sorted, 0.95), digits),
  }
}

/** Elapsed milliseconds between two monotonic nanosecond readings. */
export const elapsedMs = (startNanos: bigint, endNanos: bigint): number =>
  Number(endNanos - startNanos) / 1_000_000

export const toMilliseconds = (bytes: number): number => round(bytes / 1_048_576, 3)
