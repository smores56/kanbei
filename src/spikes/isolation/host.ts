import { Effect } from "effect"
import { pathExists, readNumericFile, readTextFile, runCommand, runShell } from "./exec.ts"
import type { HostCapabilities } from "./types.ts"

const CGROUP_ROOT = "/sys/fs/cgroup"

const controllers = (): Effect.Effect<ReadonlyArray<string>, never> =>
  readTextFile(`${CGROUP_ROOT}/cgroup.controllers`).pipe(
    Effect.map((text) => (text ?? "").trim().split(/\s+/u).filter((item) => item !== "")),
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

const systemdDelegation = (): Effect.Effect<readonly [boolean, string], never> => {
  const unit = `kanbei-iso-probe-${Deno.pid}`
  return runCommand("systemd-run", [
    "--user",
    "--unit",
    unit,
    "--collect",
    "--pipe",
    "--wait",
    "--quiet",
    "--property",
    "MemoryMax=32M",
    "--property",
    "TasksMax=8",
    "true",
  ]).pipe(
    Effect.map((output) =>
      output.code === 0
        ? ([true, `systemd-run --user accepted MemoryMax=32M TasksMax=8 (exit 0)`] as const)
        : ([false, `systemd-run --user rejected the probe (exit ${output.code})`] as const)
    ),
    Effect.catch((error) => Effect.succeed([false, error.detail] as const)),
  )
}

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

/** MicroVM runtimes are counted only when their binary is on PATH. */
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

export const probeHost: Effect.Effect<HostCapabilities> = Effect.gen(function* () {
  const cgroupControllers = yield* controllers()
  const [delegated, systemdProbe] = yield* systemdDelegation()
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
    systemdUserDelegation: delegated ? "available" : "unavailable",
    systemdProbe,
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

export const cgroupCurrentBytes = (cgroupPath: string): Effect.Effect<number> =>
  readNumericFile(`${CGROUP_ROOT}${cgroupPath}/memory.current`).pipe(
    Effect.map((value) => value ?? 0),
    Effect.catch(() => Effect.succeed(0)),
  )

export const cgroupPidsCurrent = (cgroupPath: string): Effect.Effect<number> =>
  readNumericFile(`${CGROUP_ROOT}${cgroupPath}/pids.current`).pipe(
    Effect.map((value) => value ?? 0),
    Effect.catch(() => Effect.succeed(0)),
  )

export const cgroupPidsMax = (cgroupPath: string): Effect.Effect<number | null> =>
  readNumericFile(`${CGROUP_ROOT}${cgroupPath}/pids.max`).pipe(
    Effect.catch(() => Effect.succeed(null)),
  )

export const cgroupExists = (cgroupPath: string): Effect.Effect<boolean> =>
  pathExists(`${CGROUP_ROOT}${cgroupPath}`).pipe(Effect.catch(() => Effect.succeed(false)))
