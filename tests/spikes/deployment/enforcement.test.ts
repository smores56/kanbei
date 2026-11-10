import { assert, assertEquals } from "@std/assert"
import { Effect } from "effect"
import { failClosedProbes } from "../../../src/spikes/deployment/probes/fail-closed.ts"
import { landlockProbes } from "../../../src/spikes/deployment/probes/landlock.ts"
import { crashMidEffectProbe } from "../../../src/spikes/deployment/probes/recovery.ts"
import { runWithProbeEnv } from "./support.ts"

const landlockAbi = async (): Promise<number> => {
  const results = await runWithProbeEnv(landlockProbes)
  const abiProbe = results.find((result) => result.id === "LL-01")
  assert(abiProbe !== undefined)
  const abi = Number(/ABI=(\d+)/u.exec(abiProbe.observed)?.[1] ?? "0")
  return abi
}

Deno.test("Landlock grants the declared path and denies an unlisted path", async () => {
  const results = await runWithProbeEnv(landlockProbes)
  const byId = new Map(results.map((result) => [result.id, result]))
  const abi = byId.get("LL-01")
  assert(abi !== undefined)
  assertEquals(abi.status, "pass")

  const filesystem = byId.get("LL-02")
  assert(filesystem !== undefined)
  if (filesystem.status === "pass") {
    assert(filesystem.evidence.includes('"outsideOpened":false'))
    assert(filesystem.evidence.includes('"insideOpened":true'))
  } else {
    assertEquals(filesystem.status, "blocked")
  }
})

Deno.test("Landlock denies a TCP bind after a network rule, or records why it could not", async () => {
  const results = await runWithProbeEnv(landlockProbes)
  const network = results.find((result) => result.id === "LL-03")
  assert(network !== undefined)
  if (network.status === "pass") {
    assert(network.observed.includes("denied"))
  } else {
    assert(["blocked", "fail"].includes(network.status))
    assert(network.observed.length > 0)
  }
})

Deno.test("an audit sink that cannot record denies the effect", async () => {
  const abi = await landlockAbi()
  const results = await runWithProbeEnv(failClosedProbes(abi))
  const byId = new Map(results.map((result) => [result.id, result]))

  const posture = byId.get("FC-01")
  assert(posture !== undefined)
  assert(["pass", "fail"].includes(posture.status))

  const denial = byId.get("FC-02")
  assert(denial !== undefined)
  assertEquals(denial.status, "pass")
  assert(denial.observed.includes("guarded effect allowed=true"))
  assert(denial.observed.includes("guarded effect after denial=false"))
})

Deno.test("an interrupted non-idempotent effect is classified outcome_unknown", async () => {
  const result = await runWithProbeEnv(Effect.suspend(() => crashMidEffectProbe))
  assertEquals(result.status, "pass")
  assert(result.observed.includes("outcome_unknown"))
  assert(result.observed.includes("completion recorded=false"))
})
