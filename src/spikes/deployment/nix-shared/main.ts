import { Effect } from "effect"

interface StepResult {
  readonly success: boolean
  readonly code: number
  readonly stdout: string
  readonly stderr: string
}

interface NixResult {
  readonly abi?: number
  readonly build?: StepResult
  readonly shell?: StepResult
  readonly direct?: StepResult
  readonly run?: StepResult
  readonly profile?: StepResult
  readonly profileList?: StepResult
  readonly profileExec?: StepResult
  readonly hello?: boolean
  readonly fatal?: string
}

const lastJsonLine = (text: string): NixResult => {
  const lines = text.trim().split("\n").toReversed()
  for (const line of lines) {
    try {
      return JSON.parse(line) as NixResult
    } catch {
      // Continue.
    }
  }
  return { fatal: "no JSON result" }
}

const command = async (
  argv: ReadonlyArray<string>,
): Promise<{ success: boolean; stdout: string; stderr: string }> => {
  const output = await new Deno.Command(argv[0] ?? "", {
    args: argv.slice(1),
    stdout: "piped",
    stderr: "piped",
  }).output()
  return {
    success: output.success,
    stdout: new TextDecoder().decode(output.stdout),
    stderr: new TextDecoder().decode(output.stderr),
  }
}

const realPath = async (path: string): Promise<string> => await Deno.realPath(path)

const closure = async (path: string): Promise<ReadonlyArray<string>> => {
  const result = await command(["/run/current-system/sw/bin/nix-store", "-qR", path])
  if (!result.success) throw new Error(`nix-store -qR ${path}: ${result.stderr}`)
  return result.stdout.trim().split("\n").filter((line) => line !== "")
}

const copyStorePath = async (storeRoot: string, source: string): Promise<void> => {
  const storePath = source.split("/").slice(0, 4).join("/")
  const target = `${storeRoot}${storePath}`
  const exists = await Deno.stat(target).then(() => true).catch(() => false)
  if (exists) return
  const copied = await command([
    "/run/current-system/sw/bin/cp",
    "-a",
    "--no-preserve=ownership",
    storePath,
    `${storeRoot}/nix/store/`,
  ])
  if (!copied.success) throw new Error(`cannot seed ${storePath}: ${copied.stderr.slice(0, 500)}`)
}

const bootstrap = async (storeRoot: string): Promise<void> => {
  const marker = `${storeRoot}/.bootstrapped`
  const exists = await Deno.stat(marker).then(() => true).catch(() => false)
  if (exists) return
  await Deno.mkdir(`${storeRoot}/nix/store`, { recursive: true })
  const nixPath = await realPath("/run/current-system/sw/bin/nix")
  const bashPath = await realPath("/run/current-system/sw/bin/bash")
  const coreutilsPath = await realPath("/run/current-system/sw/bin/mkdir")
  const denoPath = await realPath(Deno.execPath())
  const etcStore = (await realPath("/etc/static")).split("/").slice(0, 4).join("/")
  const paths = [
    ...new Set([
      ...(await closure(nixPath)),
      ...(await closure(bashPath)),
      ...(await closure(coreutilsPath)),
    ]),
  ]
  const copy = await command([
    "/run/current-system/sw/bin/nix",
    "--extra-experimental-features",
    "nix-command",
    "copy",
    "--to",
    `local?root=${storeRoot}`,
    ...paths,
  ])
  if (!copy.success) throw new Error(`shared-store bootstrap failed: ${copy.stderr.slice(0, 2000)}`)

  // NixOS /etc and its CA bundle are generated store paths, not host DB entries.
  await copyStorePath(storeRoot, `${etcStore}/placeholder`)
  await copyStorePath(
    storeRoot,
    `${
      (await realPath("/etc/ssl/certs/ca-bundle.crt")).split("/").slice(0, 4).join("/")
    }/placeholder`,
  )

  // Deno is present in the host store but not registered in the host Nix DB.
  // Seed it and its shared-library dependencies directly.
  await copyStorePath(storeRoot, denoPath)
  const ldd = await command(["/run/current-system/sw/bin/ldd", denoPath])
  const dependencies = ldd.stdout
    .split("\n")
    .map((line) => /\/nix\/store\/[^/]+/u.exec(line)?.[0])
    .filter((path): path is string => path !== undefined)
  await Promise.all(
    dependencies.map((dependency) => copyStorePath(storeRoot, `${dependency}/placeholder`)),
  )
  await Deno.writeTextFile(marker, `${new Date().toISOString()}\n`)
}

