import type { Clock } from "effect"

/**
 * Event-loop lag sampler.
 *
 * Instrumentation, not domain logic: it deliberately owns a mutable sample
 * buffer because a timer callback cannot return a value. The sampler reschedules
 * itself with `setTimeout`, so a lag sample reports exactly how late the host's
 * timer queue delivered work that was due.
 */
export interface LagSampler {
  readonly stop: () => ReadonlyArray<number>
}

export const startLagSampler = (clock: Clock.Clock, intervalMs: number): LagSampler => {
  const samples: number[] = []
  const intervalNs = BigInt(Math.round(intervalMs * 1_000_000))
  const state = { stopped: false }

  const schedule = (): void => {
    const dueAt = clock.monotonicTimeNanosUnsafe() + intervalNs
    setTimeout(() => {
      if (state.stopped) return
      const actual = clock.monotonicTimeNanosUnsafe()
      samples.push(Number(actual - dueAt) / 1_000_000)
      schedule()
    }, intervalMs)
  }

  schedule()
  return {
    stop: () => {
      state.stopped = true
      return [...samples]
    },
  }
}
