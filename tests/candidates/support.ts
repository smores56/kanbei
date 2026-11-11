import { Effect, Layer } from "effect"
import { fileBlobSinkLayer } from "../../src/adapters/blob-sink.ts"
import { fileLedgerStoreLayer } from "../../src/adapters/ledger-file.ts"
import { pointerFileLayer } from "../../src/adapters/pointer-file.ts"
import { CandidateSupervisor, candidateSupervisorLayer } from "../../src/candidates/supervisor.ts"
import type { CandidateWorkspace } from "../../src/candidates/manifest.ts"
import { contentStoreLayer } from "../../src/kernel/cas.ts"
import { Ledger, ledgerLayer } from "../../src/kernel/ledger.ts"
import { passingInputs } from "../../src/candidates/validation.ts"

export const EPOCH = "epoch-1"

export const layers = (directory: string) => {
  const ledger = ledgerLayer(EPOCH).pipe(
    Layer.provide(fileLedgerStoreLayer(`${directory}/ledger.jsonl`)),
  )
  const cas = contentStoreLayer.pipe(Layer.provide(fileBlobSinkLayer(`${directory}/cas`)))
  const pointers = pointerFileLayer(`${directory}/pointers`)
  const supervisor = candidateSupervisorLayer.pipe(
    Layer.provide(Layer.mergeAll(ledger, cas, pointers)),
  )
  return Layer.mergeAll(supervisor, ledger)
}

export const run = <A, E>(
  directory: string,
  effect: Effect.Effect<A, E, CandidateSupervisor | Ledger>,
): Promise<A> => Effect.runPromise(effect.pipe(Effect.provide(layers(directory))))

export const workspace = (
  overrides: Partial<CandidateWorkspace> = {},
): CandidateWorkspace => ({
  candidateId: "cand-1",
  scope: "project/kanbei",
  generation: 1,
  kind: "executable",
  capabilities: [{ capability: "channel.inbox.read", scope: "narrow" }],
  dependencies: [{ id: "dep.a", version: "1.0.0" }],
  schema: { contractVersion: "kb/1.0.0", dataVersion: 0 },
  ...overrides,
})

export const artifact = (text = "plugin bundle"): Uint8Array => new TextEncoder().encode(text)

export const inputs = passingInputs({ resolvedArtifacts: ["dep.a@1.0.0"] })

export const staged = (
  workspaceValue: CandidateWorkspace = workspace(),
  binary = artifact(),
  validationInputs = inputs,
) =>
  Effect.gen(function* () {
    const supervisor = yield* CandidateSupervisor
    const stagedRecord = yield* supervisor.stage(workspaceValue, binary)
    return yield* supervisor.validate(stagedRecord.manifest.candidateId, validationInputs)
  })
