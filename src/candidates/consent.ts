import { Context, Data, Effect, Layer, Option, Result, Schema } from "effect"
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

/**
 * A human session the passkey surface verified. The consent surface never mints
 * one itself: an agent-reachable caller cannot forge a ruling.
 */
export const HumanSession = Schema.Struct({
  sessionId: Schema.String,
  origin: Schema.String,
  verifiedAtUs: Schema.Number,
})
export type HumanSession = typeof HumanSession.Type

export const ConsentDisplay = Schema.Struct({
  consentDigest: Schema.String,
  manifest: CandidateConsentManifest,
  tokenHash: Schema.String,
  renderedAtUs: Schema.Number,
  sessionId: Schema.String,
  origin: Schema.String,
})
export type ConsentDisplay = typeof ConsentDisplay.Type

export const ConsentGesture = Schema.Struct({
  token: Schema.String,
  consentDigest: Schema.String,
  candidateDigest: Schema.String,
  sessionId: Schema.String,
  origin: Schema.String,
  ruling: Schema.Literal("approve"),
  rulingCommitment: Schema.String,
})
export type ConsentGesture = typeof ConsentGesture.Type

export const CandidateApprovalReceipt = Schema.Struct({
  candidateId: Schema.String,
  candidateDigest: Schema.String,
  capabilityDiffDigest: Schema.String,
  scope: Schema.String,
  expiresAtUs: Schema.Number,
  sessionId: Schema.String,
  origin: Schema.String,
  consentDigest: Schema.String,
  tokenHash: Schema.String,
  ruling: Schema.Literal("approve"),
  rulingCommitment: Schema.String,
  consumedAtUs: Schema.Number,
})
export type CandidateApprovalReceipt = typeof CandidateApprovalReceipt.Type

export type ConsentRefusalCode =
  | "approval_token_invalid"
  | "approval_already_consumed"
  | "approval_session_mismatch"
  | "approval_session_unverified"
  | "approval_expired"
  | "approval_digest_mismatch"

export class ConsentRefusal extends Data.TaggedError("ConsentRefusal")<{
  readonly code: ConsentRefusalCode
  readonly detail: string
}> {}

export const CONSENT_RENDERER_VERSION = "kanbei.consent.renderer.v1"
export const CONSENT_VIEW_REVISION = "consent-rev-1"
export const RULING = "approve" as const

/** The verified human sessions the consent surface will accept. */
export interface HumanSessions {
  readonly verify: (
    sessionId: string,
    origin: string,
  ) => Effect.Effect<Option.Option<HumanSession>>
}

export const HumanSessions = Context.Service<HumanSessions>(
  "kanbei/candidates/consent/HumanSessions",
)

/**
 * The only path that carries a plaintext token to the human. It is a separate
 * port from `requestApproval` so an agent-reachable caller never sees the token.
 */
export interface ConsentDelivery {
  readonly deliver: (session: HumanSession, token: string) => Effect.Effect<void>
}

export const ConsentDelivery = Context.Service<ConsentDelivery>(
  "kanbei/candidates/consent/Delivery",
)

/** A delivery sink over the human channel; the plaintext token goes nowhere else. */
export const consentDeliveryLayer = (
  deliver: (session: HumanSession, token: string) => void,
): Layer.Layer<ConsentDelivery> =>
  Layer.succeed(ConsentDelivery)({
    deliver: (session, token) => Effect.sync(() => deliver(session, token)),
  })

/** Every shipped entrypoint must provide delivery; this default refuses to drop. */
export const refusingConsentDeliveryLayer: Layer.Layer<ConsentDelivery> = Layer.succeed(
  ConsentDelivery,
)({
  deliver: () => Effect.die(new Error("no consent delivery port is wired")),
})

export const consentDigestOf = (
  manifest: CandidateConsentManifest,
): Effect.Effect<string> =>
  sha256Labeled(
    "kanbei.candidate.consent.v1",
    [canonicalize(Schema.decodeUnknownSync(Schema.Json)(manifest))],
  ).pipe(Effect.orDie)

export const tokenHashOf = (token: string): Effect.Effect<string> =>
  sha256Text(token).pipe(Effect.orDie)

/** The receipt's non-replayable commitment over the rendered consent and token. */
export const rulingCommitmentOf = (
  consentDigest: string,
  ruling: "approve",
  tokenHash: string,
): Effect.Effect<string> =>
  sha256Labeled("kanbei.candidate.ruling.v1", [
    canonicalize(Schema.decodeUnknownSync(Schema.Json)({ consentDigest, ruling, tokenHash })),
  ]).pipe(Effect.orDie)

const TOKEN_BYTES = 32

/**
 * The token is 32 CSPRNG bytes and is shown to the human once. `Math.random` and
 * the runtime Random service are both predictable enough to guess a token.
 */
export const mintToken = (): Effect.Effect<
  { readonly token: string; readonly tokenHash: string }
> =>
  Effect.gen(function* () {
    const bytes = new Uint8Array(TOKEN_BYTES)
    crypto.getRandomValues(bytes)
    const token = `kc_${[...bytes].map((byte) => byte.toString(16).padStart(2, "0")).join("")}`
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
    `origin ${renderModelText(display.origin).text} session ${display.sessionId}`,
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

/** Hex compare that does not leak the position of the first mismatch. */
export const timingSafeEqualHex = (left: string, right: string): boolean => {
  const a = new TextEncoder().encode(left)
  const b = new TextEncoder().encode(right)
  const length = Math.max(a.length, b.length)
  let difference = a.length ^ b.length
  for (let index = 0; index < length; index += 1) {
    difference |= (a[index] ?? 0) ^ (b[index] ?? 0)
  }
  return difference === 0
}

/**
 * Verifies a ruling gesture against the persisted display preimage. The token is
 * single-use, bound to one verified session and origin, and expires explicitly;
 * no token-hash, digest, session, or commitment mismatch can mint a receipt.
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
    const presentedHash = yield* tokenHashOf(gesture.token)
    if (!timingSafeEqualHex(presentedHash, display.tokenHash)) {
      return Result.fail(
        new ConsentRefusal({
          code: "approval_token_invalid",
          detail: "the gesture token does not hash to the rendered token",
        }),
      )
    }
    if (gesture.sessionId !== display.sessionId || gesture.origin !== display.origin) {
      return Result.fail(
        new ConsentRefusal({
          code: "approval_session_mismatch",
          detail: "the gesture comes from another session or origin",
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
    const commitment = yield* rulingCommitmentOf(
      display.consentDigest,
      gesture.ruling,
      display.tokenHash,
    )
    if (!timingSafeEqualHex(gesture.rulingCommitment, commitment)) {
      return Result.fail(
        new ConsentRefusal({
          code: "approval_digest_mismatch",
          detail: "the gesture did not bind the ruling commitment",
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
      origin: display.origin,
      consentDigest: display.consentDigest,
      tokenHash: display.tokenHash,
      ruling: RULING,
      rulingCommitment: commitment,
      consumedAtUs: nowUs,
    })
  })
