import { DENSITY_STEPS } from "./protocol.ts"
import type { ArmResult, CheckStatus, LatencySummary, SpikeResult } from "./types.ts"

const mib = (bytes: number): string => (bytes / (1024 * 1024)).toFixed(1)

const fixed = (value: number, digits: number): string => value.toFixed(digits)

const latency = (summary: LatencySummary): string =>
  `p50 ${fixed(summary.p50Ms, 1)} ms / p95 ${fixed(summary.p95Ms, 1)} ms / max ${
    fixed(summary.maxMs, 1)
  } ms (n=${summary.samples})`

const table = (
  header: ReadonlyArray<string>,
  rows: ReadonlyArray<ReadonlyArray<string>>,
): string => {
  const widths = header.map((title, index) =>
    Math.max(title.length, ...rows.map((row) => (row[index] ?? "").length))
  )
  const row = (cells: ReadonlyArray<string>): string =>
    `| ${cells.map((cell, index) => cell.padEnd(widths[index] ?? 0)).join(" | ")} |`
  return [
    row(header),
    `| ${widths.map((width) => "-".repeat(width)).join(" | ")} |`,
    ...rows.map(row),
  ].join("\n")
}

const statusCell = (status: CheckStatus): string => status === "not-applicable" ? "n/a" : status

const armSection = (arm: ArmResult): ReadonlyArray<string> => {
  const metrics = arm.metrics
  if (arm.status !== "measured" || metrics === undefined) {
    return [
      `### ${arm.arm}`,
      "",
      `Status: **${arm.status}**. ${arm.blockedReason ?? "No blocking reason recorded."}`,
      "",
      ...(arm.notes.length === 0 ? [] : [...arm.notes.map((note) => `- ${note}`), ""]),
    ]
  }
  return [
    `### ${arm.arm}`,
    "",
    table(
      ["metric", "value"],
      [
        ["cold start", latency(metrics.coldStartMs)],
        [
          "CPU workload",
          `${fixed(metrics.cpuWorkMs, 1)} ms CPU for ${
            metrics.cpuWorkIterations.toLocaleString("en-US")
          } iterations`,
        ],
        ["IPC round trip", latency(metrics.ipcRoundTripMs)],
        ["RSS per agent at target", `${mib(metrics.rssPerAgentBytes)} MiB`],
      ],
    ),
    "",
    table(
      ["agents", "outcome", "all-ready ms", "total RSS MiB", "RSS/agent MiB", "failures"],
      metrics.density.map((point) => [
        String(point.agents),
        point.outcome,
        fixed(point.readyMs, 1),
        mib(point.totalRssBytes),
        mib(point.perAgentRssBytes),
        String(point.failures),
      ]),
    ),
    "",
    ...(arm.notes.length === 0 ? [] : [...arm.notes.map((note) => `- ${note}`), ""]),
  ]
}

