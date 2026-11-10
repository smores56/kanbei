import { Effect, Schema } from "effect"
import type { ArtifactStore } from "../../evidence/artifact-store.ts"
import type { EnvironmentProbe } from "../../evidence/environment.ts"
import type { FixtureStore } from "../../evidence/fixture-store.ts"
import { runEvidence } from "../../evidence/harness.ts"
import { EvidenceEnvelope } from "../../evidence/types.ts"
import type { FixtureReference, Outcome } from "../../evidence/types.ts"
import { measureMicrovmArm } from "./arms/microvm.ts"
import { measureProcessArm, PROCESS_ARM_FILES } from "./arms/process-cgroup.ts"
import { measureWasmArm, WASM_ARM_FILES } from "./arms/wasm-instance.ts"
import { buildMatrix, recommend } from "./decision.ts"
import { readTextFile } from "./exec.ts"
import { probeHost } from "./host.ts"
import { SECURITY_STATEMENT, TARGET_CONCURRENCY } from "./protocol.ts"
import { DELEGATED_BY, HOST_DEPENDENCIES } from "./supervisor.ts"
import type { Supervisor } from "./supervisor.ts"
import type { ArmResult, SpikeResult, SupervisorInfo } from "./types.ts"

export interface SpikeOptions {
  readonly agentScript: string
  readonly entryScript: string
  readonly sourceDir: string
  readonly workRoot: string
  readonly runId: string
  readonly generatedAt: string
  readonly supervisor: Supervisor
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
  "Agent, supervisor, and broker share one UID. Isolation is namespace, chroot, Landlock, and cgroup; it is not UID-based, so a same-UID host process outside the sandbox is not contained.",
  "No seccomp policy is installed and io_uring remains enabled; Landlock does not mediate io_uring, so kernel attack surface remains.",
  "`/sys` is intentionally not mounted inside the agent root and Landlock never grants it. That is what reverses `SD-10`: an agent cannot reach its own cgroup to raise `memory.max`, `pids.max`, or `cpu.max`.",
  "Cold start now includes delegated-cgroup creation, user/PID/mount namespace setup, bind mounts, chroot, Landlock, and Deno module loading. A packaged binary would be faster; the comparison is same-host and same-runtime, not absolute.",
  "WASM RSS is a process-level delta because worker isolates share one address space. Per-instance memory is the declared linear-memory cap, not measured RSS.",
  "The WASM density row at one instance can read near-zero MiB because the process RSS delta is dominated by V8 heap reuse; the rows above one instance are the meaningful per-agent figures.",
  "The microVM arm is blocked: no microVM runtime and no sealed guest image on this host. `DG-4` cannot select a microVM on this evidence.",
  "Fork-bomb containment was probed with `pids.max=16` on a dedicated agent cgroup, not at the 64-task per-agent ceiling.",
  "The one systemd call is the delegated cgroup acquisition in `main.ts`; the per-agent path uses cgroupfs and namespaces only, but the supervisor still runs under the deployment user manager.",
]

const ContainmentCases = Schema.Struct({
  containmentCases: Schema.Array(Schema.Struct({ id: Schema.String })),
})

const Registry = Schema.Struct({
  datasets: Schema.Array(Schema.Struct({ id: Schema.String, version: Schema.String })),
})

const fixtureVersionOf = (root: string): Effect.Effect<string | undefined> =>
  readTextFile(`${root}/fixtures/registry.json`).pipe(
    Effect.orDie,
    Effect.map((text) => {
      if (text === null) return undefined
      const registry = Schema.decodeUnknownSync(Registry)(JSON.parse(text))
      return registry.datasets.find((entry) => entry.id === "security-v1")?.version
    }),
  )

const containmentCaseIds = (root: string): Effect.Effect<ReadonlyArray<string>> =>
  readTextFile(`${root}/fixtures/security-v1/cases.json`).pipe(
    Effect.orDie,
    Effect.map((text) => {
      if (text === null) return []
      const cases = Schema.decodeUnknownSync(ContainmentCases)(JSON.parse(text))
      return cases.containmentCases.map((entry) => entry.id)
    }),
  )

