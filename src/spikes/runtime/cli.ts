import { DenoServices } from "@effect/platform-deno"
import { Console, Effect, FileSystem, Layer, Schema } from "effect"
import { fileArtifactStoreLayer } from "../../evidence/artifact-store.ts"
import { EnvironmentProbe, liveEnvironmentProbeLayer } from "../../evidence/environment.ts"
import { fixtureStoreLayer } from "../../evidence/fixture-store.ts"
import { capture, ensureDirectory } from "./adapters/launch.ts"
import { runBakeOff } from "./driver.ts"
import {
  appendFixtureReference,
  outcomeOf,
  sealSecurityEvidence,
  sealSpikeEvidence,
  securityEnvelopeOutcome,
  securityFixtureReference,
} from "./envelope.ts"
import { PRE_REGISTRATION } from "./plan.ts"
import { renderSecurityEnvelope } from "./report.ts"
import { SpikeResultDocument } from "./schema.ts"

const repoRoot = Deno.cwd()
const workdir = `${repoRoot}/tmp/spikes/runtime`
const nodeDeps = `${workdir}/node-deps`
const nodeModules = `${nodeDeps}/node_modules`

const importsSchema = Schema.Struct({ imports: Schema.Record(Schema.String, Schema.String) })
const registrySchema = Schema.Struct({
  datasets: Schema.Array(Schema.Struct({ id: Schema.String, version: Schema.String })),
})

const effectVersion = (denoJson: string): string => {
  const imports = Schema.decodeUnknownSync(importsSchema)(JSON.parse(denoJson)).imports
  const specifier = imports["effect"] ?? ""
  const match = /effect@([0-9]+\.[0-9]+\.[0-9]+)/u.exec(specifier)
  return match?.[1] ?? "4.1.2"
}

const resolveNodeBinary = (): Effect.Effect<string> =>
  Effect.gen(function* () {
    const fromEnv = Deno.env.get("KANBEI_NODE_BIN")
    if (fromEnv !== undefined && fromEnv !== "") return fromEnv
    const output = yield* capture({
      command: "nix",
      args: [
        "--extra-experimental-features",
        "nix-command flakes",
        "build",
        "--no-link",
        "--print-out-paths",
        "nixpkgs#nodejs_24",
      ],
    })
    const path = output.trim()
    return path === "" ? "node" : `${path}/bin/node`
  })

const nodeInstalled = (): boolean => {
  try {
    return Deno.statSync(`${nodeModules}/effect`).isDirectory
  } catch {
    return false
  }
}

/**
 * Node has no import map. Install exactly the versions pinned in `deno.json`
 * under `tmp/`, outside version control, and let the resolve hook map them.
 */
const ensureNodeDependencies = (nodeBinary: string, denoJson: string): Effect.Effect<boolean> =>
  Effect.gen(function* () {
    if (nodeInstalled()) return true
    const npm = `${nodeBinary.slice(0, nodeBinary.lastIndexOf("/"))}/npm`
    const version = effectVersion(denoJson)
    yield* ensureDirectory(nodeDeps)
    yield* capture({
      command: npm,
      args: [
        "install",
        "--prefix",
        nodeDeps,
        "--no-audit",
        "--no-fund",
        "--loglevel",
        "error",
        `effect@${version}`,
      ],
      env: { npm_config_cache: `${workdir}/npm-cache` },
    })
    return nodeInstalled()
  })

const datasetVersion = (repoRootPath: string, id: string) =>
  Effect.gen(function* () {
    const fs = yield* FileSystem.FileSystem
    const text = yield* fs.readFileString(`${repoRootPath}/fixtures/registry.json`)
    const registry = Schema.decodeUnknownSync(registrySchema)(JSON.parse(text))
    const entry = registry.datasets.find((dataset) => dataset.id === id)
    return entry?.version ?? "1.0.0"
  })

