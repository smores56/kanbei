import { Context, Effect, Ref } from "effect"
import type { ArtifactStore, ContentDigest } from "./artifact-store.ts"
import type { Json } from "./canonical.ts"
import { RecordingRedactionError } from "./errors.ts"
import type { ArtifactError } from "./errors.ts"

export const REDACTION_POLICY = "kanbei/evidence/redaction-v1"

const credentialField =
  /^(authorization|api[_-]?key|access[_-]?token|refresh[_-]?token|secret|password|credential)$/iu

const findCredentialField = (value: Json, path: string): string | undefined => {
  if (value === null || typeof value !== "object") return undefined
  if (Array.isArray(value)) {
    for (const [index, item] of value.entries()) {
      const found = findCredentialField(item, `${path}[${index}]`)
      if (found !== undefined) return found
    }
    return undefined
  }
  for (const [key, item] of Object.entries(value)) {
    const next = path === "" ? key : `${path}.${key}`
    if (credentialField.test(key)) return next
    const found = findCredentialField(item, next)
    if (found !== undefined) return found
  }
  return undefined
}

const redactionCommitment = {
  policy: REDACTION_POLICY,
  credentialBearingBytes: "absent",
  scope: "exact-post-redaction",
} as const

export interface ModelRecordingInput {
  readonly provider: string
  readonly request: Json
  readonly response: Json
  readonly parameters?: Json
  readonly outputEvents?: Json
  readonly reasoning: "present" | "absent"
}

export interface SearchRecordingInput {
  readonly provider: string
  readonly request: Json
  readonly response: Json
}

export interface EvidenceCapture {
  readonly putArtifact: (bytes: Uint8Array) => Effect.Effect<ContentDigest, ArtifactError>
  readonly putArtifactText: (text: string) => Effect.Effect<ContentDigest, ArtifactError>
  readonly recordModel: (
    input: ModelRecordingInput,
  ) => Effect.Effect<ContentDigest, ArtifactError | RecordingRedactionError>
  readonly recordSearch: (
    input: SearchRecordingInput,
  ) => Effect.Effect<ContentDigest, ArtifactError | RecordingRedactionError>
  readonly snapshot: Effect.Effect<{
    readonly artifacts: ReadonlyArray<ContentDigest>
    readonly modelRecordings: ReadonlyArray<ContentDigest>
  }>
}

export const EvidenceCapture = Context.Service<EvidenceCapture>("kanbei/evidence/Capture")

const modelPayload = (input: ModelRecordingInput): Json => ({
  schemaVersion: 1,
  kind: "model",
  provider: input.provider,
  redaction: redactionCommitment,
  request: input.request,
  response: input.response,
  reasoning: input.reasoning,
  ...(input.parameters === undefined ? {} : { parameters: input.parameters }),
  ...(input.outputEvents === undefined ? {} : { outputEvents: input.outputEvents }),
})

const searchPayload = (input: SearchRecordingInput): Json => ({
  schemaVersion: 1,
  kind: "search",
  provider: input.provider,
  redaction: redactionCommitment,
  request: input.request,
  response: input.response,
})

const ensureRedacted = (payload: Json): Effect.Effect<void, RecordingRedactionError> => {
  const field = findCredentialField(payload, "")
  return field === undefined ? Effect.void : Effect.fail(new RecordingRedactionError({ field }))
}

export const makeEvidenceCapture = (store: ArtifactStore): Effect.Effect<EvidenceCapture> =>
  Effect.gen(function* () {
    const artifacts = yield* Ref.make<ReadonlyArray<ContentDigest>>([])
    const modelRecordings = yield* Ref.make<ReadonlyArray<ContentDigest>>([])

    const addArtifact = (digest: ContentDigest) =>
      Ref.update(artifacts, (current) => [...current, digest])

    const record = (payload: Json) =>
      Effect.gen(function* () {
        yield* ensureRedacted(payload)
        const digest = yield* store.putJson(payload)
        yield* addArtifact(digest)
        yield* Ref.update(modelRecordings, (current) => [...current, digest])
        return digest
      })

    return {
      putArtifact: (bytes) =>
        Effect.gen(function* () {
          const digest = yield* store.put(bytes)
          yield* addArtifact(digest)
          return digest
        }),
      putArtifactText: (text) =>
        Effect.gen(function* () {
          const digest = yield* store.putText(text)
          yield* addArtifact(digest)
          return digest
        }),
      recordModel: (input) => record(modelPayload(input)),
      recordSearch: (input) => record(searchPayload(input)),
      snapshot: Effect.gen(function* () {
        return {
          artifacts: yield* Ref.get(artifacts),
          modelRecordings: yield* Ref.get(modelRecordings),
        }
      }),
    }
  })