const metricsOf = (arms: ReadonlyArray<ArmResult>): Record<string, number> => {
  const metrics: Record<string, number> = {}
  for (const arm of arms) {
    metrics[`${arm.arm}.checksPassed`] = arm.checks.filter((check) =>
      check.status === "pass"
    ).length
    metrics[`${arm.arm}.checksFailed`] =
      arm.checks.filter((check) => check.status === "fail").length
    if (arm.metrics === undefined) continue
    metrics[`${arm.arm}.coldStartP50Ms`] = arm.metrics.coldStartMs.p50Ms
    metrics[`${arm.arm}.coldStartP95Ms`] = arm.metrics.coldStartMs.p95Ms
    metrics[`${arm.arm}.ipcP50Ms`] = arm.metrics.ipcRoundTripMs.p50Ms
    metrics[`${arm.arm}.ipcP95Ms`] = arm.metrics.ipcRoundTripMs.p95Ms
    metrics[`${arm.arm}.rssPerAgentMib`] = arm.metrics.rssPerAgentBytes / (1024 * 1024)
    metrics[`${arm.arm}.densityAtTarget`] =
      arm.metrics.density.find((point) => point.agents === TARGET_CONCURRENCY)?.failures ?? -1
  }
  return metrics
}

export const runSpike = (
  root: string,
  options: SpikeOptions,
): Effect.Effect<SpikeResult, never, ArtifactStore | FixtureStore | EnvironmentProbe> =>
  Effect.gen(function* () {
    const host = yield* probeHost(options.supervisor)
    const processLoc = yield* countLines(root, PROCESS_ARM_FILES)
    const wasmLoc = yield* countLines(root, WASM_ARM_FILES)
    const version = yield* fixtureVersionOf(root)
    const invariantIds = yield* containmentCaseIds(root)
    if (version === undefined) {
      return yield* Effect.die(new Error("security-v1 is not registered in fixtures/registry.json"))
    }
    yield* Effect.logInfo(
      `host: ${host.kernel} cpus=${host.cpus} cgroup=${host.cgroupVersion} userns=${host.userNamespaces} landlock=${host.landlockAbi} nsdelegate=${host.cgroupNsdelegate} parent=${host.delegatedParent} kvm=${host.kvmDevice} microvm=${host.microvmRuntimes.length}`,
    )
    yield* Effect.logInfo(
      `supervisor: ${options.supervisor.cgroupRoot} (uid ${options.supervisor.uid})`,
    )

    yield* Effect.logInfo("process-cgroup arm: starting")
    const processArm = yield* measureProcessArm(
      {
        runId: options.runId,
        agentScript: options.agentScript,
        entryScript: options.entryScript,
        sourceDir: options.sourceDir,
        workRoot: options.workRoot,
        supervisor: options.supervisor,
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
    const recommendation = recommend(arms)

    const supervisor: SupervisorInfo = {
      kind: "rootless-userns-chroot-landlock",
      uid: options.supervisor.uid,
      landlockAbi: host.landlockAbi,
      cgroupRoot: options.supervisor.cgroupRoot,
      delegatedBy: DELEGATED_BY,
      dependencies: HOST_DEPENDENCIES,
    }

    const base: Omit<SpikeResult, "evidence"> = {
      schemaVersion: 2,
      spikeId: "S3",
      validationId: "ISO-1",
      dataset: "security-v1",
      generatedAt: options.generatedAt,
      host,
      supervisor,
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
      recommendation,
      residuals: [...residualList],
    }

    const fixture: FixtureReference = {
      id: "security-v1",
      version,
      canonicalHead: { seq: 0, hash: `sha256:${"0".repeat(64)}` },
      sequenceRange: [0, 0],
    }
    const outcome: Outcome = recommendation.status === "recommend" ? "pass" : "inconclusive"
    const envelope = yield* runEvidence({
      testId: "S3",
      testClass: "spike",
      invariantIds: [...invariantIds],
      fixture,
      seeds: [0],
      body: (_snapshot, capture) =>
        capture.putArtifactText(JSON.stringify(base)).pipe(Effect.as(base)),
      metrics: metricsOf(arms),
    }).pipe(Effect.orDie)
    const sealed = Schema.decodeUnknownSync(EvidenceEnvelope)({ ...envelope, outcome })

    return { ...base, evidence: { status: "emitted" as const, envelope: sealed } }
  })
