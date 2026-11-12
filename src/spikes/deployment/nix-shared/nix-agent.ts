import { applyLandlock, emit } from "../rootless/landlock.ts"
import type { LandlockRule } from "../rootless/landlock.ts"

interface NixConfig {
  readonly rules: ReadonlyArray<LandlockRule>
  readonly buildTimeoutMs: number
  readonly nixPath: string
  readonly bashPath: string
  readonly coreutilsPath: string
  readonly denoPath: string
}

interface CommandResult {
  readonly success: boolean
  readonly code: number
  readonly stdout: string
  readonly stderr: string
}

const CONF_DIR = "/scratch/nix-conf"
const PROFILE = "/scratch/prof"

// The sandbox sees /etc from the host, where /etc/nix/nix.conf dangles into the
// host store. An explicit conf dir makes the store part of the probe's own
// contract instead of relying on Nix's compiled-in defaults. `store` takes a
// store URI, not a directory: `/nix/store` parses as a chroot store rooted
// there, so `local` is the explicit spelling of the default local store. There
// is no `state` setting in Nix 2.34.8; NIX_STATE_DIR carries it instead.
const NIX_CONF = [
  "store = local",
  "build-users-group =",
  "sandbox = false",
  "experimental-features = nix-command flakes",
].join("\n")

const writeConf = async (): Promise<void> => {
  await Deno.mkdir(CONF_DIR, { recursive: true })
  await Deno.writeTextFile(`${CONF_DIR}/nix.conf`, `${NIX_CONF}\n`)
}

const run = async (
  argv: ReadonlyArray<string>,
  config: NixConfig,
  timeoutMs: number,
): Promise<CommandResult> => {
  try {
    const child = new Deno.Command(argv[0] ?? "", {
      args: argv.slice(1),
      stdout: "piped",
      stderr: "piped",
      env: {
        ...Deno.env.toObject(),
        PATH: [
          config.nixPath.replace(/\/nix$/, ""),
          config.bashPath.replace(/\/bash$/, ""),
          config.coreutilsPath.replace(/\/coreutils$/, ""),
          config.denoPath.replace(/\/deno$/, ""),
        ].join(":"),
        NIX_REMOTE: "local",
        NIX_CONF_DIR: CONF_DIR,
        NIX_STATE_DIR: "/nix/var/nix",
        HOME: "/scratch",
        TMPDIR: "/tmp",
        XDG_CACHE_HOME: "/scratch/cache",
        DENO_DIR: "/scratch/deno-cache",
        SHELL: config.bashPath,
      },
    }).spawn()
    const timer = setTimeout(() => child.kill("SIGKILL"), timeoutMs)
    const output = await child.output()
    clearTimeout(timer)
    return {
      success: output.success,
      code: output.code,
      stdout: new TextDecoder().decode(output.stdout),
      stderr: new TextDecoder().decode(output.stderr),
    }
  } catch (error) {
    // A failed spawn is a probe result, not a crash: it must reach the record.
    return {
      success: false,
      code: 1,
      stdout: "",
      stderr: error instanceof Error ? error.message : String(error),
    }
  }
}

const skipped = (reason: string): CommandResult => ({
  success: false,
  code: 1,
  stdout: "",
  stderr: reason,
})

const report = (result: CommandResult): CommandResult => ({
  success: result.success,
  code: result.code,
  stdout: result.stdout.trim().slice(0, 2000),
  stderr: result.stderr.trim().slice(0, 2000),
})

const main = async (): Promise<void> => {
  const raw = Deno.env.get("KANBEI_NIX_CONFIG")
  if (raw === undefined) {
    emit({ fatal: "missing KANBEI_NIX_CONFIG" })
    return
  }
  const config = JSON.parse(raw) as NixConfig
  await writeConf()
  const abi = applyLandlock(config.rules)
  const build = await run(
    [config.nixPath, "build", "nixpkgs#hello", "--no-link", "--print-out-paths"],
    config,
    config.buildTimeoutMs,
  )
  const outPath = build.stdout.split("\n").find((line) => line.startsWith("/nix/store/"))
  const shell = build.success && outPath !== undefined
    ? await run(
      [config.nixPath, "shell", "nixpkgs#hello", "--command", `${outPath}/bin/hello`],
      config,
      config.buildTimeoutMs,
    )
    : skipped("build failed")
  const direct = outPath === undefined
    ? skipped("no output path")
    : await run([`${outPath}/bin/hello`], config, 30_000)
  const runHello = await run(
    [config.nixPath, "run", "nixpkgs#hello"],
    config,
    config.buildTimeoutMs,
  )
  const profile = await run(
    [config.nixPath, "profile", "add", "--profile", PROFILE, "nixpkgs#hello"],
    config,
    config.buildTimeoutMs,
  )
  const profileList = await run(
    [config.nixPath, "profile", "list", "--profile", PROFILE, "--json"],
    config,
    config.buildTimeoutMs,
  )
  const profileExec = await run([`${PROFILE}/bin/hello`], config, 30_000)

  emit({
    abi,
    build: report(build),
    shell: report(shell),
    direct: report(direct),
    run: report(runHello),
    profile: report(profile),
    profileList: report(profileList),
    profileExec: report(profileExec),
    hello: direct.stdout.trim() === "Hello, world!",
  })
}

await main()
