import type {
  CandidateConsentManifest,
  ConsentGesture,
  FrozenConsentRender,
  HumanSession,
} from "./consent.ts"
import type { LedgerRecord } from "../kernel/ledger.ts"
import type { ValidationInputs } from "./validation.ts"
import {
  CandidateApprovalReceipt,
  CONSENT_RENDERER_VERSION,
  CONSENT_VIEW_REVISION,
  ConsentDelivery,
  consentDigestOf,
  ConsentDisplay,
  HumanSessions,
  mintToken,
  renderCandidateConsent,
  rulingCommitmentOf,
  tokenHashOf,
  verifyGesture,
} from "./consent.ts"
import {
  candidateDigest,
  CandidateManifest,
  CandidateWorkspace,
  CapabilityDiff,
  capabilityDiff,
  diffDigest,
  digestOfJson,
} from "./manifest.ts"
import { CandidateConsentManifest as ConsentManifestSchema } from "./consent.ts"
import { CasError, ContentStore } from "../kernel/cas.ts"
import { Clock, Context, Data, Effect, Layer, Option, Ref, Result, Schema } from "effect"
import { Ledger, LedgerError } from "../kernel/ledger.ts"
import { PointerError, PointerStore, ScopePointer } from "./pointer.ts"
import { runValidation, ValidationReport } from "./validation.ts"
import { parseViewDocument, validateViewDocument } from "../ui/validation.ts"

export const MANIFEST_COMPONENT = "kb.state/candidate_manifest@1.0.0"
export const VALIDATION_COMPONENT = "kb.work/candidate_validation@1.0.0"
export const CONSENT_COMPONENT = "kb.work/consent_display@1.0.0"
export const APPROVAL_COMPONENT = "kb.work/candidate_approval@1.0.0"
export const ACTIVATION_COMPONENT = "kb.activation/candidate@1.0.0"
export const FAILURE_COMPONENT = "kb.activation/candidate_failure@1.0.0"
export const ROLLBACK_COMPONENT = "kb.activation/candidate_rollback@1.0.0"

export const CandidateState = Schema.Literals([
  "staged",
  "validated",
  "rejected",
  "quarantined",
  "approved",
  "active",
  "failed_health",
  "rolled_back",
])
export type CandidateState = typeof CandidateState.Type

export interface CandidateRecord {
  readonly manifest: CandidateManifest
  readonly digest: string
  readonly workspace: CandidateWorkspace
  readonly state: CandidateState
  readonly report: Option.Option<ValidationReport>
  readonly diff: Option.Option<CapabilityDiff>
  readonly diffDigest: Option.Option<string>
  readonly approval: Option.Option<CandidateApprovalReceipt>
}

export type CandidateErrorCode =
  | "candidate_unknown"
  | "validation_incomplete"
  | "candidate_rejected"
  | "candidate_quarantined"
  | "approval_required"
  | "approval_expired"
  | "approval_token_invalid"
  | "approval_already_consumed"
  | "approval_session_mismatch"
  | "approval_session_unverified"
  | "approval_digest_mismatch"
  | "digest_mismatch"
  | "scope_mismatch"
  | "view_invalid"
  | "pointer_missing"
  | "pointer_corrupt"
  | "no_rollback_target"
  | "ledger_failed"
  | "store_failed"

export class CandidateError extends Data.TaggedError("CandidateError")<{
  readonly code: CandidateErrorCode
  readonly detail: string
}> {}

/**
 * `requestApproval` hands the human only the frozen render; the plaintext token
 * leaves through `ConsentDelivery`, so an agent-reachable caller cannot rule.
 */
export interface ApprovalRequestResult {
  readonly display: ConsentDisplay
  readonly render: FrozenConsentRender
}

export interface ActivationOutcome {
  readonly candidateId: string
  readonly health: "pass" | "fail"
  readonly receipt: string
  readonly pointer: Option.Option<ScopePointer>
  readonly state: CandidateState
}

export interface ScopeRollback {
  readonly scope: string
  readonly receipt: string
  readonly pointer: Option.Option<ScopePointer>
}

