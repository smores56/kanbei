import { Result } from "effect"
import type { ChannelId } from "../kernel/identity.ts"
import { enforcementIsAtLeast, manifestIssues, sameMounts } from "../kernel/manifest.ts"
import type { ObservedPosture, SandboxManifest } from "../kernel/manifest.ts"

export type StartupRefusalCode =
  | "manifest_invalid"
  | "generation_mismatch"
  | "posture_weaker"
  | "network_present"
  | "mount_missing"
  | "undeclared_mount"
  | "channel_missing"
  | "undeclared_channel"
  | "writable_root_missing"
  | "undeclared_writable_root"

export interface StartupRefusal {
  readonly code: StartupRefusalCode
  readonly detail: string
  readonly sandboxId: string
}

export type StartupVerdict = Result.Result<SandboxManifest, StartupRefusal>

const refuse = (
  manifest: SandboxManifest,
  code: StartupRefusalCode,
  detail: string,
): StartupVerdict => Result.fail({ code, detail, sandboxId: manifest.sandboxId })

/**
 * Fail closed: a weaker-than-declared posture or any extra channel/mount is a
 * start failure, never a runtime downgrade (S-12, ZT-15, V20, V23).
 */
export const verifyStartup = (
  manifest: SandboxManifest,
  observed: ObservedPosture,
): StartupVerdict => {
  const issues = manifestIssues(manifest)
  if (issues.length > 0) return refuse(manifest, "manifest_invalid", issues.join("; "))
  if (observed.generation !== manifest.generation) {
    return refuse(
      manifest,
      "generation_mismatch",
      `observed ${observed.generation} != declared ${manifest.generation}`,
    )
  }
  if (!enforcementIsAtLeast(observed.enforcement, manifest.enforcementRequired)) {
    return refuse(
      manifest,
      "posture_weaker",
      `observed ${observed.enforcement} < required ${manifest.enforcementRequired}`,
    )
  }
  if (observed.addresses.length !== 1 || observed.addresses[0] !== "lo") {
    return refuse(manifest, "network_present", `interfaces: ${observed.addresses.join(",")}`)
  }
  const mounts = sameMounts(manifest.mounts, observed.mounts)
  if (mounts.missing.length > 0) {
    return refuse(manifest, "mount_missing", mounts.missing.map((mount) => mount.path).join(","))
  }
  if (mounts.extra.length > 0) {
    return refuse(manifest, "undeclared_mount", mounts.extra.map((mount) => mount.path).join(","))
  }
  const declared = new Set<ChannelId>(manifest.channels)
  const sockets = new Set<ChannelId>(observed.channelSockets)
  const missing = manifest.channels.filter((channel) => !sockets.has(channel))
  if (missing.length > 0) return refuse(manifest, "channel_missing", missing.join(","))
  const extra = observed.channelSockets.filter((channel) => !declared.has(channel))
  if (extra.length > 0) return refuse(manifest, "undeclared_channel", extra.join(","))
  const observedWritable = new Set(observed.writableRoots)
  const missingWritable = manifest.writableRoots.filter((root) => !observedWritable.has(root))
  if (missingWritable.length > 0) {
    return refuse(manifest, "writable_root_missing", missingWritable.join(","))
  }
  const declaredWritable = new Set(manifest.writableRoots)
  const extraWritable = observed.writableRoots.filter((root) => !declaredWritable.has(root))
  if (extraWritable.length > 0) {
    return refuse(manifest, "undeclared_writable_root", extraWritable.join(","))
  }
  return Result.succeed(manifest)
}
