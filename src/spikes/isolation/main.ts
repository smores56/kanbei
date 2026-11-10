import { Effect, Schema } from "effect"
import { renderReport } from "./report.ts"
import { runSpike } from "./runner.ts"
import { SpikeResult } from "./types.ts"

const root = Deno.cwd()
const spikeDirectory = `${root}/spikes/isolation`
const runId = `${Date.now().toString(36)}${Deno.pid}`
const agentScript = new URL("./agent-process.ts", import.meta.url).pathname

const result = await Effect.runPromise(
  runSpike(root, {
    agentScript,
    runId,
    generatedAt: new Date().toISOString(),
  }),
)

const decoded = Schema.decodeUnknownSync(SpikeResult)(result)
await Deno.mkdir(spikeDirectory, { recursive: true })
await Deno.writeTextFile(`${spikeDirectory}/result.json`, `${JSON.stringify(decoded, null, 2)}\n`)
await Deno.writeTextFile(`${spikeDirectory}/REPORT.md`, renderReport(decoded))

const encoder = new TextEncoder()
const say = (line: string): void => {
  Deno.stdout.writeSync(encoder.encode(`${line}\n`))
}

say(
  `spike: ${decoded.spikeId} (${decoded.validationId}) target=${decoded.targetConcurrency} agents`,
)
for (const arm of decoded.arms) {
  const metrics = arm.metrics
  say(
    `arm ${arm.arm}: ${arm.status}${
      metrics === undefined
        ? ""
        : ` cold-start p95=${metrics.coldStartMs.p95Ms.toFixed(0)}ms rss/agent=${
          (metrics.rssPerAgentBytes / (1024 * 1024)).toFixed(1)
        }MiB ipc p95=${metrics.ipcRoundTripMs.p95Ms.toFixed(2)}ms`
    }`,
  )
  for (const check of arm.checks) {
    if (check.status === "fail") say(`  fail ${check.id}: ${check.detail}`)
  }
}
say(`DG-4: ${decoded.recommendation.status} ${decoded.recommendation.selected ?? ""}`.trimEnd())
for (const line of decoded.recommendation.rationale) say(`  ${line}`)
say(`wrote ${spikeDirectory}/result.json and ${spikeDirectory}/REPORT.md`)