export interface CandidateSupervisor {
  readonly stage: (
    workspace: CandidateWorkspace,
    artifact: Uint8Array,
  ) => Effect.Effect<CandidateRecord, CandidateError>
  readonly validate: (
    candidateId: string,
    inputs: ValidationInputs,
  ) => Effect.Effect<CandidateRecord, CandidateError>
  readonly requestApproval: (
    candidateId: string,
    session: HumanSession,
    ttlUs: number,
  ) => Effect.Effect<ApprovalRequestResult, CandidateError>
  readonly approve: (
    candidateId: string,
    gesture: ConsentGesture,
  ) => Effect.Effect<CandidateApprovalReceipt, CandidateError>
  /** Flip, then probe, then receipt: a failed flip never records an activation. */
  readonly activate: (
    candidateId: string,
    probe: Effect.Effect<"pass" | "fail">,
  ) => Effect.Effect<ActivationOutcome, CandidateError>
  readonly rollback: (scope: string, reason: string) => Effect.Effect<ScopeRollback, CandidateError>
  readonly candidate: (candidateId: string) => Effect.Effect<Option.Option<CandidateRecord>>
  readonly candidates: Effect.Effect<ReadonlyArray<CandidateRecord>>
  readonly pointer: (scope: string) => Effect.Effect<Option.Option<ScopePointer>, CandidateError>
}

export const CandidateSupervisor = Context.Service<CandidateSupervisor>(
  "kanbei/candidates/Supervisor",
)

const ManifestPayload = Schema.Struct({
  manifest: CandidateManifest,
  digest: Schema.String,
  workspace: CandidateWorkspace,
})

const ValidationPayload = Schema.Struct({
  report: ValidationReport,
  diff: CapabilityDiff,
  diffDigest: Schema.String,
})

const ActivationPayload = Schema.Struct({
  candidateId: Schema.String,
  candidateDigest: Schema.String,
  scope: Schema.String,
  health: Schema.Literal("pass"),
  activationReceipt: Schema.String,
  pointerGeneration: Schema.Number,
  previous: Schema.optional(ScopePointer),
})
type ActivationPayload = typeof ActivationPayload.Type

const FailurePayload = Schema.Struct({
  candidateId: Schema.String,
  candidateDigest: Schema.String,
  scope: Schema.String,
  health: Schema.Literal("fail"),
  reason: Schema.String,
  restored: Schema.optional(ScopePointer),
})

const RollbackPayload = Schema.Struct({
  scope: Schema.String,
  from: Schema.String,
  to: Schema.String,
  fromGeneration: Schema.Number,
  fromReceipt: Schema.String,
  reason: Schema.String,
})

const toJson = (value: unknown): Schema.Json => Schema.decodeUnknownSync(Schema.Json)(value)

const componentPayload = (
  record: LedgerRecord,
  type: string,
): Schema.Json | undefined =>
  record.components.find((component) => component.type === type)?.payload

