/**
 * Adversarial channel-leak and CSP suite CLI. Attacks every channel end-to-end
 * in-process and records a deterministic result set. Run from the repo root.
 */
import { Console, Effect } from "effect"
import { redteamReportText, runRedteam } from "../tests/redteam/suite.ts"

const root = Deno.cwd()
const output = `${root}/docs/evidence/redteam-v1.json`
const check = Deno.args.includes("--check")
const report = await runRedteam(root)
const text = redteamReportText(report)

if (check) {
  const again = await runRedteam(root)
  if (again.runDigest !== report.runDigest) {
    await Effect.runPromise(Console.error("redteam: two runs produced different digests"))
    Deno.exit(1)
  }
  const recorded = await Deno.readTextFile(output).catch(() => "")
  if (recorded !== text) {
    await Effect.runPromise(
      Console.error(`redteam: ${output} is stale; run \`deno task redteam\``),
    )
    Deno.exit(1)
  }
}

if (!check) {
  await Deno.mkdir(`${root}/docs/evidence`, { recursive: true })
  await Deno.writeTextFile(output, text)
}

await Effect.runPromise(Console.log(`redteam: ${report.pass ? "pass" : "FAIL"}`))
await Effect.runPromise(
  Console.log(
    `cases: ${
      report.cases.filter((entry) => entry.outcome === entry.expected).length
    }/${report.cases.length} matched; suite ${report.suite}; digest ${report.runDigest}`,
  ),
)
if (report.failures.length > 0) {
  await Effect.runPromise(
    Console.error(report.failures.map((failure) => `  ${failure}`).join("\n")),
  )
}
if (!report.pass) Deno.exit(1)
