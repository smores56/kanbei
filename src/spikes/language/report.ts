import type { BakeoffResult } from "./result.ts"

const escapeCell = (value: string): string =>
  value.replaceAll("|", "\\|").replace(/\s+/gu, " ").trim()
const cell = (value: string | number | boolean | null): string =>
  value === null ? "—" : escapeCell(String(value))
const yes = (value: boolean): string => (value ? "yes" : "no")

const mib = (bytes: number | undefined): string =>
  bytes === undefined ? "—" : (bytes / (1024 * 1024)).toFixed(1)

const millis = (value: number | undefined): string => value === undefined ? "—" : value.toFixed(2)

const percent = (value: number): string => `${(value * 100).toFixed(1)}%`

const pad = (value: string, width: number): string => value.padEnd(width, " ")

const table = (
  headers: ReadonlyArray<string>,
  rows: ReadonlyArray<ReadonlyArray<string>>,
): string => {
  const cells = rows.map((row) => headers.map((_header, index) => escapeCell(row[index] ?? "")))
  const widths = headers.map((header, index) =>
    Math.max(escapeCell(header).length, ...cells.map((row) => (row[index] ?? "").length))
  )
  const line = (values: ReadonlyArray<string>): string =>
    `| ${values.map((value, index) => pad(value, widths[index] ?? 0)).join(" | ")} |`
  return [
    line(headers.map(escapeCell)),
    line(widths.map((width) => "-".repeat(Math.max(1, width)))),
    ...cells.map(line),
  ].join("\n")
}

const runtimeSection = (result: BakeoffResult): string => {
  const rows = result.localMeasurements.runtimes.map((runtime) => {
    const resolution = runtime.resolution
    return [
      cell(runtime.arm),
      cell(runtime.variant),
      resolution.status === "available" ? "available" : "unavailable",
      resolution.status === "available" ? cell(resolution.version) : cell(resolution.reason),
      resolution.status === "available" ? cell(resolution.source) : cell(resolution.missingInput),
      millis(runtime.startup?.p50Ms),
      millis(runtime.startup?.p95Ms),
      mib(runtime.peakRssBytes),
      runtime.probe === undefined ? "—" : yes(runtime.probe.fanout.ok),
      runtime.probe === undefined ? "—" : cell(runtime.probe.retry.attempts),
      runtime.probeFailure === undefined ? "—" : cell(runtime.probeFailure),
    ]
  })
  return table(
    [
      "arm",
      "variant",
      "status",
      "version",
      "source / missing input",
      "startup p50 ms",
      "startup p95 ms",
      "peak RSS MiB",
      "fanout gold",
      "retry attempts",
      "probe failure",
    ],
    rows,
  )
}

const hostCallSection = (result: BakeoffResult): string =>
  table(
    [
      "variant",
      "latency ms",
      "calls",
      "wall ms",
      "p50 overhead ms",
      "p95 overhead ms",
      "simulation",
      "fanout gold",
    ],
    result.localMeasurements.hostCalls.map((row) => [
      cell(row.variant),
      cell(row.latencyMs),
      cell(row.calls),
      millis(row.wallMs),
      millis(row.p50OverheadMs),
      millis(row.p95OverheadMs),
      cell(row.latencySimulation),
      yes(row.fanoutOk),
    ]),
  )

const snapshotSection = (result: BakeoffResult): string =>
  table(
    ["variant", "snapshot", "snapshot ok", "approval pause", "approval ok", "trust"],
    result.localMeasurements.snapshot.map((row) => [
      cell(row.variant),
      yes(row.snapshotSupported),
      yes(row.snapshotOk),
      yes(row.approvalPauseSupported),
      yes(row.approvalPauseOk),
      cell(row.trust),
    ]),
  )

const securityProbes = [
  "file-write",
  "env-read",
  "process-spawn",
  "network-socket",
  "secret-file-read",
]

const securityState = (allowed: boolean | null): string =>
  allowed === null ? "—" : allowed ? "allowed" : "denied"

const securitySection = (result: BakeoffResult): string =>
  table(
    ["variant", ...securityProbes],
    result.localMeasurements.security
      .reduce<Array<{ variant: string; values: Array<string> }>>((groups, row) => {
        const existing = groups.find((group) => group.variant === row.variant)
        if (existing === undefined) {
          groups.push({ variant: row.variant, values: [securityState(row.allowed)] })
        } else {
          existing.values.push(securityState(row.allowed))
        }
        return groups
      }, [])
      .map((group) =>
        [cell(group.variant)].concat(
          securityProbes.map((_probe, index) => cell(group.values[index] ?? "—")),
        )
      ),
  )

const disclosureSection = (result: BakeoffResult): string =>
  table(
    ["tools", "full-schema chars", "progressive chars", "savings", "tokens"],
    result.localMeasurements.progressiveDisclosure.map((row) => [
      cell(row.tools),
      cell(row.fullSchemaChars),
      cell(row.progressiveChars),
      percent(row.savingsRatio),
      "blocked",
    ]),
  )

