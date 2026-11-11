/**
 * Wire contract shared by the S3 agent and its probes. Kept separate from
 * `agent-process.ts` because that module runs the agent when imported.
 */

/** The only success reply for `WORKLOAD`; anything else leaves the check failed. */
export const WORKLOAD_OK = "WORKLOAD ok"

export const workloadReplyHolds = (reply: string): boolean => reply.trim() === WORKLOAD_OK

/**
 * Args for a long-lived child whose argv[0] is the marker.
 *
 * `exec -a <marker> sleep 300` is not portable to this host: nix coreutils-full
 * is a multicall binary that dispatches on argv[0], so a rewritten argv[0] makes
 * `sleep` abort with "unknown program". Bash keeps its own argv across exec, and
 * the script is passed as `$1` so it never needs nested quoting.
 */
export const markerArgs = (marker: string, script: string): ReadonlyArray<string> => [
  "-c",
  'exec -a "$0" bash -c "$1"',
  marker,
  script,
]

export const sleepForeverScript = (seconds: number): string => `while :; do sleep ${seconds}; done`

/** Each fork attempt becomes a marker-carrying bash that stays resident. */
export const forkBombScript = (attempts: number): string =>
  `i=0; while [ $i -lt ${attempts} ]; do ( exec -a "$0" bash -c "while :; do sleep 30; done" & ); i=$((i+1)); done; wait`