const runSpike = async (): Promise<Record<string, unknown>> => {
  const storeRoot = `${Deno.cwd()}/tmp/kanbei-shared-nix`
  const scratchRoot = `${Deno.cwd()}/tmp/kanbei-nix-shared-scratch`
  const sandboxRoot = `/dev/shm/kanbei-nix-shared-root-${Deno.pid}`
  const rootlessDir = `${Deno.cwd()}/src/spikes/deployment/rootless`
  const nixDir = `${Deno.cwd()}/src/spikes/deployment/nix-shared`
  await Deno.mkdir(`${scratchRoot}/cache`, { recursive: true })
  await Deno.mkdir(`${scratchRoot}/deno-cache`, { recursive: true })
  await bootstrap(storeRoot)
  await Deno.remove(sandboxRoot, { recursive: true }).catch(() => undefined)

  const nixPath = await realPath("/run/current-system/sw/bin/nix")
  const bashPath = await realPath("/run/current-system/sw/bin/bash")
  const coreutilsPath = await realPath("/run/current-system/sw/bin/mkdir")
  const denoPath = await realPath(Deno.execPath())
  const config = {
    buildTimeoutMs: 180_000,
    nixPath,
    bashPath,
    coreutilsPath: `${coreutilsPath.replace(/\/mkdir$/, "")}/coreutils`,
    denoPath,
    rules: [
      { path: "/nix/store", access: "write" },
      { path: "/nix/var", access: "write" },
      { path: "/etc", access: "read" },
      { path: "/run/systemd/resolve", access: "read" },
      { path: "/proc", access: "read" },
      { path: "/tmp", access: "write" },
      { path: "/scratch", access: "write" },
      { path: "/nix-shared", access: "read" },
      // Nix re-anchors its root as openat("/proc/self/root") + chroot(".").
      // Landlock resolves that magic link to "/", so without a rule on the
      // sandbox root the round-trip is EACCES and Nix skips the chroot.
      { path: "/", access: "read" },
    ],
  }
  const inner = [
    "set -e",
    `ROOT=${sandboxRoot}`,
    `rm -rf "$ROOT"`,
    `mkdir -p "$ROOT"/{nix/store,nix/var,etc,run/systemd/resolve,proc,tmp,scratch,rootless,nix-shared}`,
    `mount --bind ${storeRoot}/nix/store "$ROOT/nix/store"`,
    `mount --bind ${storeRoot}/nix/var "$ROOT/nix/var"`,
    `mount --bind /etc "$ROOT/etc"`,
    `mount --bind /run/systemd/resolve "$ROOT/run/systemd/resolve"`,
    `mount -t proc proc "$ROOT/proc"`,
    `mount -t tmpfs tmpfs "$ROOT/tmp"`,
    `chmod 1777 "$ROOT/tmp"`,
    `mount --bind ${scratchRoot} "$ROOT/scratch"`,
    `mount --bind ${rootlessDir} "$ROOT/rootless"`,
    `mount --bind ${nixDir} "$ROOT/nix-shared"`,
    `exec /run/current-system/sw/bin/chroot "$ROOT" "${denoPath}" run --no-check --allow-all /nix-shared/nix-agent.ts`,
  ].join("; ")

  try {
    const output = await new Deno.Command("unshare", {
      args: ["-Urpf", "--mount-proc", "bash", "-c", inner],
      env: {
        ...Deno.env.toObject(),
        KANBEI_NIX_CONFIG: JSON.stringify(config),
      },
      stdout: "piped",
      stderr: "piped",
    }).output()
    const result = lastJsonLine(new TextDecoder().decode(output.stdout))
    const stderr = new TextDecoder().decode(output.stderr)
    const outPath = result.build?.stdout.split("\n").find((line) =>
      line.startsWith("/nix/store/")
    ) ?? ""
    const hostStoreHasPath = outPath === "" ? false : await Deno.stat(outPath).then(() => true)
      .catch(() => false)
    const sharedPath = outPath === "" ? "" : `${storeRoot}${outPath}`
    const sharedStoreHasPath = sharedPath === ""
      ? false
      : await Deno.stat(sharedPath).then(() => true)
        .catch(() => false)
    const checks = {
      build: result.build?.success === true,
      direct: result.direct?.success === true,
      shell: result.shell?.success === true,
      run: result.run?.success === true,
      profile: result.profile?.success === true,
      profileList: result.profileList?.success === true,
      profileExec: result.profileExec?.success === true,
      hello: result.hello === true,
      hostStoreUnchanged: !hostStoreHasPath,
      sharedStoreHasPath,
    }
    return {
      schemaVersion: 1,
      task: "A2c",
      sharedStore: storeRoot,
      sandbox: {
        processExit: output.code,
        processSuccess: output.success,
        stderr: stderr.slice(0, 1000),
      },
      result,
      checks,
      // Deriving the verdict from the check record keeps the two from drifting;
      // a check that does not gate the verdict is not a check.
      verdict: Object.values(checks).every(Boolean) ? "pass" : "needs-fixes",
    }
  } finally {
    await Deno.remove(sandboxRoot, { recursive: true }).catch(() => undefined)
  }
}

const program = Effect.tryPromise({
  try: runSpike,
  catch: (error) => error,
})

const result = await Effect.runPromise(program)
await Deno.writeTextFile(
  `${Deno.cwd()}/spikes/deployment/nix-shared-result.json`,
  `${JSON.stringify(result, null, 2)}\n`,
)
await Deno.stdout.write(new TextEncoder().encode(`${JSON.stringify(result, null, 2)}\n`))
