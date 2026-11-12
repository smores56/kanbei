import type {
  ArmId,
  ArmMetrics,
  ArmResult,
  CheckId,
  ContainmentCheck,
  SupervisorInfo,
} from "../../../src/spikes/isolation/types.ts"
import { HOST_DEPENDENCIES } from "../../../src/spikes/isolation/supervisor.ts"

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

export const supervisor = (): SupervisorInfo => ({
  kind: "rootless-userns-chroot-landlock",
  uid: 1000,
  landlockAbi: 7,
  cgroupRoot: "/user.slice/user-1000.slice/user@1000.service/app.slice/run-1.service",
  delegatedBy: "systemd-run --user --property=Delegate=yes --property=DelegateSubgroup=supervisor",
  dependencies: HOST_DEPENDENCIES,
})

export const arm = (
  id: ArmId,
  overrides: Partial<ArmResult> = {},
): ArmResult => ({
  arm: id,
  status: "measured",
  scope: `${id} test scope`,
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
