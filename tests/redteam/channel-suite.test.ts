import { assertEquals } from "@std/assert"
import { Effect } from "effect"
import { CONTENT_SECURITY_POLICY } from "../../src/ui/csp.ts"
import { sha256Text } from "../../src/evidence/digest.ts"
import { loadCatalogue } from "./probes.ts"
import { EVIDENCE_CLAIMS, redteamReportText, runRedteam } from "./suite.ts"

Deno.test("every channel is attacked and every probe fails closed with its receipt", async () => {
  const report = await runRedteam(Deno.cwd())
  assertEquals(report.pass, true)
  assertEquals(report.failures, [])
  assertEquals(report.cases.length, 29)
  const channels = new Set(report.cases.map((probeCase) => probeCase.channel))
  assertEquals(
    [...channels].toSorted(),
    ["boundary", "chat", "csp", "csr", "inbox", "search", "todo", "vault"],
  )
  for (const probeCase of report.cases) {
    assertEquals(probeCase.outcome, probeCase.expected, probeCase.id)
    const claim = EVIDENCE_CLAIMS[probeCase.id]
    assertEquals(claim !== undefined, true, `${probeCase.id} has no evidence claim`)
    if (claim === "receipt" || claim === "quarantine") {
      assertEquals(probeCase.receipt, true, `${probeCase.id} has no ${claim} evidence`)
    }
  }
})

Deno.test("the red team rerun digest equals the recorded result", async () => {
  const first = await runRedteam(Deno.cwd())
  const second = await runRedteam(Deno.cwd())
  assertEquals(first.runDigest, second.runDigest)
  const recorded = await Deno.readTextFile(`${Deno.cwd()}/docs/evidence/redteam-v1.json`)
  assertEquals(recorded, redteamReportText(first))
})

Deno.test("a weakened CSP or a widened manifest breaks the gate", async () => {
  const catalogue = await loadCatalogue(Deno.cwd())
  const digest = await Effect.runPromise(sha256Text(CONTENT_SECURITY_POLICY))
  assertEquals(
    digest,
    catalogue.cspPolicyDigest,
    "the CSP changed; update the pinned digest only with a new fixture version",
  )
  const tampered = `${CONTENT_SECURITY_POLICY}; img-src *`
  const tamperedDigest = await Effect.runPromise(sha256Text(tampered))
  assertEquals(tamperedDigest === catalogue.cspPolicyDigest, false)
})