const applyRecord = (index: IndexState, record: LedgerRecord): IndexState => {
  const manifestPayload = componentPayload(record, MANIFEST_COMPONENT)
  if (manifestPayload !== undefined) {
    const payload = Schema.decodeUnknownSync(ManifestPayload)(manifestPayload)
    return {
      ...index,
      candidates: {
        ...index.candidates,
        [payload.manifest.candidateId]: {
          manifest: payload.manifest,
          digest: payload.digest,
          workspace: payload.workspace,
          state: "staged",
          report: Option.none(),
          diff: Option.none(),
          diffDigest: Option.none(),
          approval: Option.none(),
        },
      },
    }
  }
  const validationPayload = componentPayload(record, VALIDATION_COMPONENT)
  if (validationPayload !== undefined) {
    const payload = Schema.decodeUnknownSync(ValidationPayload)(validationPayload)
    const candidate = index.candidates[payload.report.candidateId]
    if (candidate === undefined) return index
    const state: CandidateState = payload.report.verdict === "validated"
      ? "validated"
      : payload.report.verdict === "rejected"
      ? "rejected"
      : "quarantined"
    return {
      ...index,
      candidates: {
        ...index.candidates,
        [candidate.manifest.candidateId]: {
          ...candidate,
          state,
          report: Option.some(payload.report),
          diff: Option.some(payload.diff),
          diffDigest: Option.some(payload.diffDigest),
        },
      },
    }
  }
  const consentPayload = componentPayload(record, CONSENT_COMPONENT)
  if (consentPayload !== undefined) {
    const display = Schema.decodeUnknownSync(ConsentDisplay)(consentPayload)
    return {
      ...index,
      consents: {
        ...index.consents,
        [display.tokenHash]: { display, consumedAtUs: Option.none() },
      },
    }
  }
  const approvalPayload = componentPayload(record, APPROVAL_COMPONENT)
  if (approvalPayload !== undefined) {
    const receipt = Schema.decodeUnknownSync(CandidateApprovalReceipt)(approvalPayload)
    const candidate = index.candidates[receipt.candidateId]
    const consumed = index.consents[receipt.tokenHash]
    return {
      ...index,
      candidates: candidate === undefined ? index.candidates : {
        ...index.candidates,
        [receipt.candidateId]: { ...candidate, state: "approved", approval: Option.some(receipt) },
      },
      // Consumption is per token, not per consent digest: another display for the
      // same candidate must not be marked used by this receipt.
      consents: consumed === undefined ? index.consents : {
        ...index.consents,
        [receipt.tokenHash]: { ...consumed, consumedAtUs: Option.some(receipt.consumedAtUs) },
      },
    }
  }
  const activationPayload = componentPayload(record, ACTIVATION_COMPONENT)
  if (activationPayload !== undefined) {
    const payload = Schema.decodeUnknownSync(ActivationPayload)(activationPayload)
    const candidate = index.candidates[payload.candidateId]
    if (candidate === undefined) return index
    return {
      ...index,
      candidates: {
        ...index.candidates,
        [payload.candidateId]: { ...candidate, state: "active" },
      },
    }
  }
  const failurePayload = componentPayload(record, FAILURE_COMPONENT)
  if (failurePayload !== undefined) {
    const payload = Schema.decodeUnknownSync(FailurePayload)(failurePayload)
    const candidate = index.candidates[payload.candidateId]
    if (candidate === undefined) return index
    return {
      ...index,
      candidates: {
        ...index.candidates,
        [payload.candidateId]: { ...candidate, state: "failed_health" },
      },
    }
  }
  const rollbackPayload = componentPayload(record, ROLLBACK_COMPONENT)
  if (rollbackPayload !== undefined) {
    const payload = Schema.decodeUnknownSync(RollbackPayload)(rollbackPayload)
    const previous = index.candidates[payload.from]
    return previous === undefined ? index : {
      ...index,
      candidates: {
        ...index.candidates,
        [payload.from]: { ...previous, state: "rolled_back" },
      },
    }
  }
  return index
}

const emptyIndex: IndexState = { candidates: {}, consents: {} }

const sortedDependencies = (workspace: CandidateWorkspace): Schema.Json =>
  toJson(
    workspace.dependencies.toSorted((left, right) => left.id < right.id ? -1 : 1),
  )

const mapFailure = (error: LedgerError | CasError | PointerError): CandidateError =>
  new CandidateError({
    code: error instanceof LedgerError ? "ledger_failed" : "store_failed",
    detail: error instanceof LedgerError
      ? error.detail
      : "reason" in error
      ? error.reason
      : error.detail,
  })

interface ConsentRecord {
  readonly display: ConsentDisplay
  readonly consumedAtUs: Option.Option<number>
}

interface IndexState {
  readonly candidates: Readonly<Record<string, CandidateRecord>>
  readonly consents: Readonly<Record<string, ConsentRecord>>
}

const activationKey = (scope: string, candidateId: string, digest: string): string =>
  `activation:${scope}:${candidateId}:${digest}`

const capabilityNames = (
  entries: ReadonlyArray<{ readonly capability: string; readonly scope: string }>,
): string =>
  entries.map((entry) => `${entry.capability}(${entry.scope})`).toSorted().join(", ") || "none"

const approvalIssues = (
  candidate: CandidateRecord,
  atUs: number,
): Option.Option<CandidateError> => {
  if (Option.isNone(candidate.approval)) {
    return Option.some(
      new CandidateError({ code: "approval_required", detail: candidate.manifest.candidateId }),
    )
  }
  const approval = candidate.approval.value
  if (approval.candidateDigest !== candidate.digest) {
    return Option.some(
      new CandidateError({
        code: "digest_mismatch",
        detail: "the approval binds another candidate digest",
      }),
    )
  }
  if (approval.scope !== candidate.manifest.scope) {
    return Option.some(
      new CandidateError({
        code: "scope_mismatch",
        detail: `${approval.scope} != ${candidate.manifest.scope}`,
      }),
    )
  }
  if (
    Option.isNone(candidate.diffDigest) ||
    candidate.diffDigest.value !== approval.capabilityDiffDigest
  ) {
    return Option.some(
      new CandidateError({
        code: "digest_mismatch",
        detail: "the approval binds another capability diff",
      }),
    )
  }
  if (atUs >= approval.expiresAtUs) {
    return Option.some(
      new CandidateError({ code: "approval_expired", detail: candidate.manifest.candidateId }),
    )
  }
  return Option.none()
}

