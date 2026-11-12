import { assert, assertEquals } from "@std/assert"
import {
  forkBombScript,
  markerArgs,
  sleepForeverScript,
  WORKLOAD_OK,
  workloadReplyHolds,
} from "../../../src/spikes/isolation/agent-contract.ts"

Deno.test("workload passes only on the exact success line", () => {
  assertEquals(workloadReplyHolds("WORKLOAD ok"), true)
  assertEquals(workloadReplyHolds("  WORKLOAD ok\n"), true)
  assertEquals(workloadReplyHolds("WORKLOAD fail read /etc: PermissionDenied"), false)
  assertEquals(workloadReplyHolds("WORKLOAD ok extra"), false)
  assertEquals(workloadReplyHolds("PONG"), false)
  assertEquals(WORKLOAD_OK, "WORKLOAD ok")
})

Deno.test("marker child keeps the marker in its own argv after exec", async () => {
  const directory = await Deno.makeTempDir()
  const output = `${directory}/cmdline`
  const script = `cat /proc/$$/cmdline > "$KANBEI_TEST_OUT"; ${sleepForeverScript(30)}`
  const marker = `kanbei-marker-${crypto.randomUUID()}`
  const child = new Deno.Command("bash", {
    args: [...markerArgs(marker, script)],
    env: { KANBEI_TEST_OUT: output },
    stdin: "null",
    stdout: "null",
    stderr: "null",
  }).spawn()
  try {
    const deadline = Date.now() + 5_000
    const waitForOutput = async (): Promise<string> => {
      if (Date.now() >= deadline) return ""
      try {
        return await Deno.readTextFile(output)
      } catch {
        await new Promise((resolve) => setTimeout(resolve, 25))
        return await waitForOutput()
      }
    }
    const cmdline = await waitForOutput()
    assert(cmdline.includes(marker), `cmdline ${JSON.stringify(cmdline)} lacks ${marker}`)
  } finally {
    child.kill("SIGKILL")
    await child.status
    await Deno.remove(directory, { recursive: true })
  }
})

Deno.test("fork bomb script makes every attempt marker-carrying", () => {
  const script = forkBombScript(200)
  assert(script.includes("-lt 200"))
  assert(script.includes('exec -a "$0" bash -c "while :; do sleep 30; done"'))
})
