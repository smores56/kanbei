import { Schema } from "effect"
import { ChannelId } from "./identity.ts"

export const MountAccess = Schema.Literals(["ro", "rw"])
export type MountAccess = typeof MountAccess.Type

export const MountClass = Schema.Literals([
  "image",
  "inbox",
  "vault",
  "todo",
  "scratch",
  "agent_state",
  "mutable_code",
  "runtime",
])
export type MountClass = typeof MountClass.Type

export const EnforcementClass = Schema.Literals(["none", "monty_wasm", "process_cgroup"])
export type EnforcementClass = typeof EnforcementClass.Type

export const SandboxMount = Schema.Struct({
  path: Schema.String,
  access: MountAccess,
  class: MountClass,
})
export type SandboxMount = typeof SandboxMount.Type

export const SandboxLimits = Schema.Struct({
  wallMs: Schema.Number,
  memoryBytes: Schema.Number,
  pidsMax: Schema.Number,
  cpuQuota: Schema.Number,
  openFiles: Schema.Number,
  todoBytes: Schema.Number,
})
export type SandboxLimits = typeof SandboxLimits.Type

/** The declared posture the supervisor verifies before agent code runs. */
export const SandboxManifest = Schema.Struct({
  sandboxId: Schema.String,
  agentId: Schema.String,
  generation: Schema.String,
  cgroupPath: Schema.String,
  enforcementRequired: EnforcementClass,
  mounts: Schema.Array(SandboxMount),
  channels: Schema.Array(ChannelId),
  limits: SandboxLimits,
})
export type SandboxManifest = typeof SandboxManifest.Type

/** What the host actually observes after the sandbox starts. */
export const ObservedPosture = Schema.Struct({
  enforcement: EnforcementClass,
  generation: Schema.String,
  addresses: Schema.Array(Schema.String),
  channelSockets: Schema.Array(ChannelId),
  mounts: Schema.Array(SandboxMount),
})
export type ObservedPosture = typeof ObservedPosture.Type

const enforcementRank: Readonly<Record<EnforcementClass, number>> = {
  none: 0,
  monty_wasm: 1,
  process_cgroup: 2,
}

const mountKey = (mount: SandboxMount): string => `${mount.path}|${mount.access}|${mount.class}`

const isPositive = (value: number): boolean => Number.isFinite(value) && value > 0

/** Structural rules that hold regardless of the observed posture. */
export const manifestIssues = (manifest: SandboxManifest): ReadonlyArray<string> => {
  const issues: Array<string> = []
  const mountClasses = new Set(manifest.mounts.map((mount) => mount.class))
  if (new Set(manifest.mounts.map(mountKey)).size !== manifest.mounts.length) {
    issues.push("duplicate mount")
  }
  if (new Set(manifest.channels).size !== manifest.channels.length) {
    issues.push("duplicate channel")
  }
  if (!mountClasses.has("inbox") || !mountClasses.has("vault")) {
    issues.push("inbox and vault mounts are required")
  }
  for (const mount of manifest.mounts) {
    if (mount.class === "inbox" || mount.class === "vault") {
      if (mount.access !== "ro") issues.push(`${mount.class} mount must be read-only`)
    }
    if (mount.class === "todo" && mount.access !== "rw") {
      issues.push("todo mount must be read-write")
    }
  }
  const limits = manifest.limits
  if (
    !isPositive(limits.wallMs) || !isPositive(limits.memoryBytes) ||
    !isPositive(limits.pidsMax) || !isPositive(limits.cpuQuota) ||
    !isPositive(limits.openFiles) || !isPositive(limits.todoBytes)
  ) {
    issues.push("limits must be positive")
  }
  if (manifest.cgroupPath.trim() === "") issues.push("cgroupPath is required")
  return issues
}

export const enforcementIsAtLeast = (
  observed: EnforcementClass,
  required: EnforcementClass,
): boolean => enforcementRank[observed] >= enforcementRank[required]

export const sameMounts = (
  declared: ReadonlyArray<SandboxMount>,
  observed: ReadonlyArray<SandboxMount>,
): {
  readonly missing: ReadonlyArray<SandboxMount>
  readonly extra: ReadonlyArray<SandboxMount>
} => {
  const observedKeys = new Set(observed.map(mountKey))
  const declaredKeys = new Set(declared.map(mountKey))
  return {
    missing: declared.filter((mount) => !observedKeys.has(mountKey(mount))),
    extra: observed.filter((mount) => !declaredKeys.has(mountKey(mount))),
  }
}
