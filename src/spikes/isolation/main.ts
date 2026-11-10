import { DenoServices } from "@effect/platform-deno"
import { Effect, Layer, Schema } from "effect"
import { fileArtifactStoreLayer } from "../../evidence/artifact-store.ts"
import { liveEnvironmentProbeLayer } from "../../evidence/environment.ts"
import { fixtureStoreLayer } from "../../evidence/fixture-store.ts"
import { renderReport } from "./report.ts"
import { runSpike } from "./runner.ts"
import { acquireSupervisor } from "./supervisor.ts"
import { SpikeResult } from "./types.ts"

const DELEGATED_ENV = "KANBEI_S3_DELEGATED"

const root = Deno.cwd()
const runId = Deno.env.get("KANBEI_S3_RUN_ID") ?? `${Date.now().toString(36)}${Deno.pid}`

const encoder = new TextEncoder()
const say = (line: string): void => {
  Deno.stdout.writeSync(encoder.encode(`${line}\n`))
}

/**
 * One systemd call for the whole spike: a process-free delegated unit cgroup
 * with the payload in `<unit>/supervisor`. Per-agent limits are plain
 * cgroupfs below it, so no per-agent unit ever exists.
 */
const reexecDelegated = async (): Promise<never> => {
  const mainScript = new URL("./main.ts", import.meta.url).pathname
  const child = new Deno.Command("systemd-run", {
    args: [
      "--user",
      "--property=Delegate=yes",
      "--property=DelegateSubgroup=supervisor",
      "--pipe",
      "--wait",
      `--working-directory=${root}`,
      `--setenv=${DELEGATED_ENV}=1`,
      `--setenv=KANBEI_S3_RUN_ID=${runId}`,
      `--setenv=DENO_DIR=${Deno.env.get("DENO_DIR") ?? `${root}/tmp/deno-cache`}`,
      Deno.execPath(),
      "run",
      "-A",
      "--unstable-worker-options",
      mainScript,
    ],
    stdin: "inherit",
    stdout: "inherit",
    stderr: "inherit",
  }).spawn()
  const status = await child.status
  Deno.exit(status.code)
}

if (Deno.env.get(DELEGATED_ENV) !== "1") await reexecDelegated()

const spikeDirectory = `${root}/spikes/isolation`
const agentScript = new URL("./agent-process.ts", import.meta.url).pathname
const entryScript = new URL("./sandbox-entry.ts", import.meta.url).pathname

const supervisor = await Effect.runPromise(
  acquireSupervisor().pipe(
    Effect.tapError((error) =>
      Effect.sync(() => say(`supervisor: ${error.operation} failed: ${error.detail}`))
    ),
    Effect.orDie,
  ),
)

const layers = Layer.mergeAll(
  fileArtifactStoreLayer(`${root}/tmp/evidence-cas`),
  fixtureStoreLayer(root),
  liveEnvironmentProbeLayer,
)

const result = await Effect.runPromise(
  runSpike(root, {
    agentScript,
    entryScript,
    sourceDir: `${root}/src`,
    workRoot: `${root}/tmp/isolation/${runId}`,
    runId,
    generatedAt: new Date().toISOString(),
    supervisor,
  }).pipe(Effect.provide(layers), Effect.provide(DenoServices.layer)),
)

const decoded = Schema.decodeUnknownSync(SpikeResult)(result)
await Deno.mkdir(spikeDirectory, { recursive: true })
await Deno.writeTextFile(`${spikeDirectory}/result.json`, `${JSON.stringify(decoded, null, 2)}\n`)
await Deno.writeTextFile(`${spikeDirectory}/REPORT.md`, renderReport(decoded))
if (decoded.evidence.status === "emitted") {
  await Deno.writeTextFile(
    `${spikeDirectory}/envelope.json`,
    `${JSON.stringify(decoded.evidence.envelope, null, 2)}\n`,
  )
}

say(
  `spike: ${decoded.spikeId} (${decoded.validationId}) target=${decoded.targetConcurrency} agents`,
)
say(
  `supervisor: ${decoded.supervisor.kind} uid=${decoded.supervisor.uid} landlock-abi=${decoded.supervisor.landlockAbi} cgroup=${decoded.supervisor.cgroupRoot}`,
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
say(`DG-4: ${decoded.recommendation.status} ${decoded.recommendation.recommended ?? ""}`.trimEnd())
for (const line of decoded.recommendation.rationale) say(`  ${line}`)
say(
  decoded.evidence.status === "emitted"
    ? `evidence: testId=${decoded.evidence.envelope.testId} fixture=${decoded.evidence.envelope.fixtureVersion} outcome=${decoded.evidence.envelope.outcome} artifacts=${
      decoded.evidence.envelope.artifacts.join(",")
    }`
    : `evidence: unavailable (${decoded.evidence.reason})`,
)
say(`wrote ${spikeDirectory}/result.json and ${spikeDirectory}/REPORT.md`)
// WASM worker teardown leaves a pending event-loop resource in Deno 2.9; the
// entrypoint is done, so exit explicitly instead of spinning until the unit is killed.
Deno.exit(0)
