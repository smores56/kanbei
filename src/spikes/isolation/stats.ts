import type { LatencySummary } from "./types.ts"

/** Nearest-rank percentile over an ascending array. */
export const percentile = (sorted: ReadonlyArray<number>, p: number): number => {
  if (sorted.length === 0) return 0
  const rank = Math.ceil((p / 100) * sorted.length) - 1
  const index = Math.min(sorted.length - 1, Math.max(0, rank))
  return sorted[index] ?? 0
}

export const summarize = (samplesMs: ReadonlyArray<number>): LatencySummary => {
  const sorted = [...samplesMs].toSorted((left, right) => left - right)
  return {
    p50Ms: percentile(sorted, 50),
    p95Ms: percentile(sorted, 95),
    maxMs: sorted.at(-1) ?? 0,
    samples: sorted.length,
  }
}

export const mean = (values: ReadonlyArray<number>): number =>
  values.length === 0 ? 0 : values.reduce((total, value) => total + value, 0) / values.length

export const round = (value: number, digits: number): number => {
  const factor = 10 ** digits
  return Math.round(value * factor) / factor
}
