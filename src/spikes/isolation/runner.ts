import { Effect } from "effect"
import { measureMicrovmArm } from "./arms/microvm.ts"
import { measureProcessArm, PROCESS_ARM_FILES } from "./arms/process-cgroup.ts"
import { measureWasmArm, WASM_ARM_FILES } from "./arms/wasm-instance.ts"
import { buildMatrix, recommend } from "./decision.ts"
import { readTextFile } from "./exec.ts"
import { probeHost } from "./host.ts"
import {
  AGENT_MEMORY_MAX_MIB,
  AGENT_TASKS_MAX,
  SECURITY_STATEMENT,
  TARGET_CONCURRENCY,
} from "./protocol.ts"
import type { SpikeResult } from "./types.ts"

export interface SpikeOptions {
  readonly agentScript: string
  readonly runId: string
  readonly generatedAt: string
}

const countLines = (root: string, files: ReadonlyArray<string>): Effect.Effect<number> =>
  Effect.forEach(
    files,
    (file) =>
      readTextFile(`${root}/${file}`).pipe(
        Effect.map((text) => (text ?? "").split("\n").filter((line) => line.trim() !== "").length),
        Effect.catch(() => Effect.succeed(0)),
      ),
    { concurrency: 1 },
  ).pipe(Effect.map((counts) => counts.reduce((total, count) => total + count, 0)))

const residualList: ReadonlyArray<string> = [
  "No security property is claimed for any arm. This record covers crash and resource containment only.",
  "Target concurrency 16 is a spike-local assumption; `LIM-1` sets the real operator ceiling, so density at any other count must be re-read from the sweep.",
  "Process-arm ceilings depend on systemd user cgroup delegation. Without it the arm degrades to rlimits and would fail the containment checks; the deployment plane must guarantee delegation (`DEP-1`).",
  "WASM RSS is a process-level delta because worker isolates share one address space. Per-instance memory is the declared linear-memory cap, not measured RSS.",
  "The WASM density row at one instance can read near-zero MiB because the process RSS delta is dominated by V8 heap reuse; the rows above one instance are the meaningful per-agent figures.",
  "The microVM arm is blocked: no microVM runtime and no sealed guest image on this host. `DG-4` cannot select a microVM on this evidence.",
  "Cold start includes systemd transient unit creation and on-the-fly Deno TypeScript loading. A packaged agent binary or a precompiled JS entrypoint would be faster; the comparison is same-host and same-runtime, not absolute.",
  "Fork-bomb containment was probed with `pids.max=16` on a dedicated transient unit, not at the 64-task per-agent ceiling.",
  "`security-v1` is not sealed in this repository, so no `EvidenceEnvelope` is emitted. S3 must be re-run against the sealed dataset before `A5` consumes it.",
]

export const runSpike = (root: string, options: SpikeOptions): Effect.Effect<SpikeResult> =>
  Effect.gen(function* () {
    const host = yield* probeHost
    const processLoc = yield* countLines(root, PROCESS_ARM_FILES)
    const wasmLoc = yield* countLines(root, WASM_ARM_FILES)
    yield* Effect.logInfo(
      `host: ${host.kernel} cpus=${host.cpus} cgroup=${host.cgroupVersion} delegation=${host.systemdUserDelegation} kvm=${host.kvmDevice} microvm=${host.microvmRuntimes.length}`,
    )

    yield* Effect.logInfo("process-cgroup arm: starting")
    const processArm = yield* measureProcessArm(
      {
        runId: options.runId,
        agentScript: options.agentScript,
        memoryMaxMib: AGENT_MEMORY_MAX_MIB,
        tasksMax: AGENT_TASKS_MAX,
      },
      processLoc,
    )
    yield* Effect.logInfo(`process-cgroup arm: ${processArm.status}`)

    yield* Effect.logInfo("wasm-instance arm: starting")
    const wasmArm = yield* measureWasmArm(wasmLoc)
    yield* Effect.logInfo(`wasm-instance arm: ${wasmArm.status}`)

    const microvmArm = yield* measureMicrovmArm(host, 0)
    yield* Effect.logInfo(`microvm arm: ${microvmArm.status}`)
    const arms = [processArm, wasmArm, microvmArm]

    return {
      schemaVersion: 1,
      spikeId: "S3",
      validationId: "ISO-1",
      dataset: "security-v1",
      generatedAt: options.generatedAt,
      host,
      targetConcurrency: TARGET_CONCURRENCY,
      security: {
        propertyClaimed: false,
        statement: SECURITY_STATEMENT,
        excluded: [
          "credential, approval, and trust-root boundaries (`ZT-01`, `ZT-11`, `ZT-13`), which remain owned by the broker, consent surface, and OS sandbox",
          "filesystem, network, and process capability enforcement (`DEP-1`)",
          "containment of malicious code that is already executing inside the agent; only crash and resource faults are measured",
          "container-per-agent, which the requirement excludes",
        ],
      },
      arms,
      matrix: [...buildMatrix(arms)],
      recommendation: recommend(arms),
      residuals: [...residualList],
      evidence: {
        status: "unavailable",
        reason:
          "`security-v1` has no sealed fixture in this repository (`fixtures/registry.json` lists journey-v1, append-v1, cost-v1, ui-v1 only), so this is a measurement record rather than a validation envelope",
      },
    }
  })
