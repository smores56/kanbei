import { Schema } from "effect"

const IsoDate = Schema.String.pipe(
  Schema.check(Schema.isPattern(/^\d{4}-\d{2}-\d{2}(?:T[0-9:.]+Z)?$/u)),
)

/** The sealed labels and knobs. Answers live here, never in the harness. */
export const JourneyFixture = Schema.Struct({
  schemaVersion: Schema.Literal(1),
  dataset: Schema.Literal("journey-v2"),
  version: Schema.Literal("1.0.0"),
  canonicalHead: Schema.Struct({
    seq: Schema.Number,
    hash: Schema.String,
  }),
  sequenceRange: Schema.Tuple([Schema.Number, Schema.Number]),
  clock: Schema.Struct({
    start: IsoDate,
    freshnessWindowMs: Schema.Number,
    staleWindowMs: Schema.Number,
    staleAdvanceMs: Schema.Number,
  }),
  sources: Schema.Struct({
    lifeAdmin: Schema.Array(Schema.String),
    material: Schema.Array(Schema.String),
    staleOrigin: Schema.String,
    unknownOrigin: Schema.String,
  }),
  ingest: Schema.Struct({
    acceptedBatches: Schema.Array(Schema.String),
    replayBatch: Schema.String,
    expectedGap: Schema.Struct({
      origin: Schema.String,
      missing: Schema.Number,
      sequence: Schema.Number,
    }),
    expectedSuppression: Schema.Struct({ origin: Schema.String, missing: Schema.Number }),
  }),
  advice: Schema.Struct({
    horizonDays: Schema.Number,
    expected: Schema.Array(Schema.Struct({ key: Schema.String, dueAt: IsoDate })),
    expectedStale: Schema.Array(Schema.Struct({ key: Schema.String, dueAt: IsoDate })),
  }),
  memory: Schema.Struct({
    claims: Schema.Array(
      Schema.Struct({
        claimId: Schema.String,
        subject: Schema.String,
        value: Schema.String,
        confidence: Schema.Number,
        source: Schema.String,
      }),
    ),
    retractions: Schema.Array(
      Schema.Struct({ targetClaimId: Schema.String, reason: Schema.String }),
    ),
    expected: Schema.Record(Schema.String, Schema.Literals(["active", "contested", "suspect"])),
  }),
  todo: Schema.Struct({
    limits: Schema.Struct({
      maxBytes: Schema.Number,
      maxWriteBytes: Schema.Number,
      maxLines: Schema.Number,
      maxLineChars: Schema.Number,
    }),
    create: Schema.String,
    complete: Schema.String,
    overSize: Schema.Number,
    conflictEdit: Schema.String,
  }),
  search: Schema.Struct({
    config: Schema.Struct({
      maxQueryChars: Schema.Number,
      maxLimit: Schema.Number,
      maxTextChars: Schema.Number,
      queriesPerHour: Schema.Number,
      queriesPerDay: Schema.Number,
    }),
    allowed: Schema.Struct({
      query: Schema.String,
      limit: Schema.Number,
      hits: Schema.Array(
        Schema.Struct({
          title: Schema.String,
          locator: Schema.String,
          text: Schema.String,
          rank: Schema.Number,
        }),
      ),
    }),
    dlp: Schema.Struct({
      query: Schema.String,
      limit: Schema.Number,
      reason: Schema.Literal("dlp"),
    }),
    judge: Schema.Struct({
      query: Schema.String,
      limit: Schema.Number,
      reason: Schema.Literal("judge"),
    }),
    markup: Schema.Struct({
      query: Schema.String,
      limit: Schema.Number,
      hits: Schema.Array(
        Schema.Struct({
          title: Schema.String,
          locator: Schema.String,
          text: Schema.String,
          rank: Schema.Number,
        }),
      ),
    }),
    chunked: Schema.Struct({
      queries: Schema.Array(Schema.String),
      expected: Schema.Array(Schema.Union([
        Schema.Literal("hits"),
        Schema.Literal("budget"),
      ])),
    }),
    forgedFields: Schema.Array(Schema.String),
  }),
  csp: Schema.Struct({ planted: Schema.String, cell: Schema.String }),
  boundary: Schema.Struct({
    declaredChannels: Schema.Array(Schema.String),
    requiredMountClasses: Schema.Array(Schema.String),
    leakManifestChannels: Schema.Array(Schema.String),
  }),
})
export type JourneyFixture = typeof JourneyFixture.Type

/** Thresholds are pre-registered with the fixture, not tuned by the runner. */
export const JourneyThresholds = Schema.Struct({
  schemaVersion: Schema.Literal(1),
  dataset: Schema.Literal("journey-v2"),
  version: Schema.Literal("1.0.0"),
  basis: Schema.String,
  advisory: Schema.Struct({ precisionMin: Schema.Number, recallMin: Schema.Number }),
  memory: Schema.Struct({ precisionMin: Schema.Number, recallMin: Schema.Number }),
  freshness: Schema.Struct({ staleAsFreshMax: Schema.Number, missingAsCurrentMax: Schema.Number }),
  todo: Schema.Struct({ capViolationsMax: Schema.Number, conflictOverwritesMax: Schema.Number }),
  search: Schema.Struct({
    blockedRecallMin: Schema.Number,
    budgetOverrunsMax: Schema.Number,
    forgedFieldsForwardedMax: Schema.Number,
  }),
  channelLeaksMax: Schema.Number,
  cspRemoteRequestsMax: Schema.Number,
  deterministicDigests: Schema.Boolean,
})
export type JourneyThresholds = typeof JourneyThresholds.Type

export const JourneySource = Schema.Struct({
  dataset: Schema.Literal("journey-v2"),
  version: Schema.Literal("1.0.0"),
  recipient: Schema.String,
  sealedBy: Schema.String,
  batches: Schema.Array(
    Schema.Struct({
      caseId: Schema.String,
      file: Schema.String,
      origin: Schema.String,
      sequence: Schema.Number,
    }),
  ),
})
export type JourneySource = typeof JourneySource.Type

export type JourneyStepOutcome = {
  readonly testId: string
  readonly outcome: "pass" | "fail"
  readonly checks: ReadonlyArray<string>
  readonly metrics: Readonly<Record<string, number>>
  readonly detail: Schema.Json
  /** Residuals the step cannot close in this repo; they land on the envelope. */
  readonly residuals?: ReadonlyArray<string>
}
