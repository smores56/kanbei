import { Console, Effect } from "effect"
import { reportText, runJourney } from "./runner.ts"

const root = Deno.cwd()
const output = `${root}/docs/evidence/journey-v2.json`
const check = Deno.args.includes("--check")

const { report } = await runJourney(root)
const text = reportText(report)

const summary = [
  `journey-v2: ${report.pass ? "pass" : "FAIL"}`,
  `fixture: ${report.dataset} tree ${report.fixtureTreeHash}`,
  `runDigest: ${report.runDigest}`,
  ...report.steps.map((step) =>
    `${step.testId}: ${step.outcome}${
      step.checks.length === 0 ? "" : ` (${step.checks.join(", ")})`
    }`
  ),
  ...Object.entries(report.metrics).toSorted(([left], [right]) => left.localeCompare(right)).map(
    ([name, value]) => `metric ${name}=${value}`,
  ),
].join("\n")

if (check) {
  // The pre-registered threshold decides whether the second run is required;
  // it is not a decorative field.
  if (report.thresholds.deterministicDigests) {
    const again = await runJourney(root)
    if (again.report.runDigest !== report.runDigest) {
      await Effect.runPromise(
        Console.error("journey-v2: two runs on an unchanged head produced different digests"),
      )
      await Effect.runPromise(
        Console.error(`first  ${report.runDigest}\nsecond ${again.report.runDigest}`),
      )
      Deno.exit(1)
    }
  }
  const recorded = await Deno.readTextFile(output).catch(() => "")
  if (recorded !== text) {
    await Effect.runPromise(
      Console.error(`journey-v2: ${output} is stale; run \`deno task journey\``),
    )
    Deno.exit(1)
  }
}

if (!check) {
  await Deno.mkdir(`${root}/docs/evidence`, { recursive: true })
  await Deno.writeTextFile(output, text)
}

await Effect.runPromise(Console.log(summary))
if (report.violations.length > 0) {
  await Effect.runPromise(
    Console.error(
      ["journey-v2: thresholds not met", ...report.violations.map((entry) => `  ${entry}`)].join(
        "\n",
      ),
    ),
  )
}
if (!report.pass) Deno.exit(1)