/** A receipt's commitment must recompute from its own consent digest and token. */
const approvalCommitmentIssue = (
  approval: CandidateApprovalReceipt,
): Effect.Effect<Option.Option<CandidateError>> =>
  rulingCommitmentOf(approval.consentDigest, approval.ruling, approval.tokenHash).pipe(
    Effect.map((commitment) =>
      commitment === approval.rulingCommitment ? Option.none<CandidateError>() : Option.some(
        new CandidateError({
          code: "approval_digest_mismatch",
          detail: "the approval commitment does not bind its consent",
        }),
      )
    ),
  )

export const candidateSupervisorLayer: Layer.Layer<
  CandidateSupervisor,
  never,
  Ledger | ContentStore | PointerStore | HumanSessions | ConsentDelivery
> = Layer.effect(
  CandidateSupervisor,
  Effect.gen(function* () {
    const ledger = yield* Ledger
    const cas = yield* ContentStore
    const pointers = yield* PointerStore
    const sessions = yield* HumanSessions
    const delivery = yield* ConsentDelivery
    const initial = yield* ledger.records.pipe(Effect.orDie)
    const index = yield* Ref.make(
      initial.reduce<IndexState>(applyRecord, emptyIndex),
    )

    const candidatesEffect: CandidateSupervisor["candidates"] = Ref.get(index).pipe(
      Effect.map((current) =>
        Object.values(current.candidates).toSorted((left, right) =>
          left.manifest.createdAtUs - right.manifest.createdAtUs
        )
      ),
    )

    const find = (candidateId: string): Effect.Effect<CandidateRecord, CandidateError> =>
      Ref.get(index).pipe(
        Effect.flatMap((current) => {
          const candidate = current.candidates[candidateId]
          return candidate === undefined
            ? Effect.fail(
              new CandidateError({ code: "candidate_unknown", detail: candidateId }),
            )
            : Effect.succeed(candidate)
        }),
      )

    const append = (
      type: string,
      payload: Schema.Json,
      role: "frozen" | "human_gesture",
      principal: string,
      authorityClass: "coordination" | "human_authority" | "activation_receipt",
      idempotencyKey?: string,
    ) =>
      ledger.append({
        components: [{ type, payload }],
        appender: role,
        principal,
        authorityClass,
        ...(idempotencyKey === undefined ? {} : { idempotencyKey }),
      }).pipe(
        Effect.mapError((error) => mapFailure(error)),
        Effect.flatMap((result) =>
          Ref.update(index, (current) => applyRecord(current, result.record)).pipe(
            Effect.as(result),
          )
        ),
      )

    const verifySession = (sessionId: string, origin: string) =>
      sessions.verify(sessionId, origin).pipe(
        Effect.flatMap((verified) =>
          Option.isNone(verified)
            ? Effect.fail(
              new CandidateError({
                code: "approval_session_unverified",
                detail: "the ruling session is not a verified human session",
              }),
            )
            : Effect.succeed(verified.value)
        ),
      )

    const stage: CandidateSupervisor["stage"] = (workspace, artifact) =>
      Effect.gen(function* () {
        const published = yield* cas.publish(
          artifact,
          "application/vnd.kanbei.candidate.bundle",
        ).pipe(Effect.mapError((error) => mapFailure(error)))
        const now = yield* Clock.currentTimeMillis
        const manifest: CandidateManifest = {
          candidateId: workspace.candidateId,
          scope: workspace.scope,
          generation: workspace.generation,
          ...(workspace.base === undefined ? {} : { base: workspace.base }),
          kind: workspace.kind,
          artifact: {
            digest: published.manifest.digest,
            manifestDigest: published.manifestDigest,
            size: published.manifest.size,
          },
          manifestDigest: yield* digestOfJson(toJson(workspace), "kanbei.pluginmanifest.v1"),
          dependencyLockDigest: yield* digestOfJson(
            sortedDependencies(workspace),
            "kanbei.dependencylock.v1",
          ),
          capabilityRequestsDigest: yield* digestOfJson(
            toJson(workspace.capabilities),
            "kanbei.capabilities.v1",
          ),
          schema: workspace.schema,
          createdAtUs: Math.floor(now * 1000),
        }
        const digest = yield* candidateDigest(manifest)
        yield* append(
          MANIFEST_COMPONENT,
          toJson({ manifest, digest, workspace }),
          "frozen",
          "candidate-supervisor",
          "coordination",
        )
        return yield* find(workspace.candidateId)
      })

    const validate: CandidateSupervisor["validate"] = (candidateId, inputs) =>
      Effect.gen(function* () {
        const candidate = yield* find(candidateId)
        // Re-validating a refused candidate would launder it into `validated`.
        if (candidate.state === "rejected" || candidate.state === "quarantined") {
          return yield* Effect.fail(
            new CandidateError({
              code: candidate.state === "quarantined"
                ? "candidate_quarantined"
                : "candidate_rejected",
              detail: `${candidateId} was ${candidate.state}; re-validation is refused`,
            }),
          )
        }
        const base = candidate.manifest.base === undefined
          ? undefined
          : (yield* Ref.get(index)).candidates[candidate.manifest.base]
        const diff = capabilityDiff(
          base?.workspace.capabilities ?? [],
          candidate.workspace.capabilities,
          base?.workspace.dependencies ?? [],
          candidate.workspace.dependencies,
          inputs.migrations ?? [],
        )
        const digest = yield* diffDigest(diff)
        const report = yield* runValidation(
          candidate.manifest,
          candidate.digest,
          candidate.workspace,
          inputs,
        )
        yield* append(
          VALIDATION_COMPONENT,
          toJson({ report, diff, diffDigest: digest }),
          "frozen",
          "candidate-supervisor",
          "coordination",
        )
        return yield* find(candidateId)
      })

    const requestApproval: CandidateSupervisor["requestApproval"] = (
      candidateId,
      session,
      ttlUs,
    ) =>
      Effect.gen(function* () {
        const verified = yield* verifySession(session.sessionId, session.origin)
        const candidate = yield* find(candidateId)
        if (candidate.state !== "validated") {
          return yield* Effect.fail(
            new CandidateError({ code: "validation_incomplete", detail: candidate.state }),
          )
        }
        if (Option.isNone(candidate.diff) || Option.isNone(candidate.diffDigest)) {
          return yield* Effect.fail(
            new CandidateError({ code: "validation_incomplete", detail: "no capability diff" }),
          )
        }
        const diff = candidate.diff.value
        const minted = yield* mintToken()
        const now = yield* Clock.currentTimeMillis
        const expiresAtUs = Math.floor(now * 1000) + ttlUs
        const manifest: CandidateConsentManifest = {
          candidateId,
          candidateDigest: candidate.digest,
          scope: candidate.manifest.scope,
          capabilityDiffDigest: candidate.diffDigest.value,
          expiresAtUs,
          rendererVersion: CONSENT_RENDERER_VERSION,
          viewRevision: CONSENT_VIEW_REVISION,
          labels: {
            added: capabilityNames(diff.added),
            removed: capabilityNames(diff.removed),
            broadened: capabilityNames(
              diff.changed.filter((entry) => entry.broadening).map((entry) => entry.after),
            ),
            dependencies: diff.dependencyChanges
              .map((entry) => `${entry.id}@${entry.after}`)
              .toSorted()
              .join(", ") || "none",
            scope: candidate.manifest.scope,
          },
        }
        const consentDigest = yield* consentDigestOf(
          Schema.decodeUnknownSync(ConsentManifestSchema)(manifest),
        )
        const display: ConsentDisplay = {
          consentDigest,
          manifest,
          tokenHash: minted.tokenHash,
          renderedAtUs: Math.floor(now * 1000),
          sessionId: verified.sessionId,
          origin: verified.origin,
        }
        yield* append(
          CONSENT_COMPONENT,
          toJson(display),
          "frozen",
          "consent-surface",
          "coordination",
        )
        // The token leaves only through the human delivery port.
        yield* delivery.deliver(verified, minted.token)
        return { display, render: renderCandidateConsent(display) }
      })

    const approve: CandidateSupervisor["approve"] = (candidateId, gesture) =>
      Effect.gen(function* () {
        const verified = yield* verifySession(gesture.sessionId, gesture.origin)
        const candidate = yield* find(candidateId)
        const tokenHash = yield* tokenHashOf(gesture.token)
        const current = yield* Ref.get(index)
        const consent = current.consents[tokenHash]
        if (consent === undefined) {
          return yield* Effect.fail(
            new CandidateError({
              code: "approval_token_invalid",
              detail: "no rendered display for this token",
            }),
          )
        }
        if (consent.display.manifest.candidateId !== candidateId) {
          return yield* Effect.fail(
            new CandidateError({
              code: "approval_digest_mismatch",
              detail: "the receipt names another candidate",
            }),
          )
        }
        if (
          consent.display.sessionId !== verified.sessionId ||
          consent.display.origin !== verified.origin
        ) {
          return yield* Effect.fail(
            new CandidateError({
              code: "approval_session_mismatch",
              detail: "the display was rendered for another session",
            }),
          )
        }
        const now = yield* Clock.currentTimeMillis
        const verdict = yield* verifyGesture(
          consent.display,
          consent.consumedAtUs,
          gesture,
          Math.floor(now * 1000),
        )
        if (Result.isFailure(verdict)) {
          return yield* Effect.fail(
            new CandidateError({
              code: verdict.failure.code,
              detail: verdict.failure.detail,
            }),
          )
        }
        if (verdict.success.candidateDigest !== candidate.digest) {
          return yield* Effect.fail(
            new CandidateError({
              code: "digest_mismatch",
              detail: "the candidate digest changed after render",
            }),
          )
        }
        yield* append(
          APPROVAL_COMPONENT,
          toJson(verdict.success),
          "human_gesture",
          verified.sessionId,
          "human_authority",
        )
        return verdict.success
      })

    const viewIssues = (
      candidate: CandidateRecord,
    ): Effect.Effect<Option.Option<CandidateError>> =>
      Effect.gen(function* () {
        if (candidate.manifest.kind !== "declarative") return Option.none<CandidateError>()
        const read = yield* Effect.result(
          cas.read(
            candidate.manifest.artifact.digest,
            candidate.manifest.artifact.size,
          ),
        )
        if (Result.isFailure(read)) {
          return Option.some(mapFailure(read.failure))
        }
        const bytes = read.success
        const decoded = yield* Effect.result(
          Effect.try(() => JSON.parse(new TextDecoder().decode(bytes))),
        )
        if (Result.isFailure(decoded)) return Option.none<CandidateError>()
        const value = decoded.success
        if (typeof value !== "object" || value === null) return Option.none<CandidateError>()
        const record = value as Record<string, unknown>
        if (!("viewId" in record) && !("schemaVersion" in record)) {
          return Option.none<CandidateError>()
        }
        const parsed = parseViewDocument(value)
        if (Result.isFailure(parsed)) {
          return Option.some(
            new CandidateError({
              code: "view_invalid",
              detail: `schema_invalid: ${parsed.failure.detail}`,
            }),
          )
        }
        const authorizedSources = candidate.report.pipe(
          Option.map((report) => report.authorizedSources),
          Option.getOrElse(() => []),
        )
        const checked = validateViewDocument(parsed.success, { authorizedSources })
        return Result.isFailure(checked)
          ? Option.some(
            new CandidateError({
              code: "view_invalid",
              detail: checked.failure.map((entry) => `${entry.code}: ${entry.detail}`).join("; "),
            }),
          )
          : Option.none<CandidateError>()
      })

    const flip = (
      scope: string,
      candidate: CandidateRecord,
      receipt: string,
      previous: Option.Option<ScopePointer>,
    ): Effect.Effect<ScopePointer, CandidateError> =>
      Effect.gen(function* () {
        const now = yield* Clock.currentTimeMillis
        const pointer: ScopePointer = {
          scope,
          generation: Option.match(previous, {
            onNone: () => 1,
            onSome: (value) => value.generation + 1,
          }),
          candidateId: candidate.manifest.candidateId,
          candidateDigest: candidate.digest,
          stateGeneration: candidate.manifest.generation,
          activatedAtUs: Math.floor(now * 1000),
          activationReceipt: receipt,
          ...(Option.isNone(previous) ? {} : {
            prev: {
              candidateId: previous.value.candidateId,
              stateGeneration: previous.value.stateGeneration,
            },
          }),
        }
        yield* pointers.publish(pointer).pipe(Effect.mapError((error) => mapFailure(error)))
        return pointer
      })

    const restore = (
      scope: string,
      previous: Option.Option<ScopePointer>,
    ): Effect.Effect<void, CandidateError> =>
      Option.match(previous, {
        onNone: () => pointers.clear(scope).pipe(Effect.mapError((error) => mapFailure(error))),
        onSome: (pointer) =>
          pointers.publish(pointer).pipe(Effect.mapError((error) => mapFailure(error))),
      })

    const activate: CandidateSupervisor["activate"] = (candidateId, probe) =>
      Effect.gen(function* () {
        const candidate = yield* find(candidateId)
        if (candidate.state === "rejected" || candidate.state === "quarantined") {
          return yield* Effect.fail(
            new CandidateError({
              code: candidate.state === "quarantined"
                ? "candidate_quarantined"
                : "candidate_rejected",
              detail: candidateId,
            }),
          )
        }
        const nowUs = Math.floor((yield* Clock.currentTimeMillis) * 1000)
        if (candidate.manifest.kind === "executable") {
          if (candidate.state !== "approved") {
            return yield* Effect.fail(
              new CandidateError({
                code: candidate.state === "validated"
                  ? "approval_required"
                  : "validation_incomplete",
                detail: `${candidateId} is ${candidate.state}`,
              }),
            )
          }
          const issues = approvalIssues(candidate, nowUs)
          if (Option.isSome(issues)) return yield* Effect.fail(issues.value)
          const candidateApproval = candidate.approval
          if (Option.isNone(candidateApproval)) {
            return yield* Effect.fail(
              new CandidateError({ code: "approval_required", detail: candidateId }),
            )
          }
          const commitment = yield* approvalCommitmentIssue(candidateApproval.value)
          if (Option.isSome(commitment)) return yield* Effect.fail(commitment.value)
        } else {
          if (candidate.state !== "validated") {
            return yield* Effect.fail(
              new CandidateError({
                code: "validation_incomplete",
                detail: `declarative ${candidateId} is ${candidate.state}`,
              }),
            )
          }
          const invalid = yield* viewIssues(candidate)
          if (Option.isSome(invalid)) return yield* Effect.fail(invalid.value)
        }
        const current = yield* pointers.read(candidate.manifest.scope).pipe(
          Effect.mapError((error) => mapFailure(error)),
        )
        const key = activationKey(candidate.manifest.scope, candidateId, candidate.digest)
        // Flip first: a failed flip leaves the previous candidate live and records nothing.
        const pointer = yield* flip(candidate.manifest.scope, candidate, key, current)
        const health = yield* probe
        if (health === "fail") {
          yield* restore(candidate.manifest.scope, current)
          const failure = yield* append(
            FAILURE_COMPONENT,
            toJson({
              candidateId,
              candidateDigest: candidate.digest,
              scope: candidate.manifest.scope,
              health: "fail" as const,
              reason: "post-flip health failed",
              ...(Option.isSome(current) ? { restored: current.value } : {}),
            }),
            "frozen",
            "activation-supervisor",
            "activation_receipt",
            `failure:${key}`,
          )
          return {
            candidateId,
            health: "fail" as const,
            receipt: failure.record.record_id,
            pointer: current,
            state: "failed_health" as const,
          }
        }
        const activation = yield* append(
          ACTIVATION_COMPONENT,
          toJson({
            candidateId,
            candidateDigest: candidate.digest,
            scope: candidate.manifest.scope,
            health: "pass" as const,
            activationReceipt: key,
            pointerGeneration: pointer.generation,
            ...(Option.isSome(current) ? { previous: current.value } : {}),
          }),
          "frozen",
          "activation-supervisor",
          "activation_receipt",
          key,
        ).pipe(
          // The pointer is already flipped; if the receipt cannot be recorded,
          // put the previous candidate back rather than run unrecorded.
          Effect.tapError(() => restore(candidate.manifest.scope, current)),
        )
        return {
          candidateId,
          health: "pass" as const,
          receipt: activation.record.record_id,
          pointer: Option.some(pointer),
          state: "active" as const,
        }
      })

    const activationChain = (
      scope: string,
      current: ScopePointer,
    ): Effect.Effect<Result.Result<ActivationPayload, CandidateError>> =>
      Effect.gen(function* () {
        const records = yield* Effect.result(ledger.records)
        if (Result.isFailure(records)) return Result.fail(mapFailure(records.failure))
        const activation = records.success.find((record) =>
          record.idempotency_key === current.activationReceipt
        )
        const payload = activation === undefined
          ? undefined
          : componentPayload(activation, ACTIVATION_COMPONENT)
        if (payload === undefined) {
          return Result.fail(
            new CandidateError({
              code: "pointer_corrupt",
              detail: `${scope} points at an activation that is not in the ledger`,
            }),
          )
        }
        const parsed = Schema.decodeUnknownSync(ActivationPayload)(payload)
        const previous = parsed.previous
        if (current.prev === undefined) {
          return previous === undefined ? Result.succeed(parsed) : Result.fail(
            new CandidateError({
              code: "pointer_corrupt",
              detail: "the ledger records a predecessor the pointer lost",
            }),
          )
        }
        if (
          previous === undefined ||
          previous.candidateId !== current.prev.candidateId ||
          previous.stateGeneration !== current.prev.stateGeneration ||
          previous.generation !== current.generation - 1
        ) {
          return Result.fail(
            new CandidateError({
              code: "pointer_corrupt",
              detail: "the pointer predecessor disagrees with the ledger activation",
            }),
          )
        }
        return Result.succeed(parsed)
      })

    const rollback: CandidateSupervisor["rollback"] = (scope, reason) =>
      Effect.gen(function* () {
        const current = yield* pointers.read(scope).pipe(
          Effect.mapError((error) => mapFailure(error)),
        )
        if (Option.isNone(current)) {
          return yield* Effect.fail(
            new CandidateError({ code: "pointer_missing", detail: scope }),
          )
        }
        const pointer = current.value
        const target = pointer.prev
        if (target === undefined) {
          // Nothing is live before this pointer: a repeated rollback is a no-op.
          return yield* Effect.fail(
            new CandidateError({ code: "no_rollback_target", detail: scope }),
          )
        }
        const chain = yield* activationChain(scope, pointer)
        if (Result.isFailure(chain)) return yield* Effect.fail(chain.failure)
        const restored = yield* find(target.candidateId)
        if (restored.state !== "active") {
          return yield* Effect.fail(
            new CandidateError({
              code: "validation_incomplete",
              detail: `rollback target ${target.candidateId} is ${restored.state}`,
            }),
          )
        }
        if (restored.manifest.kind === "executable") {
          const nowUs = Math.floor((yield* Clock.currentTimeMillis) * 1000)
          const issues = approvalIssues(restored, nowUs)
          if (Option.isSome(issues)) return yield* Effect.fail(issues.value)
          const restoredApproval = restored.approval
          if (Option.isNone(restoredApproval)) {
            return yield* Effect.fail(
              new CandidateError({ code: "approval_required", detail: target.candidateId }),
            )
          }
          const commitment = yield* approvalCommitmentIssue(restoredApproval.value)
          if (Option.isSome(commitment)) return yield* Effect.fail(commitment.value)
        }
        const receipt = yield* append(
          ROLLBACK_COMPONENT,
          toJson({
            scope,
            from: pointer.candidateId,
            to: target.candidateId,
            fromGeneration: pointer.generation,
            fromReceipt: pointer.activationReceipt,
            reason,
          }),
          "frozen",
          "activation-supervisor",
          "activation_receipt",
          `rollback:${scope}:${pointer.activationReceipt}`,
        )
        const previousPointer = chain.success.previous
        if (previousPointer === undefined) {
          yield* pointers.clear(scope).pipe(Effect.mapError((error) => mapFailure(error)))
          return { scope, receipt: receipt.record.record_id, pointer: Option.none() }
        }
        // Restore the predecessor directly; end its chain so a repeated rollback
        // cannot walk past the candidate the human asked for.
        const restoredPointer: ScopePointer = {
          ...previousPointer,
          generation: pointer.generation + 1,
        }
        yield* pointers.publish(restoredPointer).pipe(
          Effect.mapError((error) => mapFailure(error)),
        )
        return {
          scope,
          receipt: receipt.record.record_id,
          pointer: Option.some(restoredPointer),
        }
      })

    return {
      stage,
      validate,
      requestApproval,
      approve,
      activate,
      rollback,
      candidate: (candidateId) =>
        Ref.get(index).pipe(
          Effect.map((current) => {
            const found = current.candidates[candidateId]
            return found === undefined ? Option.none() : Option.some(found)
          }),
        ),
      candidates: candidatesEffect,
      pointer: (scope) => pointers.read(scope).pipe(Effect.mapError((error) => mapFailure(error))),
    }
  }),
)
