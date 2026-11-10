import { Schema } from "effect"
import { lang1PreRegistration } from "./pre-registration.ts"
import { TASKS } from "./tasks.ts"
import { ARM_IDS, LATENCY_PROFILES_MS, ModelCell, ModelSlot } from "./types.ts"
import type { MissingInput } from "./types.ts"

export const ModelManifest = Schema.Struct({
  schemaVersion: Schema.Literal(1),
  providers: Schema.Array(
    Schema.Struct({
      provider: Schema.String,
      primary: Schema.String,
      smaller: Schema.String,
    }),
  ),
})
export type ModelManifest = typeof ModelManifest.Type

export interface ModelMatrixInputs {
  readonly manifest: ModelManifest | undefined
  readonly credentials: Readonly<Record<string, boolean>>
  readonly sandboxFrozen: boolean
  readonly registeredDatasets: Readonly<Record<string, boolean>>
}

export interface ModelMatrix {
  readonly slots: ReadonlyArray<ModelSlot>
  readonly missingInputs: ReadonlyArray<MissingInput>
  readonly cells: ReadonlyArray<ModelCell>
  readonly plannedCells: number
  readonly plannedTrials: number
  readonly status: "blocked" | "ready"
}

const sandboxInput: MissingInput = {
  id: "MI-04",
  description: "Frozen OS-sandbox profile and image digest",
  exact:
    "S1 requires a frozen sandbox profile digest; this host reports sandboxPosture=unverified and has no frozen S1 sandbox profile configured",
}

const costInput: MissingInput = {
  id: "MI-05",
  description: "cost-v1 dataset registration",
  exact:
    "fixtures/registry.json must register cost-v1@1.0.0; the registry lists journey-v1, append-v1, cost-v1, and ui-v1",
}

const securityInput: MissingInput = {
  id: "MI-06",
  description: "security-v1 dataset",
  exact: "fixtures/security-v1@1.0.0 and its fixtures/registry.json entry are missing",
}

const manifestInput: MissingInput = {
  id: "MI-01",
  description: "Frozen model manifest",
  exact:
    "spikes/language/model-manifest.json with schemaVersion=1 and providers[].primary and providers[].smaller model IDs",
}

export const buildModelMatrix = (inputs: ModelMatrixInputs): ModelMatrix => {
  const missing: Array<MissingInput> = []
  const slots: Array<ModelSlot> = lang1PreRegistration.modelSlots.map((slot) => {
    const providerEntry = inputs.manifest?.providers.find((entry) =>
      entry.provider === slot.provider
    )
    const modelId = providerEntry === undefined
      ? null
      : slot.tier === "primary"
      ? providerEntry.primary
      : providerEntry.smaller
    return {
      provider: slot.provider,
      tier: slot.tier,
      modelId,
      credentialEnv: slot.credentialEnv,
      credentialPresent: inputs.credentials[slot.credentialEnv] === true,
    }
  })

  if (inputs.manifest === undefined || slots.some((slot) => slot.modelId === null)) {
    missing.push(manifestInput)
  }

  const providers = [...new Set(slots.map((slot) => slot.provider))]
  for (const provider of providers) {
    const credentialEnv = slots.find((slot) => slot.provider === provider)?.credentialEnv
    if (credentialEnv !== undefined && inputs.credentials[credentialEnv] !== true) {
      missing.push({
        id: `MI-CRED-${provider.toUpperCase()}`,
        description: `${provider} API credential`,
        exact: `environment variable ${credentialEnv} must be set for a live model arm`,
      })
    }
  }

  if (!inputs.sandboxFrozen) missing.push(sandboxInput)
  if (inputs.registeredDatasets["cost-v1@1.0.0"] !== true) missing.push(costInput)
  if (inputs.registeredDatasets["security-v1@1.0.0"] !== true) missing.push(securityInput)

  const missingIds = missing.map((input) => input.id)
  const cells: Array<ModelCell> = []
  for (const arm of ARM_IDS) {
    for (const slot of slots) {
      for (const task of TASKS) {
        for (const latencyMs of LATENCY_PROFILES_MS) {
          cells.push({
            arm,
            taskId: task.id,
            latencyMs,
            provider: slot.provider,
            tier: slot.tier,
            trials: 0,
            plannedTrials: lang1PreRegistration.trialsPerCell,
            status: missingIds.length === 0 ? "ready" : "blocked",
            blockedBy: missingIds,
            metrics: {},
          })
        }
      }
    }
  }

  return {
    slots,
    missingInputs: missing,
    cells,
    plannedCells: cells.length,
    plannedTrials: cells.length * lang1PreRegistration.trialsPerCell,
    status: missingIds.length === 0 ? "ready" : "blocked",
  }
}
