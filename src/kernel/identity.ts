import { Result, Schema } from "effect"

/** The closed channel set; any other byte path is a channel leak (ZT-15). */
export const ChannelId = Schema.Literals(["inbox", "vault", "todo", "chat", "csr", "search"])
export type ChannelId = typeof ChannelId.Type

export const ComponentKind = Schema.Literals([
  "core",
  "sequencer",
  "llm",
  "monty_host",
  "channel",
  "agent",
  "plugin_host",
  "ui",
  "consent",
  "watchdog",
])
export type ComponentKind = typeof ComponentKind.Type

const Digest = Schema.String.pipe(Schema.check(Schema.isPattern(/^sha256:[0-9a-f]{64}$/u)))

export const ProcessIdentity = Schema.Struct({
  component: ComponentKind,
  instanceId: Schema.String,
  generation: Schema.String,
  cgroupPath: Schema.String,
  channels: Schema.Array(ChannelId),
  sandboxId: Schema.optional(Schema.String),
  startToken: Digest,
})
export type ProcessIdentity = typeof ProcessIdentity.Type

/** The supervisor's live record; a channel host compares a caller against it. */
export const SandboxRecord = Schema.Struct({
  sandboxId: Schema.String,
  agentId: Schema.String,
  generation: Schema.String,
  cgroupPath: Schema.String,
  channels: Schema.Array(ChannelId),
  startToken: Digest,
})
export type SandboxRecord = typeof SandboxRecord.Type

export type ChannelDenialCode =
  | "component_not_agent"
  | "generation_mismatch"
  | "cgroup_mismatch"
  | "sandbox_mismatch"
  | "token_mismatch"
  | "channel_not_granted"

export interface ChannelDenial {
  readonly code: ChannelDenialCode
  readonly detail: string
}

export interface Attribution {
  readonly sandboxId: string
  readonly agentId: string
  readonly channel: ChannelId
}

export type CallerVerdict = Result.Result<Attribution, ChannelDenial>

const right = (value: Attribution): CallerVerdict => Result.succeed(value)
const left = (code: ChannelDenialCode, detail: string): CallerVerdict =>
  Result.fail({ code, detail })

/**
 * Attribution and accident prevention inside the VM, not a security boundary
 * between agents (S-13). Every check must pass before a channel operation runs.
 */
export const verifyCaller = (
  record: SandboxRecord,
  caller: ProcessIdentity,
  requested: ChannelId,
): CallerVerdict => {
  if (caller.component !== "agent" && caller.component !== "plugin_host") {
    return left("component_not_agent", `component ${caller.component} cannot use a sandbox channel`)
  }
  if (caller.generation !== record.generation) {
    return left("generation_mismatch", `generation ${caller.generation} != ${record.generation}`)
  }
  if (caller.cgroupPath !== record.cgroupPath) {
    return left("cgroup_mismatch", `cgroup ${caller.cgroupPath} != ${record.cgroupPath}`)
  }
  if (caller.sandboxId !== undefined && caller.sandboxId !== record.sandboxId) {
    return left("sandbox_mismatch", `sandbox ${caller.sandboxId} != ${record.sandboxId}`)
  }
  if (caller.startToken !== record.startToken) {
    return left("token_mismatch", "start token does not match the live sandbox")
  }
  if (!record.channels.includes(requested) || !caller.channels.includes(requested)) {
    return left("channel_not_granted", `channel ${requested} is not in the sandbox manifest`)
  }
  return right({
    sandboxId: record.sandboxId,
    agentId: record.agentId,
    channel: requested,
  })
}
