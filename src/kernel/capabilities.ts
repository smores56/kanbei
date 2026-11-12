import { Result, Schema } from "effect"

/**
 * The closed capability vocabulary. There is no filesystem, network, or
 * subprocess member: an agent's entire reach is one of these channel calls.
 */
export const CapabilityName = Schema.Literals([
  "channel.inbox.read",
  "channel.vault.read",
  "channel.todo.read",
  "channel.todo.write",
  "channel.chat.respond",
  "channel.csr.renew",
  "channel.search.query",
  "lane.invoke",
])
export type CapabilityName = typeof CapabilityName.Type

export const ALL_CAPABILITIES: ReadonlyArray<CapabilityName> = [
  "channel.inbox.read",
  "channel.vault.read",
  "channel.todo.read",
  "channel.todo.write",
  "channel.chat.respond",
  "channel.csr.renew",
  "channel.search.query",
  "lane.invoke",
]

/** The capabilities a sandbox may call; anything else is undeclared. */
export const CapabilityManifest = Schema.Struct({
  agentId: Schema.String,
  sandboxId: Schema.String,
  capabilities: Schema.Array(CapabilityName),
})
export type CapabilityManifest = typeof CapabilityManifest.Type

export const isCapabilityName = (value: unknown): value is CapabilityName =>
  Schema.is(CapabilityName)(value)

/** Declarative manifest problems that refuse a sandbox before any call. */
export const capabilityManifestIssues = (
  manifest: CapabilityManifest,
): ReadonlyArray<string> => {
  const issues: Array<string> = []
  if (manifest.agentId.trim() === "") issues.push("agentId is required")
  if (manifest.sandboxId.trim() === "") issues.push("sandboxId is required")
  if (new Set(manifest.capabilities).size !== manifest.capabilities.length) {
    issues.push("duplicate capability")
  }
  const unknown = manifest.capabilities.filter((name) => !isCapabilityName(name))
  if (unknown.length > 0) issues.push(`unknown capability: ${unknown.join(",")}`)
  return issues
}

/** Strict boundary parse: unknown keys and unknown capabilities refuse. */
export const parseCapabilityManifest = (
  input: unknown,
): Result.Result<CapabilityManifest, string> => {
  try {
    const decoded = Schema.decodeUnknownSync(CapabilityManifest, {
      onExcessProperty: "error",
    })(input)
    const issues = capabilityManifestIssues(decoded)
    return issues.length === 0 ? Result.succeed(decoded) : Result.fail(issues.join("; "))
  } catch (error) {
    return Result.fail(error instanceof Error ? error.message.slice(0, 300) : String(error))
  }
}

/** Monty-visible stub names are Python identifiers; the receipt keeps the dotted name. */
export const stubNameFor = (capability: CapabilityName): string => capability.replaceAll(".", "_")
