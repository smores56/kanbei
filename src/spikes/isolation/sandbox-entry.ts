/**
 * Trusted preload: apply the Landlock ruleset, then replace this image with the
 * agent so it runs with exactly the four LANG-1 capability operations: read the
 * mounted `/etc`, write `/scratch`, spawn a process, and reach the loopback
 * endpoint the workload probe uses. Landlock still narrows the paths.
 */
import { applyLandlock, execveProcess } from "../deployment/rootless/landlock.ts"

const encoder = new TextEncoder()

const rules = [
  { path: "/nix/store", access: "read" as const },
  { path: "/run/current-system", access: "read" as const },
  { path: "/etc", access: "read" as const },
  { path: "/proc", access: "read" as const },
  { path: "/agent", access: "read" as const },
  { path: "/scratch", access: "write" as const },
]

const agent = Deno.args[0]

if (agent === undefined) {
  Deno.stderr.writeSync(encoder.encode("sandbox-entry: missing agent script\n"))
  Deno.exit(2)
}

try {
  applyLandlock(rules)
} catch (error) {
  Deno.stderr.writeSync(encoder.encode(`sandbox-entry: landlock failed: ${String(error)}\n`))
  Deno.exit(3)
}

execveProcess(
  Deno.execPath(),
  [
    "run",
    "--allow-env",
    "--allow-run",
    "--allow-read=/etc,/scratch",
    "--allow-write=/scratch",
    "--allow-net=127.0.0.1:9",
    agent,
  ],
  Deno.env.toObject(),
)
