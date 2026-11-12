import { assertEquals, assertStringIncludes } from "@std/assert"
import {
  releasePath,
  sandboxCommand,
  sandboxPath,
  sandboxScript,
} from "../../../src/spikes/isolation/sandbox.ts"
import type { SandboxPlan } from "../../../src/spikes/isolation/sandbox.ts"

const plan: SandboxPlan = {
  sandboxRoot: "/repo/tmp/isolation/run/slot-1/root",
  scratchDir: "/repo/tmp/isolation/run/slot-1/scratch",
  sourceDir: "/repo/src",
  denoPath: "/nix/store/deno/bin/deno",
  entryScript: "/agent/spikes/isolation/sandbox-entry.ts",
  agentScript: "/agent/spikes/isolation/agent-process.ts",
  environment: { KANBEI_ISO_MARKER: "run-orphan" },
}

Deno.test("sandbox script mounts runtime read-only, scratch writable, and proc", () => {
  const script = sandboxScript(plan)
  for (
    const mount of [
      'mount --bind /nix/store "$ROOT/nix/store"',
      'mount --bind /run/current-system "$ROOT/run/current-system"',
      'mount --bind /etc "$ROOT/etc"',
      'mount --bind /repo/src "$ROOT/agent"',
      'mount --bind /repo/tmp/isolation/run/slot-1/scratch "$ROOT/scratch"',
      'mount -t proc proc "$ROOT/proc"',
    ]
  ) {
    assertStringIncludes(script, mount)
  }
  assertEquals(script.split("mount -o remount,bind,ro").length - 1, 4)
  assertEquals(script.includes("/sys"), false)
})

Deno.test("sandbox script applies rlimits and execs the Landlock preload", () => {
  const script = sandboxScript(plan)
  assertStringIncludes(script, "ulimit -c 0")
  assertStringIncludes(script, "ulimit -n 256")
  assertStringIncludes(script, "ulimit -f 16384")
  assertStringIncludes(
    script,
    'exec chroot "$ROOT" /nix/store/deno/bin/deno run --allow-ffi --allow-env /agent/spikes/isolation/sandbox-entry.ts /agent/spikes/isolation/agent-process.ts',
  )
})

Deno.test("sandbox command is an unprivileged user, PID, and mount namespace", () => {
  const command = sandboxCommand(plan)
  assertEquals(command.command, "unshare")
  assertEquals(command.args.slice(0, 3), ["-Urpf", "--mount-proc", "bash"])
  assertEquals(command.args[3], "-c")
  assertEquals(command.env["PATH"], "/run/current-system/sw/bin")
  assertEquals(command.env["HOME"], "/scratch")
  assertEquals(command.env["DENO_DIR"], "/scratch/deno-cache")
  assertEquals(command.env["KANBEI_ISO_MARKER"], "run-orphan")
})

Deno.test("sandboxPath maps repository files into the chroot and leaves others alone", () => {
  assertEquals(
    sandboxPath("/repo/src", "/repo/src/spikes/isolation/agent-process.ts"),
    "/agent/spikes/isolation/agent-process.ts",
  )
  assertEquals(sandboxPath("/repo/src", "/tmp/other.ts"), "/tmp/other.ts")
})

Deno.test("sandbox script blocks on the release file before exec", () => {
  const script = sandboxScript(plan)
  const rlimits = script.indexOf("ulimit -f 16384")
  const barrier = script.indexOf(`while [ ! -e ${plan.sandboxRoot}/scratch/.kanbei-release ]`)
  const exec = script.indexOf("exec chroot")
  assertEquals(rlimits >= 0 && rlimits < barrier, true)
  assertEquals(barrier < exec, true)
  assertEquals(
    releasePath("/repo/tmp/isolation/run/slot-1/scratch"),
    "/repo/tmp/isolation/run/slot-1/scratch/.kanbei-release",
  )
})
