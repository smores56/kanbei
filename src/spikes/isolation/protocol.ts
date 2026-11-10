/**
 * Pre-registered S3 (`ISO-1`) protocol constants.
 *
 * Every threshold here was fixed before the first measurement run. Changing one
 * invalidates the recorded result and requires a re-run, otherwise the spike
 * becomes a post-hoc rationalisation of whichever arm happened to win.
 */

/** Working concurrency target for the containment matrix. `LIM-1` owns the real ceiling. */
export const TARGET_CONCURRENCY = 16

/** Increasing agent counts for the density sweep. */
export const DENSITY_STEPS: ReadonlyArray<number> = [1, 2, 4, 8, 16, 32]

/** Extra density steps for the in-process arm, where instances are cheap. */
export const WASM_DENSITY_STEPS: ReadonlyArray<number> = [1, 2, 4, 8, 16, 32, 64]

/** Fixed CPU workload, identical shape in both arms: an integer accumulation loop. */
export const CPU_WORK_ITERATIONS = 2_000_000

/** IPC round trips per sample set. */
export const IPC_ROUND_TRIPS = 200

/** Cold-start repetitions after one warm-up run. */
export const COLD_START_SAMPLES = 5

/** Per-agent cgroup ceiling used by the process arm. */
export const AGENT_MEMORY_MAX_MIB = 128
export const AGENT_TASKS_MAX = 64
export const AGENT_CPU_QUOTA_PERCENT = 100
export const AGENT_WALL_CLOCK_SECONDS = 120

/** Ceilings used by containment probes so a probe cannot exhaust the host. */
export const OOM_PROBE_MEMORY_MAX_MIB = 64
export const FORK_BOMB_TASKS_MAX = 16
export const HANG_PROBE_TIMEOUT_MS = 1500
export const CLEANUP_TIMEOUT_MS = 5000

/** WASM instance memory cap (pages of 64 KiB). 16 pages = 1 MiB. */
export const WASM_MAX_PAGES = 16

/**
 * Pre-registered acceptance budgets. An arm is eligible only when every
 * containment check passes, it can host the real agent workload, it reaches the
 * target concurrency inside these budgets, and it needs no privileged install.
 */
export const BUDGET = {
  /** p95 cold start to first ready signal. */
  coldStartP95Ms: 500,
  /** Mean resident memory attributable to one agent at target concurrency. */
  perAgentRssMiB: 64,
  /** p95 same-host IPC round trip. */
  ipcP95Ms: 25,
  /** Maximum supervisor implementation size counted in this spike. */
  supervisorLoc: 700,
  /** Host packages the mechanism adds beyond Deno and the user systemd manager. */
  hostDependencies: 2,
} as const

export const DECISION_RULE =
  "Eligible = status measured AND every containment check pass or not-applicable AND hosts-agent-workload pass AND density at target concurrency ok AND p95 cold start <= 500 ms AND per-agent RSS <= 64 MiB AND p95 IPC <= 25 ms AND no root requirement AND host dependencies <= 2. Among eligible arms, select the lowest p95 cold start, then the lowest per-agent RSS, then the lowest p95 IPC. No eligible arm means no selection; DG-4 stays open."

export const SECURITY_STATEMENT =
  "No security property is claimed for any arm. S3 (ISO-1) measures crash and resource containment only. No arm is evidence of a credential, approval, filesystem, network, or trust-root boundary; those remain owned by the OS sandbox, broker, and capability contracts."
