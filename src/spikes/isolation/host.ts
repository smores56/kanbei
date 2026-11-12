import { Effect } from "effect"
import { probeLandlockAbi } from "../deployment/rootless/landlock.ts"
import { pathExists, readTextFile, runCommand, runShell } from "./exec.ts"
import { CGROUP_MOUNT, cgroupMountOptions, parseControllers } from "./supervisor.ts"
import type { Supervisor } from "./supervisor.ts"
import type { HostCapabilities } from "./types.ts"

const controllers = (): Effect.Effect<ReadonlyArray<string>, never> =>
  readTextFile(`${CGROUP_MOUNT}/cgroup.controllers`).pipe(
    Effect.map((text) => parseControllers(text ?? "")),
    Effect.catch(() => Effect.succeed([])),
  )

const memTotalBytes = (): Effect.Effect<number, never> =>
  readTextFile("/proc/meminfo").pipe(
    Effect.map((text) => {
      const match = /^MemTotal:\s+(\d+)\s+kB$/mu.exec(text ?? "")
      return match?.[1] === undefined ? 0 : Number(match[1]) * 1024
    }),
    Effect.catch(() => Effect.succeed(0)),
  )

/** Unprivileged user namespaces are the substrate; there is no privileged fallback. */
const userNamespaces = (): Effect.Effect<boolean, never> =>
  runCommand("unshare", ["-Urpf", "true"]).pipe(
    Effect.map((output) => output.code === 0),
    Effect.catch(() => Effect.succeed(false)),
  )

const landlockAbi = (): Effect.Effect<number, never> =>
  Effect.try({ try: () => probeLandlockAbi(), catch: () => 0 }).pipe(
    Effect.catch(() => Effect.succeed(0)),
  )

const delegatedParentOwned = (supervisor: Supervisor): Effect.Effect<boolean, never> =>
  Effect.tryPromise({
    try: async () => {
      const info = await Deno.stat(`${CGROUP_MOUNT}${supervisor.cgroupRoot}`)
      return info.uid === Deno.uid()
    },
    catch: () => false,
  }).pipe(Effect.catch(() => Effect.succeed(false)))

const kvmDevice = (): Effect.Effect<HostCapabilities["kvmDevice"], never> =>
  Effect.gen(function* () {
    const exists = yield* pathExists("/dev/kvm").pipe(Effect.catch(() => Effect.succeed(false)))
    if (!exists) return "absent" as const
    return yield* Effect.tryPromise({
      try: async () => {
        const handle = await Deno.open("/dev/kvm", { read: true })
        handle.close()
        return "accessible" as const
      },
      catch: () => "present-not-accessible" as const,
    }).pipe(Effect.catch(() => Effect.succeed("present-not-accessible" as const)))
  })

const splitPaths = (text: string): ReadonlyArray<string> =>
  text.split("\n").map((line) => line.trim()).filter((line) => line !== "")

const virtualisationBinaries = (): Effect.Effect<
  readonly [ReadonlyArray<string>, ReadonlyArray<string>],
  never
> =>
  runShell(
    'for b in firecracker cloud-hypervisor microvm; do command -v "$b"; done; echo ---; for b in qemu-system-x86_64; do command -v "$b"; done',
  ).pipe(
    Effect.map((output) => {
      const [microvmPart = "", generalPart = ""] = output.stdout.split("---")
      return [splitPaths(microvmPart), splitPaths(generalPart)] as const
    }),
    Effect.catch(() => Effect.succeed([[], []] as const)),
  )

export const probeHost = (supervisor: Supervisor): Effect.Effect<HostCapabilities> =>
  Effect.gen(function* () {
    const cgroupControllers = yield* controllers()
    const namespaces = yield* userNamespaces()
    const abi = yield* landlockAbi()
    const mountOptions = yield* readTextFile("/proc/self/mountinfo").pipe(
      Effect.catch(() => Effect.succeed(null)),
    )
    const owned = yield* delegatedParentOwned(supervisor)
    const kvm = yield* kvmDevice()
    const [microvmRuntimes, generalVmm] = yield* virtualisationBinaries()
    const totalMemoryBytes = yield* memTotalBytes()
    return {
      kernel: `${Deno.build.os}-${Deno.osRelease()}`,
      runtime: `deno@${Deno.version.deno}`,
      cpus: navigator.hardwareConcurrency,
      totalMemoryBytes,
      cgroupVersion: cgroupControllers.length > 0 ? "v2" : "none",
      cgroupControllers: [...cgroupControllers],
      userNamespaces: namespaces ? "available" : "unavailable",
      landlockAbi: abi,
      cgroupNsdelegate: cgroupMountOptions(mountOptions ?? "").includes("nsdelegate"),
      delegatedParent: owned ? "owned-by-uid" : "not-owned",
      kvmDevice: kvm,
      microvmRuntimes: [...microvmRuntimes],
      generalVmm: [...generalVmm],
      wasmEngine: `v8@${Deno.version.v8} (core wasm, no WASI runtime on PATH)`,
      privilegedOperations: false,
    }
  })

export const memoryAvailableBytes: Effect.Effect<number> = readTextFile("/proc/meminfo").pipe(
  Effect.map((text) => {
    const match = /^MemAvailable:\s+(\d+)\s+kB$/mu.exec(text ?? "")
    return match?.[1] === undefined ? 0 : Number(match[1]) * 1024
  }),
  Effect.catch(() => Effect.succeed(0)),
)
