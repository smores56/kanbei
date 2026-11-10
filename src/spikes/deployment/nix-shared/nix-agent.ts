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

const run = async (
  argv: ReadonlyArray<string>,
  config: NixConfig,
  timeoutMs: number,
): Promise<CommandResult> => {
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
      NIX_CONFIG:
        "build-users-group =\nsandbox = false\nexperimental-features = nix-command flakes",
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
}

const main = async (): Promise<void> => {
  const raw = Deno.env.get("KANBEI_NIX_CONFIG")
  if (raw === undefined) {
    emit({ fatal: "missing KANBEI_NIX_CONFIG" })
    return
  }
  const config = JSON.parse(raw) as NixConfig
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
    : { success: false, code: 1, stdout: "", stderr: "build failed" }
  const direct = outPath === undefined
    ? { success: false, code: 1, stdout: "", stderr: "no output path" }
    : await run([`${outPath}/bin/hello`], config, 30_000)

  emit({
    abi,
    build: {
      success: build.success,
      code: build.code,
      stdout: build.stdout.trim().slice(0, 1000),
      stderr: build.stderr.trim().slice(0, 2000),
    },
    shell: {
      success: shell.success,
      code: shell.code,
      stdout: shell.stdout.trim().slice(0, 1000),
      stderr: shell.stderr.trim().slice(0, 2000),
    },
    direct: {
      success: direct.success,
      code: direct.code,
      stdout: direct.stdout.trim().slice(0, 1000),
      stderr: direct.stderr.trim().slice(0, 2000),
    },
    hello: direct.stdout.trim() === "Hello, world!",
  })
}

await main()
