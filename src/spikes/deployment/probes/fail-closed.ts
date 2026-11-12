import { Effect } from "effect"
import { ensureDir, readProcOr } from "../facts.ts"
import { recover } from "../recover.ts"
import { decidePosture } from "../policy.ts"
import type { DeclaredPosture, ObservedCapabilities } from "../policy.ts"
import { findings, probe, ProbeEnv } from "../probe.ts"
import type { ProbeEnvShape } from "../probe.ts"
import { guardedEffect } from "../policy.ts"

const declaration: DeclaredPosture = {
  secretIsolation: "same-uid",
  filesystemIsolation: "landlock",
  hardCeilings: "cgroup",
}

const readNumber = (target: string): Effect.Effect<number, unknown, ProbeEnvShape> =>
  readProcOr(target, "").pipe(
    Effect.map((value) => Number(value.trim())),
    Effect.map((value) => (Number.isFinite(value) ? value : 0)),
  )

const postureEvaluation = (landlockAbi: number) =>
  probe(
    {
      id: "FC-01",
      category: "fail-closed",
      arm: "both",
      method: "evaluate the pre-registered posture rules against this host's measured capabilities",
      prerequisites: [],
    },
    Effect.gen(function* () {
      const env = yield* ProbeEnv
      const probeDir = yield* ensureDir("fail-closed")
      const sink = env.path.join(probeDir, "audit.log")
      yield* env.fs.writeFileString(sink, "audit\n")
      const sinkWritable = yield* env.fs.writeFileString(sink, "audit\n").pipe(
        Effect.as(true),
        recover(() => Effect.succeed(false)),
      )
      const ptraceScope = yield* readNumber("/proc/sys/kernel/yama/ptrace_scope")
      const maxUserNamespaces = yield* readNumber("/proc/sys/user/max_user_namespaces")
      const controllers = yield* readProcOr(
        `/sys/fs/cgroup/user.slice/user-${Deno.uid()}.slice/user@${Deno.uid()}.service/cgroup.controllers`,
        "",
      )
      const observed: ObservedCapabilities = {
        landlockAbi,
        ptraceScope,
        userNamespacesEnabled: maxUserNamespaces > 0,
        cgroupDelegated: controllers.trim() !== "",
        auditSinkWritable: sinkWritable,
      }
      const decision = decidePosture(declaration, observed)
      return findings(
        decision.decision === "allowed" ? "pass" : "fail",
        `declared=${JSON.stringify(declaration)} -> ${decision.decision}${
          decision.reasons.length === 0 ? "" : `: ${decision.reasons.join("; ")}`
        }`,
        `observed=${JSON.stringify(observed)}; residuals=${decision.acceptedResiduals.join("; ")}`,
        decision.decision === "allowed"
          ? "the pure user-space posture is admissible only with the accepted residuals recorded; it is not a claim of kernel-enforced secret isolation"
          : undefined,
      )
    }),
  )

const auditSinkDenial = probe(
  {
    id: "FC-02",
    category: "fail-closed",
    arm: "both",
    method: "make the audit sink unwritable and confirm the guarded effect is denied",
    prerequisites: ["deno"],
  },
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const dir = yield* ensureDir("fail-closed", "gate")
    const sink = env.path.join(dir, "audit.log")
    const marker = env.path.join(dir, "effect-ran")
    yield* env.fs.writeFileString(sink, "audit\n")
    const writable = yield* env.fs.writeFileString(sink, "audit-2\n").pipe(
      Effect.as(true),
      recover(() => Effect.succeed(false)),
    )
    yield* env.fs.chmod(sink, 0o444)
    const blocked = yield* env.fs.writeFileString(sink, "audit-3\n").pipe(
      Effect.as(true),
      recover(() => Effect.succeed(false)),
    )
    const allowedRun = yield* guardedEffect(writable, env.fs.writeFileString(marker, "ran\n")).pipe(
      Effect.as(true),
      recover(() => Effect.succeed(false)),
    )
    yield* env.fs.remove(marker).pipe(Effect.ignore)
    yield* env.fs.chmod(sink, 0o600).pipe(Effect.ignore)
    const deniedRun = yield* guardedEffect(blocked, env.fs.writeFileString(marker, "ran\n")).pipe(
      Effect.as(true),
      recover(() => Effect.succeed(false)),
    )
    const markerExists = yield* env.fs.exists(marker)
    return findings(
      writable && !blocked && allowedRun && !deniedRun && !markerExists ? "pass" : "fail",
      `sink writable=${writable}, writable after chmod 0444=${blocked}, guarded effect allowed=${allowedRun}, guarded effect after denial=${deniedRun}`,
      `audit sink ${sink}; effect marker exists=${markerExists}`,
      allowedRun && !deniedRun
        ? "an unwritable audit sink denies the effect and records the denial; no effect runs unrecorded"
        : undefined,
    )
  }),
)

export const failClosedProbes = (landlockAbi: number) =>
  Effect.all([postureEvaluation(landlockAbi), auditSinkDenial], {
    concurrency: 1,
  }).pipe(Effect.map((results) => results))
