import type {
  ArmId,
  ArmMetrics,
  ArmResult,
  CheckId,
  ContainmentCheck,
} from "../../../src/spikes/isolation/types.ts"

export const metrics = (overrides: Partial<ArmMetrics> = {}): ArmMetrics => ({
  coldStartMs: { p50Ms: 40, p95Ms: 55, maxMs: 60, samples: 5 },
  cpuWorkMs: 12,
  cpuWorkIterations: 2_000_000,
  ipcRoundTripMs: { p50Ms: 0.2, p95Ms: 0.5, maxMs: 1, samples: 200 },
  rssPerAgentBytes: 48 * 1024 * 1024,
  density: [
    {
      agents: 1,
      outcome: "ok",
      readyMs: 40,
      totalRssBytes: 48 * 1024 * 1024,
      perAgentRssBytes: 48 * 1024 * 1024,
      failures: 0,
    },
    {
      agents: 16,
      outcome: "ok",
      readyMs: 900,
      totalRssBytes: 768 * 1024 * 1024,
      perAgentRssBytes: 48 * 1024 * 1024,
      failures: 0,
    },
  ],
  ...overrides,
})

const passingChecks: ReadonlyArray<CheckId> = [
  "crash-isolation",
  "hang-containment",
  "oom-containment",
  "fork-bomb-containment",
  "cleanup-after-kill",
  "orphan-reaping",
  "ipc-round-trip",
  "resource-ceilings",
  "hosts-agent-workload",
]

export const checks = (
  statuses: Partial<Record<CheckId, ContainmentCheck["status"]>> = {},
): ReadonlyArray<ContainmentCheck> =>
  passingChecks.map((id) => ({ id, status: statuses[id] ?? "pass", detail: `${id} detail` }))

export const arm = (
  id: ArmId,
  overrides: Partial<ArmResult> = {},
): ArmResult => ({
  arm: id,
  status: "measured",
  metrics: metrics(),
  checks: checks(),
  operability: {
    hostDependencies: ["deno"],
    requiresRoot: false,
    supervisorLoc: 300,
    packaging: "test",
    debuggability: "test",
    limitations: [],
  },
  notes: [],
  ...overrides,
})
