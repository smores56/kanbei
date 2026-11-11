import { Data, Effect, Option, Random, Result, Schema } from "effect"
import { canonicalize } from "../evidence/canonical.ts"
import { sha256Labeled, sha256Text } from "../evidence/digest.ts"
import { renderModelText } from "../ui/render.ts"

export const CandidateConsentManifest = Schema.Struct({
  candidateId: Schema.String,
  candidateDigest: Schema.String,
  scope: Schema.String,
  capabilityDiffDigest: Schema.String,
  expiresAtUs: Schema.Number,
  rendererVersion: Schema.String,
  viewRevision: Schema.String,
  labels: Schema.Record(Schema.String, Schema.String),
})
export type CandidateConsentManifest = typeof CandidateConsentManifest.Type

export const ConsentDisplay = Schema.Struct({
  consentDigest: Schema.String,
  manifest: CandidateConsentManifest,
  tokenHash: Schema.String,
  renderedAtUs: Schema.Number,
  sessionId: Schema.String,
})
export type ConsentDisplay = typeof ConsentDisplay.Type

export const ConsentGesture = Schema.Struct({
  token: Schema.String,
  consentDigest: Schema.String,
  candidateDigest: Schema.String,
  sessionId: Schema.String,
})
export type ConsentGesture = typeof ConsentGesture.Type

export const CandidateApprovalReceipt = Schema.Struct({
  candidateId: Schema.String,
  candidateDigest: Schema.String,
  capabilityDiffDigest: Schema.String,
  scope: Schema.String,
  expiresAtUs: Schema.Number,
  sessionId: Schema.String,
  consentDigest: Schema.String,
  ruling: Schema.Literal("approve"),
  consumedAtUs: Schema.Number,
})
export type CandidateApprovalReceipt = typeof CandidateApprovalReceipt.Type

export type ConsentRefusalCode =
  | "approval_token_invalid"
  | "approval_already_consumed"
  | "approval_session_mismatch"
  | "approval_expired"
  | "approval_digest_mismatch"

export class ConsentRefusal extends Data.TaggedError("ConsentRefusal")<{
  readonly code: ConsentRefusalCode
  readonly detail: string
}> {}

export const CONSENT_RENDERER_VERSION = "kanbei.consent.renderer.v1"
export const CONSENT_VIEW_REVISION = "consent-rev-1"

export const consentDigestOf = (
  manifest: CandidateConsentManifest,
): Effect.Effect<string> =>
  sha256Labeled(
    "kanbei.candidate.consent.v1",
    [canonicalize(Schema.decodeUnknownSync(Schema.Json)(manifest))],
  ).pipe(Effect.orDie)

export const tokenHashOf = (token: string): Effect.Effect<string> =>
  sha256Text(token).pipe(Effect.orDie)

/** 32 random bytes from the runtime Random service; the token is shown once. */
export const mintToken = (): Effect.Effect<
  { readonly token: string; readonly tokenHash: string }
> =>
  Effect.gen(function* () {
    const draws = yield* Effect.forEach(
      Array.from({ length: 32 }, (_, index) => index),
      () => Random.next,
    )
    const token = `kc_${
      draws.map((draw) => Math.floor(draw * 256).toString(16).padStart(2, "0"))
        .join("")
    }`
    return { token, tokenHash: yield* tokenHashOf(token) }
  })

export interface FrozenConsentRender {
  readonly mediaType: "text/plain; charset=utf-8"
  readonly text: string
  readonly lines: ReadonlyArray<string>
}

/**
 * The frozen consent surface's render: the digest, scope, diff, expiry, and
 * labels the human is binding. It is plain text and independent of the shell.
 */
export const renderCandidateConsent = (display: ConsentDisplay): FrozenConsentRender => {
  const labels = Object.entries(display.manifest.labels).toSorted(([left], [right]) =>
    left < right ? -1 : 1
  )
  const lines = [
    `consent candidate ${renderModelText(display.manifest.candidateId).text}`,
    `candidate digest ${display.manifest.candidateDigest}`,
    `consent digest ${display.consentDigest}`,
    `scope ${renderModelText(display.manifest.scope).text}`,
    `capability diff ${display.manifest.capabilityDiffDigest}`,
    `expires ${new Date(display.manifest.expiresAtUs / 1000).toISOString()}`,
    `renderer ${display.manifest.rendererVersion} view ${display.manifest.viewRevision}`,
    ...labels.map(([key, value]) =>
      `label ${renderModelText(key).text}: ${renderModelText(value).text}`
    ),
  ]
  return {
    mediaType: "text/plain; charset=utf-8",
    text: lines.join("\n"),
    lines,
  }
}

/**
 * Verifies a ruling gesture against the persisted display preimage. The token is
 * single-use, bound to one session, and expires explicitly; no digest or session
 * mismatch can mint a receipt.
 */
export const verifyGesture = (
  display: ConsentDisplay,
  consumedAtUs: Option.Option<number>,
  gesture: ConsentGesture,
  nowUs: number,
): Effect.Effect<Result.Result<CandidateApprovalReceipt, ConsentRefusal>> =>
  Effect.gen(function* () {
    if (Option.isSome(consumedAtUs)) {
      return Result.fail(
        new ConsentRefusal({
          code: "approval_already_consumed",
          detail: `consumed at ${consumedAtUs.value}`,
        }),
      )
    }
    if (gesture.sessionId !== display.sessionId) {
      return Result.fail(
        new ConsentRefusal({
          code: "approval_session_mismatch",
          detail: "the gesture comes from another session",
        }),
      )
    }
    if (nowUs >= display.manifest.expiresAtUs) {
      return Result.fail(
        new ConsentRefusal({
          code: "approval_expired",
          detail: `expired at ${display.manifest.expiresAtUs}`,
        }),
      )
    }
    const recomputed = yield* consentDigestOf(display.manifest)
    if (
      recomputed !== display.consentDigest || gesture.consentDigest !== display.consentDigest
    ) {
      return Result.fail(
        new ConsentRefusal({
          code: "approval_digest_mismatch",
          detail: "the gesture did not bind the rendered consent digest",
        }),
      )
    }
    if (gesture.candidateDigest !== display.manifest.candidateDigest) {
      return Result.fail(
        new ConsentRefusal({
          code: "approval_digest_mismatch",
          detail: "the candidate digest changed after rendering",
        }),
      )
    }
    return Result.succeed({
      candidateId: display.manifest.candidateId,
      candidateDigest: display.manifest.candidateDigest,
      capabilityDiffDigest: display.manifest.capabilityDiffDigest,
      scope: display.manifest.scope,
      expiresAtUs: display.manifest.expiresAtUs,
      sessionId: display.sessionId,
      consentDigest: display.consentDigest,
      ruling: "approve" as const,
      consumedAtUs: nowUs,
    })
  })
