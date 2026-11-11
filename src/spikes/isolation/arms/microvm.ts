import { Effect } from "effect"
import type { ArmResult, HostCapabilities } from "../types.ts"

/**
 * The microVM arm is only measurable when a microVM runner and a sealed guest
 * image exist. A general-purpose VMM binary is not a microVM runtime.
 */
export const measureMicrovmArm = (
  host: HostCapabilities,
  supervisorLoc: number,
): Effect.Effect<ArmResult> =>
  Effect.succeed(
    host.microvmRuntimes.length === 0
      ? {
        arm: "microvm" as const,
        status: "blocked" as const,
        scope:
          "No check, metric, or operability measurement ran; the microVM arm is blocked on a runtime and a sealed guest image",
        blockedReason:
          `no microVM runtime on PATH (firecracker, cloud-hypervisor, microvm all absent; general VMM found: ${
            host.generalVmm.length === 0 ? "none" : host.generalVmm.join(", ")
          }); /dev/kvm is ${host.kvmDevice}; no sealed guest kernel, initrd, or rootfs exists in the repository, and the host has no toolchain to build one`,
        checks: [],
        operability: {
          hostDependencies: [
            "firecracker or cloud-hypervisor",
            "sealed guest kernel and initrd",
            "guest rootfs build pipeline",
          ],
          requiresRoot: false,
          supervisorLoc,
          packaging: "One microVM per agent with a pre-built, sealed guest image",
          debuggability: "Serial console and guest agent only; no host-side process introspection",
          limitations: [
            "Every agent code or runtime change requires rebuilding and resealing the guest image",
            "Guest images are additional release artifacts to store, verify, and roll back",
            "Not measured in this spike; no containment or density claim is made",
          ],
        },
        notes: [
          "Blocked, not inconclusive: no measurement was attempted because the mechanism is not installable without a guest image",
        ],
      }
      : {
        arm: "microvm" as const,
        status: "inconclusive" as const,
        scope:
          "No containment or density measurement ran; a runtime exists but no sealed guest image is available",
        blockedReason: `microvm runtimes present (${
          host.microvmRuntimes.join(", ")
        }) but no guest image is sealed for this spike`,
        checks: [],
        operability: {
          hostDependencies: [...host.microvmRuntimes, "sealed guest kernel and initrd"],
          requiresRoot: false,
          supervisorLoc,
          packaging: "One microVM per agent with a pre-built, sealed guest image",
          debuggability: "Serial console and guest agent only",
          limitations: ["No guest image available, so nothing was measured"],
        },
        notes: ["Runtime present but the spike has no sealed guest image; no claim is made"],
      },
  )
