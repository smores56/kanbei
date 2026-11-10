import { Effect } from "effect"
import type { EvidenceCapture } from "./capture.ts"
import type { ArtifactError, RecordingRedactionError } from "./errors.ts"
import type { FixtureSnapshot } from "./fixture-store.ts"

export const dryRunJourney = (
  fixture: FixtureSnapshot,
  capture: EvidenceCapture,
): Effect.Effect<void, ArtifactError | RecordingRedactionError> =>
  Effect.gen(function* () {
    yield* capture.putArtifactText(
      JSON.stringify(
        {
          event: "journey-dry-run",
          fixture: `${fixture.id}@${fixture.version}`,
          treeHash: fixture.treeHash,
          files: Object.keys(fixture.files).toSorted(),
        },
        null,
        2,
      ),
    )
    yield* capture.recordModel({
      provider: "fixture",
      request: { task: "journey-dry-run", fixture: `${fixture.id}@${fixture.version}` },
      response: { status: "ok", projects: 3 },
      reasoning: "absent",
    })
  })