const taskSection = (result: BakeoffResult): string =>
  table(
    ["#", "id", "category", "adversarial", "rubric criteria", "required capabilities"],
    result.taskSuite.tasks.map((task) => [
      cell(task.ordinal),
      cell(task.id),
      cell(task.category),
      yes(task.adversarial),
      cell(task.rubric.length),
      cell(task.requiredCapabilities.join(", ")),
    ]),
  )

const modelSection = (result: BakeoffResult): string => {
  const slotRows = result.modelMatrix.slots.map((slot) => [
    cell(slot.provider),
    cell(slot.tier),
    slot.modelId === null ? "unset" : cell(slot.modelId),
    cell(slot.credentialEnv),
    yes(slot.credentialPresent),
  ])
  const blockedByArm = result.modelMatrix.cells.reduce<Record<string, number>>((acc, modelCell) => {
    acc[modelCell.arm] = (acc[modelCell.arm] ?? 0) + 1
    return acc
  }, {})
  const armRows = Object.entries(blockedByArm).map((
    [arm, count],
  ) => [cell(arm), cell(count), "blocked"])
  return [
    table(["provider", "tier", "model ID", "credential env", "credential present"], slotRows),
    "",
    `Axes: 5 arms × 10 tasks × 3 host-call latencies × 4 model slots; per-cell rows are in result.json. ` +
    `Cells: ${result.modelMatrix.plannedCells}; planned trials: ${result.modelMatrix.plannedTrials} ` +
    `(${result.modelMatrix.trialsPerCell} per cell at temperature ${result.modelMatrix.temperature}); ` +
    `completed model trials: 0.`,
    "",
    table(["arm", "blocked cells", "status"], armRows),
    "",
    "Missing inputs:",
    "",
    ...result.modelMatrix.missingInputs.map((input) =>
      `- \`${input.id}\` ${input.description}: ${input.exact}`
    ),
  ].join("\n")
}

const taxonomySection = (result: BakeoffResult): string =>
  table(
    ["id", "failure class", "hard fail"],
    result.failureTaxonomy.map((
      entry,
    ) => [cell(entry.id), cell(entry.description), yes(entry.hardFail)]),
  )

const preRegistrationSection = (result: BakeoffResult): string => {
  const registration = result.preRegistration
  const armRows = registration.arms.map((arm) => [
    cell(arm.arm),
    cell(arm.candidates.join(", ")),
    cell(arm.evaluatorVariantRule),
  ])
  const hypothesisRows = registration.hypotheses.map((hypothesis) => [
    cell(hypothesis.id),
    cell(hypothesis.statement),
    cell(hypothesis.nonInferiorityMargin),
  ])
  const metricRow = registration.metrics
    .map((metric) => `${metric.id} (${metric.unit}, ${metric.direction}, ${metric.source})`)
    .join("; ")
  return [
    `Protocol \`${registration.protocolId}\` for decision \`${
      registration.decisionIds.join("`, `")
    }\`; ` +
    `datasets ${registration.datasets.join(", ")}.`,
    "",
    `Design: ${registration.analysis.design}; ${registration.trialsPerCell} trials per cell at temperature ` +
    `${registration.temperature}; host-call latencies ${
      registration.hostCallLatenciesMs.join(", ")
    } ms; ` +
    `${registration.providersRequired} providers with ${registration.modelsPerProvider} models each, including a ` +
    `smaller model; ${registration.analysis.confidenceLevel * 100}% percentile bootstrap with ` +
    `${registration.analysis.bootstrapResamples} resamples; seeds ` +
    `${registration.analysis.seeds.base} (${registration.analysis.seeds.derivation}); ${registration.analysis.multiplicity}.`,
    "",
    table(["arm", "candidates", "evaluator variant rule"], armRows),
    "",
    "Hypotheses and non-inferiority margins:",
    "",
    table(["id", "statement", "margin"], hypothesisRows),
    "",
    `Metrics: ${metricRow}`,
    "",
    "Decision rules:",
    "",
    `- Selection: ${registration.decisionRules.selection}`,
    `- Inconclusive: ${registration.decisionRules.inconclusive}`,
    `- Blocked: ${registration.decisionRules.blocked}`,
    `- Tie-break: ${registration.decisionRules.tieBreak}`,
    `- Partial evidence: ${registration.decisionRules.partialEvidence}`,
    "",
    "Hard fails:",
    "",
    ...registration.hardFails.map((fail) =>
      `- \`${fail.id}\` ${fail.condition} → ${fail.consequence}`
    ),
    "",
    "Frozen inputs:",
    "",
    ...registration.frozenInputs.map((input) =>
      `- \`${input.id}\` ${input.description}: ${input.value ?? "required before trials (unset)"}`
    ),
  ].join("\n")
}

