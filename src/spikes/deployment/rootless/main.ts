import { Effect } from "effect"

interface LandingResult {
  readonly abi?: number
  readonly allowedRead?: boolean
  readonly deniedReadBlocked?: boolean
  readonly allowedWrite?: boolean
  readonly deniedWriteBlocked?: boolean
  readonly brokerFdBlocked?: boolean
  readonly brokerSignalBlocked?: boolean
  readonly brokerSocketReachable?: boolean
  readonly systemdControlBlocked?: boolean
  readonly procSelfRootBlocked?: boolean
  readonly details?: Record<string, string>
  readonly fatal?: string
}

const sleep = (millis: number): Promise<void> =>
  new Promise((resolve) => setTimeout(resolve, millis))

const waitForFile = (path: string, timeoutMs: number): Promise<string> => {
  const deadline = Date.now() + timeoutMs
  const poll = async (): Promise<string> => {
    try {
      return await Deno.readTextFile(path)
    } catch {
      if (Date.now() >= deadline) throw new Error(`timed out waiting for ${path}`)
      await sleep(50)
      return poll()
    }
  }
  return poll()
}

const lastJsonLine = (text: string): LandingResult => {
  const lines = text.trim().split("\n").toReversed()
  for (const line of lines) {
    try {
      return JSON.parse(line) as LandingResult
    } catch {
      // Continue to the next line.
    }
  }
  return { fatal: "no JSON result" }
}