const renderTable = (document: SpikeResultDocument): string => {
  const header = "| metric | unit | node | deno | ratio | winner |"
  const divider = "|---|---|---:|---:|---:|---|"
  const rows = document.measurements.map((measurement) =>
    `| ${measurement.label} | ${measurement.unit} | ${measurement.node} | ${measurement.deno} | ${measurement.ratio} | ${measurement.winner} |`
  )
  return [header, divider, ...rows].join("\n")
}

const layers = Layer.mergeAll(
  fileArtifactStoreLayer(`${repoRoot}/tmp/evidence-cas`),
  fixtureStoreLayer(repoRoot),
  liveEnvironmentProbeLayer,
)

const program = Effect.gen(function* () {
  const fs = yield* FileSystem.FileSystem
  const denoJson = yield* fs.readFileString(`${repoRoot}/deno.json`)
  const nodeBinary = yield* resolveNodeBinary()
  const installed = yield* ensureNodeDependencies(nodeBinary, denoJson)

  if (!installed) {
    yield* Console.log(
      `node dependencies unavailable under ${nodeModules}; Node arm will be blocked`,
    )
  }

  const environment = yield* (yield* EnvironmentProbe).fingerprint
  const bakeOff = yield* runBakeOff(
    { repoRoot, workdir, nodeBinary, nodeModules },
    environment,
  )

  const base: Omit<SpikeResultDocument, "envelope"> = {
    schemaVersion: 1,
    protocol: "S1",
    decisionId: "NODE-DENO-1",
    preRegistration: PRE_REGISTRATION,
    environment,
    arms: { node: bakeOff.arms.node, deno: bakeOff.arms.deno },
    measurements: [...bakeOff.outcome.measurements],
    hardFails: [...bakeOff.outcome.hardFails],
    composite: bakeOff.outcome.composite,
    decision: bakeOff.outcome.decision,
    rejectedAlternative: bakeOff.outcome.rejectedAlternative,
    residualRisks: [...bakeOff.residualRisks],
    blocked: [...bakeOff.blocked],
    unboundProbes: [...bakeOff.unboundProbes],
  }

  const version = yield* datasetVersion(repoRoot, "append-v1")
  const outcome = outcomeOf(
    bakeOff.outcome.decision.value,
    bakeOff.decisionBlocks,
    bakeOff.unboundProbes,
  )
  const envelope = yield* sealSpikeEvidence(base, appendFixtureReference(version), outcome)
  const securityVersion = yield* datasetVersion(repoRoot, "security-v1")
  const securityEnvelope = yield* sealSecurityEvidence(
    base,
    securityFixtureReference(securityVersion),
    securityEnvelopeOutcome(bakeOff.arms, PRE_REGISTRATION.permissionExpectations),
  )
  const document: SpikeResultDocument = Schema.decodeUnknownSync(SpikeResultDocument)({
    ...base,
    envelope,
    securityEnvelope,
  })

  yield* fs.makeDirectory(`${repoRoot}/spikes/runtime`, { recursive: true })
  yield* fs.writeFileString(
    `${repoRoot}/spikes/runtime/result.json`,
    `${JSON.stringify(document, null, 2)}\n`,
  )

  yield* Console.log(renderTable(document))
  yield* Console.log(
    `decision: ${document.decision.value} (${document.decision.rule}); composite node=${document.composite.node} deno=${document.composite.deno} margin=${document.composite.margin}`,
  )
  yield* Console.log(`hard fails: ${document.hardFails.length}`)
  for (const failure of document.hardFails) {
    yield* Console.log(`  ${failure.runtime} ${failure.id}: ${failure.detail}`)
  }
  for (const entry of document.blocked) {
    yield* Console.log(`  blocked ${entry.arm} ${entry.measurement}: ${entry.reason}`)
  }
  yield* Console.log(`envelope outcome: ${document.envelope.outcome}`)
  yield* Console.log(renderSecurityEnvelope(document))
})

const provided = program.pipe(Effect.provide(layers), Effect.provide(DenoServices.layer))

await Effect.runPromise(provided)
