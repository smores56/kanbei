import { Data } from "effect"

export class CanonicalJsonError extends Data.TaggedError("CanonicalJsonError")<{
  readonly reason: string
}> {}

export class DigestError extends Data.TaggedError("DigestError")<{
  readonly reason: string
}> {}

export class ArtifactError extends Data.TaggedError("ArtifactError")<{
  readonly operation: "read" | "write" | "mkdir" | "digest"
  readonly path: string
  readonly reason: string
}> {}

export class FixtureError extends Data.TaggedError("FixtureError")<{
  readonly dataset: string
  readonly version: string
  readonly reason: string
}> {}

export class EvidenceError extends Data.TaggedError("EvidenceError")<{
  readonly testId: string
  readonly reason: string
}> {}

export class RecordingRedactionError extends Data.TaggedError("RecordingRedactionError")<{
  readonly field: string
}> {}
