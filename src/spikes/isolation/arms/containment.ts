/**
 * Pure pass criteria for the process arm's destructive probes. Kept free of
 * Deno, cgroup, and host imports so the criteria can be unit-tested directly.
 */

/** Evidence that the orphan probes must agree on; any disagreement fails the check. */
export interface OrphanEvidence {
  readonly markerPidsBefore: number
  readonly pidsCurrentBefore: number
  readonly orphanPidPresent: boolean
  readonly orphanAliveAfter: boolean
  readonly hostCountBefore: number
  readonly hostCountAfter: number
  readonly populatedAfter: number
  readonly procsAfter: number
  readonly markerPidsAfter: number
  readonly cgroupRemoved: boolean
}

/**
 * A pass needs the child observed alive before the kill (cgroup membership and
 * marker), and every post-kill signal empty. A marker scan alone is vacuous when
 * the marker never survives exec, and `pids.current` alone says nothing about
 * what survives the kill. The host-wide process count is recorded but not gated:
 * unrelated system activity moves it between samples.
 */
export const orphanContainmentHolds = (evidence: OrphanEvidence): boolean =>
  evidence.markerPidsBefore > 0 &&
  evidence.pidsCurrentBefore >= 2 &&
  evidence.orphanPidPresent &&
  !evidence.orphanAliveAfter &&
  evidence.populatedAfter === 0 &&
  evidence.procsAfter === 0 &&
  evidence.markerPidsAfter === 0 &&
  evidence.cgroupRemoved

export interface ForkBombEvidence {
  readonly markerPidsBefore: number
  readonly pidsMaxEvents: number
  readonly populatedAfter: number
  readonly procsAfter: number
  readonly markerPidsAfter: number
}

/**
 * Peak `pids.current` is an observation only: the kernel never lets it exceed
 * `pids.max`, so comparing it to the ceiling proves nothing. The refusal event,
 * an emptied cgroup, and marker survival are the falsifiable signals.
 */
export const forkBombHolds = (evidence: ForkBombEvidence): boolean =>
  evidence.markerPidsBefore > 0 &&
  evidence.pidsMaxEvents > 0 &&
  evidence.populatedAfter === 0 &&
  evidence.procsAfter === 0 &&
  evidence.markerPidsAfter === 0