export const renderReport = (result: BakeoffResult): string => {
  const decision = result.decisionRecord
  return [
    "# S1 / LANG-1 agent-facing execution-language bake-off",
    "",
    `Status: **blocked** — gate \`${decision.gate}\` is open; recommendation: \`${decision.recommendation}\`.`,
    "",
    "No model results are reported. The model arm could not run in this environment; every model cell is " +
    "marked blocked with its exact missing input. Local runtime, operability, and capability measurements are " +
    "reported and are evidence context only, never a selection.",
    "",
    "## 1. Pre-registration",
    "",
    preRegistrationSection(result),
    "",
    "## 2. Environment and method",
    "",
    table(
      ["field", "value"],
      [
        ["host fingerprint", cell(result.environment.hostFingerprint)],
        ["kernel", cell(result.environment.kernel)],
        ["runtime", cell(result.environment.runtime)],
        ["sandbox posture", cell(result.environment.sandboxPosture)],
        ["broker identity", cell(result.environment.brokerIdentity)],
        ["CPUs", cell(result.environment.cpus)],
        [
          "task suite",
          `${result.taskSuite.suiteId}@${result.taskSuite.suiteVersion} (${result.taskSuite.digest})`,
        ],
      ],
    ),
    "",
    "Local measurements use one host-injected probe program per runtime variant. Host calls are simulated " +
    "inside the probe: `sleep` for JS and CPython, a host-side sleep for Monty, `sleep(1)` subprocesses for " +
    "Bash, and a CPU busy-wait for Lua/Luau because those runtimes expose no portable wall-clock sleep. " +
    "Per-call overhead is wall time minus the nominal latency. Peak RSS is sampled from " +
    "`/proc/<pid>/status` `VmHWM` every 5 ms. Local probes reduce the call count to 50, 20, and 3 " +
    "at 10, 100, and 1000 ms to bound wall time; the pre-registered model workload keeps the full " +
    "task call counts. None of these numbers include the broker IPC path.",
    "",
    "## 3. Task suite",
    "",
    taskSection(result),
    "",
    "## 4. Local measurements",
    "",
    "### 4.1 Runtime resolution, startup, and memory",
    "",
    runtimeSection(result),
    "",
    "### 4.2 Host-call latency and fan-out correctness",
    "",
    hostCallSection(result),
    "",
    "### 4.3 Snapshot, restore, and approval pause",
    "",
    snapshotSection(result),
    "",
    "### 4.4 Capability and security probe matrix",
    "",
    "These rows describe the local posture. They are not a `security-v1` gate result: no frozen OS-sandbox " +
    "profile is available on this host, so an `allowed` row means the runtime did not deny the capability " +
    "locally, not that the behaviour is acceptable in production.",
    "",
    securitySection(result),
    "",
    "### 4.5 Progressive disclosure",
    "",
    disclosureSection(result),
    "",
    "Characters are recorded; provider token counts require the frozen tokenizer and model and are blocked.",
    "",
    "## 5. Blocked model matrix",
    "",
    modelSection(result),
    "",
    "## 6. Failure taxonomy",
    "",
    taxonomySection(result),
    "",
    "## 7. Recommendation status",
    "",
    `Gate \`${decision.gate}\` status: **${decision.status}**; recommendation: **${decision.recommendation}**.`,
    "",
    "Rationale:",
    "",
    ...decision.rationale.map((line) => `- ${line}`),
    "",
    "Supported by local evidence:",
    "",
    ...decision.supportedFindings.map((line) => `- ${line}`),
    "",
    "Not supported by local evidence:",
    "",
    ...decision.unsupportedFindings.map((line) => `- ${line}`),
    "",
    "Closing `DG-1` requires the model manifest, provider credentials, the frozen OS-sandbox profile, the " +
    "`cost-v1@1.0.0` and `security-v1@1.0.0` datasets, and complete paired trials (600 cells × 30 trials). " +
    "Until then the gate stays open and no arm is selected or rejected on model evidence.",
    "",
    "## 8. Reproduction",
    "",
    "```bash",
    "deno run -A src/spikes/language/cli.ts",
    "```",
    "",
    "Runtimes missing from `PATH` are resolved non-destructively from pinned nixpkgs flakes " +
    "(`nixpkgs#python3`, `nixpkgs#python3Packages.pydantic-monty`, `nixpkgs#nodejs`, `nixpkgs#lua5_4`, " +
    "`nixpkgs#luau`, `nixpkgs#bash`). Model runs are never attempted without a manifest and credentials.",
    "",
    "## 9. Residuals",
    "",
    ...result.residuals.map((line) => `- ${line}`),
    "",
    `Evidence: \`${result.evidence.testId}\` class \`${result.evidence.testClass}\` outcome ` +
    `\`${result.evidence.outcome}\`; envelope \`${result.evidence.envelope}\` (${
      result.evidence.reasons.join("; ")
    }).`,
    "",
  ].join("\n")
}
