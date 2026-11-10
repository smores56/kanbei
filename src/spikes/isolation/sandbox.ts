/** A2b mount/chroot plan: per-agent namespace, read-only runtime mounts, scratch, and `/agent`. */
export interface SandboxPlan {
  readonly sandboxRoot: string
  readonly scratchDir: string
  readonly sourceDir: string
  readonly denoPath: string
  readonly entryScript: string
  readonly agentScript: string
  readonly environment: Readonly<Record<string, string>>
}

export interface SandboxCommand {
  readonly command: string
  readonly args: ReadonlyArray<string>
  readonly env: Readonly<Record<string, string>>
}

const readOnlyMounts: ReadonlyArray<readonly [string, string]> = [
  ["/nix/store", "nix/store"],
  ["/run/current-system", "run/current-system"],
  ["/etc", "etc"],
]

const mountLines = (plan: SandboxPlan): ReadonlyArray<string> => [
  ...readOnlyMounts.flatMap(([source, target]) => [
    `mount --bind ${source} "$ROOT/${target}"`,
    `mount -o remount,bind,ro "$ROOT/${target}"`,
  ]),
  `mount --bind ${plan.sourceDir} "$ROOT/agent"`,
  `mount -o remount,bind,ro "$ROOT/agent"`,
  `mount --bind ${plan.scratchDir} "$ROOT/scratch"`,
  `mount -t proc proc "$ROOT/proc"`,
]

export const sandboxScript = (plan: SandboxPlan): string =>
  [
    "set -e",
    `ROOT=${plan.sandboxRoot}`,
    'rm -rf "$ROOT"',
    'mkdir -p "$ROOT"/{nix/store,run/current-system,etc,proc,agent,scratch}',
    ...mountLines(plan),
    "ulimit -c 0",
    "ulimit -n 256",
    "ulimit -f 16384",
    `exec chroot "$ROOT" ${plan.denoPath} run --allow-ffi --allow-env ${plan.entryScript} ${plan.agentScript}`,
  ].join("; ")

export const sandboxCommand = (plan: SandboxPlan): SandboxCommand => ({
  command: "unshare",
  args: ["-Urpf", "--mount-proc", "bash", "-c", sandboxScript(plan)],
  env: {
    PATH: "/run/current-system/sw/bin",
    HOME: "/scratch",
    DENO_DIR: "/scratch/deno-cache",
    ...plan.environment,
  },
})

/** Host path -> path inside the chroot, given the bind-mounted source root. */
export const sandboxPath = (sourceDir: string, hostPath: string): string => {
  const prefix = sourceDir.endsWith("/") ? sourceDir : `${sourceDir}/`
  return hostPath.startsWith(prefix) ? `/agent/${hostPath.slice(prefix.length)}` : hostPath
}
