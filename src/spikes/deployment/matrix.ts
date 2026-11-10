import { Effect } from "effect"
import { assessArm, buildRecommendation } from "./deployment-plane.ts"
import { recover } from "./recover.ts"
import { readProcOr } from "./facts.ts"
import { failClosedProbes } from "./probes/fail-closed.ts"
import { identityProbes } from "./probes/identity.ts"
import { landlockProbes } from "./probes/landlock.ts"
import { limitProbes } from "./probes/limits.ts"
import { namespaceProbes } from "./probes/namespaces.ts"
import { networkProbes } from "./probes/network.ts"
import { processProbes } from "./probes/process.ts"
import { recoveryProbes } from "./probes/recovery.ts"
import { secretProbes } from "./probes/secrets.ts"
import { systemdProbes } from "./probes/systemd.ts"
import { ProbeEnv } from "./probe.ts"
import type { ProbeEnvShape } from "./probe.ts"
import type { DecisionGateInput, HostProfile, ProbeResult, SpikeResultBody } from "./types.ts"

const firstLine = (text: string): string => text.split("\n")[0]?.trim() ?? ""

const hostProfile = (landlockAbi: string): Effect.Effect<HostProfile, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const uid = Deno.uid() ?? 0
    const groups = yield* env.command.run("id", ["-nG"], { timeoutMs: 8_000 }).pipe(
      Effect.map((result) => result.stdout.trim().split(/\s+/u)),
      recover(() => Effect.succeed([] as ReadonlyArray<string>)),
    )
    const systemdVersion = yield* env.command.run("systemctl", ["--version"], { timeoutMs: 8_000 })
      .pipe(
        Effect.map((result) => firstLine(result.stdout)),
        recover(() => Effect.succeed("absent")),
      )
    const cgroupV2 = yield* env.fs.exists("/sys/fs/cgroup/cgroup.controllers")
    const delegated = yield* readProcOr(
      `/sys/fs/cgroup/user.slice/user-${uid}.slice/user@${uid}.service/cgroup.controllers`,
      "",
    )
    const status = yield* readProcOr("/proc/self/status", "")
    const capabilities = /CapEff:\s*([0-9a-f]+)/u.exec(status)?.[1] ?? "unknown"
    const linger = yield* env.command.run("loginctl", [
      "show-user",
      Deno.env.get("USER") ?? "unknown",
      "-p",
      "Linger",
    ], { timeoutMs: 8_000 }).pipe(
      Effect.map((result) => result.stdout.trim().replace("Linger=", "") || "unknown"),
      recover(() => Effect.succeed("unknown")),
    )
    return {
      hostname: Deno.hostname(),
      kernel: `${Deno.build.os}-${Deno.osRelease()}`,
      user: Deno.env.get("USER") ?? "unknown",
      uid: Deno.uid() ?? 0,
      groups: [...groups],
      systemd: systemdVersion,
      cgroupVersion: cgroupV2 ? "v2" : "v1-or-unknown",
      yamaPtraceScope: yield* readProcOr("/proc/sys/kernel/yama/ptrace_scope", "absent"),
      landlockAbi,
      maxUserNamespaces: yield* readProcOr("/proc/sys/user/max_user_namespaces", "absent"),
      linger,
      delegatedControllers: delegated.trim().split(/\s+/u).filter((value) => value !== ""),
      ambientCapabilities: capabilities,
    }
  })

export const collectProbes: Effect.Effect<ReadonlyArray<ProbeResult>, unknown, ProbeEnvShape> =
  Effect.gen(function* () {
    const namespaces = yield* namespaceProbes
    const process = yield* processProbes
    const landlock = yield* landlockProbes
    const systemd = yield* systemdProbes
    const limits = yield* limitProbes
    const recovery = yield* recoveryProbes
    const network = yield* networkProbes
    const secrets = yield* secretProbes
    const identity = yield* identityProbes
    const landlockAbi = /ABI=(\d+)/u.exec(
      landlock.find((probe) => probe.id === "LL-01")?.observed ?? "",
    )?.[1] ?? "0"
    const failClosed = yield* failClosedProbes(Number(landlockAbi))
    return [
      ...namespaces,
      ...process,
      ...landlock,
      ...systemd,
      ...limits,
      ...recovery,
      ...network,
      ...secrets,
      ...identity,
      ...failClosed,
    ]
  })

const decisionGate = (probes: ReadonlyArray<ProbeResult>): DecisionGateInput => ({
  gate: "DG-3",
  status: "input-ready-human-signature-required",
  blocks: ["E1", "E2", "H7"],
  evidenceProbes: probes
    .filter((probe) => probe.status === "fail" || probe.status === "blocked")
    .map((probe) => probe.id),
  statement:
    "Variant D (hybrid) is a risk-based proposal: the broker and sandbox supervisor as dedicated system users, core/agents/UI under the deployment user. DEP-1 had no pre-registered decision rule and the dedicated-UID arm is unverified, so this is not a rule-decided selection; DG-3 closes only with a human signature.",
})

export const buildSpikeResult = (
  probes: ReadonlyArray<ProbeResult>,
): Effect.Effect<SpikeResultBody, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const arms = [assessArm(probes, "pure"), assessArm(probes, "dedicated")]
    const recommendation = buildRecommendation(arms)
    const landlockAbi = /ABI=(\d+)/u.exec(
      probes.find((probe) => probe.id === "LL-01")?.observed ?? "",
    )?.[1] ?? "0"
    const host = yield* hostProfile(landlockAbi)
    const residuals = [
      ...new Set([
        ...probes.flatMap((probe) => (probe.residual === undefined ? [] : [probe.residual])),
        ...recommendation.residuals,
      ]),
    ]
    const blockedCapabilities = probes
      .filter((probe) => probe.status === "blocked")
      .map((probe) => `${probe.id}: ${probe.observed}`)
    return {
      schemaVersion: 1,
      protocol: "DEP-1",
      spike: "S2",
      dataset: "security-v1",
      host,
      probes: [...probes],
      arms,
      recommendation,
      decisionGate: decisionGate(probes),
      residuals,
      blockedCapabilities,
    }
  })

export const runSpike: Effect.Effect<SpikeResultBody, unknown, ProbeEnvShape> = Effect.gen(
  function* () {
    const probes = yield* collectProbes
    return yield* buildSpikeResult(probes)
  },
)
