export type HostCategory = "alpha" | "beta" | "gamma"

export interface HostItem {
  readonly index: number
  readonly category: HostCategory
  readonly amount: number
}

const categories: ReadonlyArray<HostCategory> = ["alpha", "beta", "gamma"]

/** Deterministic host dataset shared by every arm; the same generator is copied into each probe program. */
export const hostItem = (index: number): HostItem => ({
  index,
  category: categories[index % 3] ?? "alpha",
  amount: ((index * 37) % 100) + 1,
})

export interface FanoutGold {
  readonly checksum: number
  readonly groups: ReadonlyArray<readonly [string, number]>
  readonly top: ReadonlyArray<readonly [number, number]>
}

export const fanoutGold = (calls: number): FanoutGold => {
  const kept = Array.from({ length: calls }, (_unused, index) => hostItem(index)).filter(
    (item) => item.amount > 50,
  )
  const sums = kept.reduce<Record<string, number>>((acc, item) => {
    acc[item.category] = (acc[item.category] ?? 0) + item.amount
    return acc
  }, { alpha: 0, beta: 0, gamma: 0 })
  const top = kept
    .toSorted((a, b) => (b.amount - a.amount) || (a.index - b.index))
    .slice(0, 3)
    .map((item) => [item.index, item.amount] as const)
  return {
    checksum: kept.reduce((total, item) => total + item.amount, 0),
    groups: Object.entries(sums).toSorted(([a], [b]) => (a < b ? -1 : a > b ? 1 : 0)),
    top,
  }
}

export const FANOUT_GOLD_50 = fanoutGold(50)

export const STREAM_CHECKSUM_20 = Array.from({ length: 20 }, (_unused, index) => hostItem(index))
  .reduce((total, item) => total + item.amount, 0)

export const PROBE_CALLS: Readonly<Record<string, number>> = {
  "10": 50,
  "100": 20,
  "1000": 3,
}