const runSpike = async (): Promise<Record<string, unknown>> => {
  const root = `${Deno.cwd()}/tmp/a2b`
  await Deno.remove(root, { recursive: true }).catch(() => undefined)
  await Deno.mkdir(`${root}/allowed`, { recursive: true })
  await Deno.mkdir(`${root}/denied`, { recursive: true })
  await Deno.mkdir(`${root}/scratch/deno-cache`, { recursive: true })
  await Deno.mkdir(`${root}/broker`, { recursive: true })
  await Deno.writeTextFile(`${root}/allowed/allowed.txt`, "allowed\n")
  await Deno.writeTextFile(`${root}/denied/secret.txt`, "same-uid-secret\n")
  await Deno.writeTextFile(`${root}/broker/secret.txt`, "broker-credential-canary\n")

  const rootlessDir = `${Deno.cwd()}/src/spikes/deployment/rootless`
  const brokerScript = `${rootlessDir}/broker.ts`
  const brokerSocketDir = `/dev/shm/kanbei-a2b-${Deno.pid}`
  const sandboxRoot = `/dev/shm/kanbei-a2b-root-${Deno.pid}`
  await Deno.remove(brokerSocketDir, { recursive: true }).catch(() => undefined)
  await Deno.mkdir(brokerSocketDir, { recursive: true })
  const brokerSocket = `${brokerSocketDir}/broker.sock`
  const ready = `${root}/broker/ready.json`
  const broker = new Deno.Command(Deno.execPath(), {
    args: [
      "run",
      "--allow-all",
      brokerScript,
      `${root}/broker/secret.txt`,
      brokerSocket,
      ready,
    ],
    stdout: "piped",
    stderr: "piped",
  }).spawn()

  try {
    const readyText = await waitForFile(ready, 5000)
    const readyData = JSON.parse(readyText) as { pid: number; socket: string }
    const config = {
      brokerPid: readyData.pid,
      brokerSocket: "/broker/broker.sock",
      allowedFile: "/allowed/allowed.txt",
      deniedFile: "/denied/secret.txt",
      writeAllowedDir: "/scratch",
      deniedWritePath: "/denied/escape.txt",
      hostEscapePath: `/proc/self/root${root}/denied/secret.txt`,
      rules: [
        { path: "/nix/store", access: "read" },
        { path: "/run/current-system", access: "read" },
        { path: "/etc", access: "read" },
        { path: "/proc", access: "read" },
        { path: "/allowed", access: "read" },
        { path: "/scratch", access: "write" },
        { path: "/broker", access: "write" },
      ],
    }
    const uid = Deno.uid() ?? 1000
    const inner = [
      "set -e",
      `ROOT=${sandboxRoot}`,
      `rm -rf "$ROOT"`,
      `mkdir -p "$ROOT"/{nix/store,run/current-system,etc,proc,allowed,denied,scratch,broker,rootless}`,
      `mount --bind /nix/store "$ROOT/nix/store"`,
      `mount --bind /run/current-system "$ROOT/run/current-system"`,
      `mount --bind /etc "$ROOT/etc"`,
      `mount --bind ${root}/allowed "$ROOT/allowed"`,
      `mount --bind ${root}/denied "$ROOT/denied"`,
      `mount --bind ${root}/scratch "$ROOT/scratch"`,
      `mount --bind ${brokerSocketDir} "$ROOT/broker"`,
      `mount --bind ${rootlessDir} "$ROOT/rootless"`,
      `mount -t proc proc "$ROOT/proc"`,
      `exec /run/current-system/sw/bin/chroot "$ROOT" "${Deno.execPath()}" run --no-check --allow-all /rootless/agent-entry.ts`,
    ].join("; ")
    const sandbox = new Deno.Command("unshare", {
      args: ["-Urpf", "--mount-proc", "bash", "-c", inner],
      env: {
        ...Deno.env.toObject(),
        HOME: "/scratch",
        DENO_DIR: "/scratch/deno-cache",
        KANBEI_A2B_CONFIG: JSON.stringify(config),
      },
      stdout: "piped",
      stderr: "piped",
    }).spawn()
    const output = await sandbox.output()
    const landing = lastJsonLine(new TextDecoder().decode(output.stdout))
    const stderr = new TextDecoder().decode(output.stderr)
    return {
      schemaVersion: 1,
      task: "A2b",
      sandbox: {
        mechanism: "rootless user+PID+mount namespaces + mounted root + Landlock",
        uid,
        brokerPid: readyData.pid,
        brokerSocket,
        processExit: output.code,
        processSuccess: output.success,
        stderr: stderr.slice(0, 1000),
      },
      landing,
      checks: {
        allowedRead: landing.allowedRead === true,
        deniedReadBlocked: landing.deniedReadBlocked === true,
        allowedWrite: landing.allowedWrite === true,
        deniedWriteBlocked: landing.deniedWriteBlocked === true,
        brokerFdBlocked: landing.brokerFdBlocked === true,
        brokerSignalBlocked: landing.brokerSignalBlocked === true,
        brokerSocketReachable: landing.brokerSocketReachable === true,
        systemdControlBlocked: landing.systemdControlBlocked === true,
        procSelfRootBlocked: landing.procSelfRootBlocked === true,
      },
      verdict: landing.allowedRead === true &&
          landing.deniedReadBlocked === true &&
          landing.allowedWrite === true &&
          landing.deniedWriteBlocked === true &&
          landing.brokerFdBlocked === true &&
          landing.brokerSignalBlocked === true &&
          landing.brokerSocketReachable === true &&
          landing.systemdControlBlocked === true &&
          landing.procSelfRootBlocked === true
        ? "pass"
        : "needs-fixes",
    }
  } finally {
    try {
      broker.kill("SIGKILL")
    } catch {
      // The broker may have been killed by the sandbox signal probe.
    }
    await Deno.remove(brokerSocketDir, { recursive: true }).catch(() => undefined)
    await Deno.remove(sandboxRoot, { recursive: true }).catch(() => undefined)
  }
}

const program = Effect.tryPromise({
  try: runSpike,
  catch: (error) => error,
})

const result = await Effect.runPromise(program)
await Deno.writeTextFile(
  `${Deno.cwd()}/spikes/deployment/rootless-result.json`,
  `${JSON.stringify(result, null, 2)}\n`,
)
await Deno.stdout.write(new TextEncoder().encode(`${JSON.stringify(result, null, 2)}\n`))