export const renderReport = (result: SpikeResult): string => {
  const arms = result.arms
  const selected = result.recommendation.recommended
  const host = result.host

  const lines: ReadonlyArray<string> = [
    "# S3 (`ISO-1`) — lightweight inter-agent isolation bake-off",
    "",
    table(
      ["field", "value"],
      [
        ["spike", "`S3`"],
        ["validation identifier", "`ISO-1`"],
        ["dataset", `\`${result.dataset}\` (sealed; evidence ${result.evidence.status})`],
        ["run timestamp", result.generatedAt],
        ["host kernel", host.kernel],
        ["runtime", host.runtime],
        ["cpus", String(host.cpus)],
        ["total memory", `${mib(host.totalMemoryBytes)} MiB`],
        ["cgroup", `${host.cgroupVersion} (${host.cgroupControllers.join(" ")})`],
        ["unprivileged user namespaces", host.userNamespaces],
        ["Landlock ABI", String(host.landlockAbi)],
        ["cgroup2 nsdelegate", host.cgroupNsdelegate ? "present" : "absent"],
        ["delegated cgroup parent", host.delegatedParent],
        ["kvm device", host.kvmDevice],
        [
          "microVM runtimes on PATH",
          host.microvmRuntimes.length === 0 ? "none" : host.microvmRuntimes.join(", "),
        ],
        [
          "target concurrency",
          `${result.targetConcurrency} agents (spike assumption; \`LIM-1\` owns the real ceiling)`,
        ],
      ],
    ),
    "",
    "## Supervisor",
    "",
    table(
      ["field", "value"],
      [
        ["kind", result.supervisor.kind],
        ["uid", String(result.supervisor.uid)],
        ["Landlock ABI", String(result.supervisor.landlockAbi)],
        ["delegated cgroup root", `\`${result.supervisor.cgroupRoot}\``],
        ["delegated by", `\`${result.supervisor.delegatedBy}\``],
        ...result.supervisor.dependencies.map((dependency, index) =>
          [`dependency ${index + 1}`, dependency] as const
        ),
      ],
    ),
    "",
    "## No security property is claimed",
    "",
    result.security.statement,
    "",
    "Explicitly outside this spike:",
    "",
    ...result.security.excluded.map((item) => `- ${item}`),
    "",
    "## Decision input for `DG-4`",
    "",
    `Outcome: **${
      result.recommendation.status === "recommend"
        ? `recommend \`${selected ?? "none"}\``
        : "no selection; `DG-4` stays open"
    }**.`,
    "",
    `Pre-registered rule: ${result.recommendation.rule}`,
    "",
    table(
      ["arm", "eligible", "reason when not eligible"],
      arms.map((arm) => [
        arm.arm,
        result.recommendation.eligible.includes(arm.arm) ? "yes" : "no",
        result.recommendation.ineligible.find((entry) => entry.arm === arm.arm)?.reason ?? "—",
      ]),
    ),
    "",
    ...result.recommendation.rationale.map((line) => `- ${line}`),
    "",
    "## Method",
    "",
    `Every arm is measured on the same host with the same reference agent workload: a fixed integer accumulation loop (${
      fixed(
        result.arms.find((arm) => arm.metrics !== undefined)?.metrics?.cpuWorkIterations ?? 0,
        0,
      )
    } iterations), a same-host request/response round trip, and a density sweep at ${
      DENSITY_STEPS.join(", ")
    } agents.`,
    "",
    `The process arm runs one Deno child per agent as the init of its own rootless user+PID+mount namespace, with a minimal chroot root and a Landlock ruleset applied before the agent code runs. Each agent gets a cgroup directory under the one delegated root with \`memory.max\`, \`memory.swap.max=0\`, \`pids.max\`, and \`cpu.max\`, plus rlimits on core size, file size, and file descriptors. The WASM arm runs one WebAssembly instance inside one permission-less Deno worker per agent. The microVM arm is not measurable on this host.`,
    "",
    "Cold start is measured from supervisor spawn to the first ready signal, after one discarded warm-up run. Per-agent RSS is read from the agent cgroup (`memory.current`) for the process arm and from the process RSS delta for the WASM arm. Density records the time until every agent is ready and the failures at that count.",
    "",
    "Density all-ready is the slowest agent's own ready time, not wall clock from the first spawn, so supervisor bookkeeping does not inflate it. Process-arm density RSS is the mean `memory.current` at all-ready; WASM density RSS is the process RSS delta divided by instance count.",
    "",
    "Implementation lines count the supervision surface per arm: supervisor glue, the namespace/chroot sandbox plan, the agent entrypoint, and the protocol. Measurement probes and the probe-only WASM binary encoder are excluded, so the number reflects the mechanism rather than this harness.",
    "",
    "Containment probes are destructive only inside the measured cgroup and sandbox: crash (`SIGKILL` of the agent host pid), hang (`cgroup.kill` on an infinite loop), OOM (request beyond the memory ceiling), fork bomb (200 fork attempts under `pids.max`), orphan reaping (a grandchild that dies with the PID-namespace init), and cleanup after kill (`cgroup.kill`, `cgroup.events populated=0`, `rmdir`). No probe writes outside the delegated cgroup, the per-agent sandbox root, and its scratch directory.",
    "",
    "## Containment matrix",
    "",
    table(
      ["check", ...arms.map((arm) => arm.arm)],
      result.matrix.map((
        row,
      ) => [row.check, ...arms.map((arm) => statusCell(row.cells[arm.arm] ?? "not-run"))]),
    ),
    "",
    "## Measurements",
    "",
    ...arms.flatMap((arm) => armSection(arm)),
    "## Containment evidence",
    "",
    ...arms.flatMap((arm) => [
      `### ${arm.arm}`,
      "",
      ...(arm.checks.length === 0 ? ["No probe was run for this arm.", ""] : [
        ...arm.checks.map((check) => `- **${check.id}** — \`${check.status}\` — ${check.detail}`),
        "",
      ]),
    ]),
    "## Operability costs",
    "",
    table(
      ["arm", "host dependencies", "root", "implementation lines", "packaging", "debuggability"],
      arms.map((arm) => [
        arm.arm,
        arm.operability.hostDependencies.join("; ") || "none",
        arm.operability.requiresRoot ? "required" : "not required",
        String(arm.operability.supervisorLoc),
        arm.operability.packaging,
        arm.operability.debuggability,
      ]),
    ),
    "",
    ...arms.flatMap((arm) => [
      `**${arm.arm} limitations**`,
      "",
      ...(arm.operability.limitations.length === 0
        ? ["- none recorded", ""]
        : [...arm.operability.limitations.map((item) => `- ${item}`), ""]),
    ]),
    "## Blockers",
    "",
    ...(arms.flatMap((arm) =>
        arm.status === "blocked" || arm.status === "inconclusive"
          ? [`- **${arm.arm}** (${arm.status}): ${arm.blockedReason ?? "no reason recorded"}`]
          : []
      ).length === 0
      ? ["- none"]
      : arms.flatMap((arm) =>
        arm.status === "blocked" || arm.status === "inconclusive"
          ? [`- **${arm.arm}** (${arm.status}): ${arm.blockedReason ?? "no reason recorded"}`]
          : []
      )),
    "",
    "## Residuals",
    "",
    ...result.residuals.map((item) => `- ${item}`),
    "",
    "## Evidence status",
    "",
    result.evidence.status === "emitted"
      ? `\`EvidenceEnvelope\`: **emitted** — testId \`${result.evidence.envelope.testId}\`, fixture \`${result.evidence.envelope.fixtureVersion}\`, outcome \`${result.evidence.envelope.outcome}\`, artifacts ${
        result.evidence.envelope.artifacts.map((digest) => `\`${digest}\``).join(", ")
      }. The envelope is committed beside the result as \`spikes/isolation/envelope.json\`.`
      : `\`EvidenceEnvelope\`: **unavailable**. ${result.evidence.reason}.`,
    "",
    "## Reproduction",
    "",
    "```sh",
    "systemd-run --user --property=Delegate=yes --property=DelegateSubgroup=supervisor \\",
    '  --pipe --wait --working-directory="$PWD" --setenv=KANBEI_S3_DELEGATED=1 \\',
    '  --setenv=DENO_DIR="$PWD/tmp/deno-cache" \\',
    '  "$(command -v deno)" run -A --unstable-worker-options src/spikes/isolation/main.ts',
    "# or simply, from the repository root: main.ts re-execs into the command above",
    "deno run -A --unstable-worker-options src/spikes/isolation/main.ts",
    "deno test -A tests/spikes/isolation",
    "```",
    "",
    "The runner rewrites `spikes/isolation/result.json` and this file. Any change to a threshold in `src/spikes/isolation/protocol.ts` invalidates the recorded result and requires a re-run.",
    "",
  ]

  return `${lines.join("\n").trimEnd()}\n`
}
