import { Clock, Context, Data, Effect, Layer, Option, Ref, Result, Schema } from "effect"
import { CasError, ContentStore } from "../kernel/cas.ts"
import { Ledger, LedgerError } from "../kernel/ledger.ts"
import type { LedgerRecord } from "../kernel/ledger.ts"
import {
  CONSENT_RENDERER_VERSION,
  CONSENT_VIEW_REVISION,
  consentDigestOf,
  ConsentDisplay,
  mintToken,
  renderCandidateConsent,
  tokenHashOf,
  verifyGesture,
} from "./consent.ts"
import type {
  CandidateApprovalReceipt,
  CandidateConsentManifest,
  ConsentGesture,
  FrozenConsentRender,
} from "./consent.ts"
import { CandidateConsentManifest as ConsentManifestSchema } from "./consent.ts"
import {
  candidateDigest,
  CandidateManifest,
  CandidateWorkspace,
  CapabilityDiff,
  capabilityDiff,
  diffDigest,
  digestOfJson,
} from "./manifest.ts"
import { PointerError, PointerStore, ScopePointer } from "./pointer.ts"
import { runValidation, ValidationReport } from "./validation.ts"
import type { ValidationInputs } from "./validation.ts"

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
  | "approval_token_invalid"
  | "approval_already_consumed"
  | "approval_session_mismatch"
  | "approval_expired"
  | "approval_digest_mismatch"
  | "digest_mismatch"
  | "scope_mismatch"
  | "pointer_missing"
  | "store_failed"
  | "ledger_failed"

export class CandidateError extends Data.TaggedError("CandidateError")<{
  readonly code: CandidateErrorCode
  readonly detail: string
}> {}

export interface ApprovalRequestResult {
  readonly display: ConsentDisplay
  readonly token: string
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

/**
 * The activation supervisor: staging, the eight validations, the capability
 * diff, the frozen consent render and single-use approval, one pointer flip per
 * scope, post-flip health, rollback, and previous-candidate retention.
 */
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
    sessionId: string,
    ttlUs: number,
  ) => Effect.Effect<ApprovalRequestResult, CandidateError>
  readonly approve: (
    candidateId: string,
    gesture: ConsentGesture,
  ) => Effect.Effect<CandidateApprovalReceipt, CandidateError>
  readonly activate: (
    candidateId: string,
    health: "pass" | "fail",
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
  previous: Schema.optional(ScopePointer),
})
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
  to: Schema.optional(Schema.String),
  reason: Schema.String,
})

interface ConsentRecord {
  readonly display: ConsentDisplay
  readonly consumedAtUs: Option.Option<number>
}

interface IndexState {
  readonly candidates: Readonly<Record<string, CandidateRecord>>
  readonly consents: Readonly<Record<string, ConsentRecord>>
}

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
    const receipt = Schema.decodeUnknownSync(
      Schema.Struct({
        candidateId: Schema.String,
        candidateDigest: Schema.String,
        capabilityDiffDigest: Schema.String,
        scope: Schema.String,
        expiresAtUs: Schema.Number,
        sessionId: Schema.String,
        consentDigest: Schema.String,
        ruling: Schema.Literal("approve"),
        consumedAtUs: Schema.Number,
      }),
    )(approvalPayload)
    const candidate = index.candidates[receipt.candidateId]
    return {
      ...index,
      candidates: candidate === undefined ? index.candidates : {
        ...index.candidates,
        [receipt.candidateId]: { ...candidate, state: "approved", approval: Option.some(receipt) },
      },
      consents: Object.fromEntries(
        Object.entries(index.consents).map(([hash, entry]) => [
          hash,
          entry.display.consentDigest === receipt.consentDigest
            ? { ...entry, consumedAtUs: Option.some(receipt.consumedAtUs) }
            : entry,
        ]),
      ),
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
    const previous = payload.from === undefined ? undefined : index.candidates[payload.from]
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

export const candidateSupervisorLayer: Layer.Layer<
  CandidateSupervisor,
  never,
  Ledger | ContentStore | PointerStore
> = Layer.effect(
  CandidateSupervisor,
  Effect.gen(function* () {
    const ledger = yield* Ledger
    const cas = yield* ContentStore
    const pointers = yield* PointerStore
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
    ) =>
      ledger.append({
        components: [{ type, payload }],
        appender: role,
        principal,
        authorityClass,
      }).pipe(
        Effect.mapError((error) => mapFailure(error)),
        Effect.flatMap((result) =>
          Ref.update(index, (current) => applyRecord(current, result.record)).pipe(
            Effect.as(result),
          )
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
        const report = runValidation(
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
      sessionId,
      ttlUs,
    ) =>
      Effect.gen(function* () {
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
        const token = yield* mintToken()
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
            added: `${diff.added.length} capabilities added`,
            removed: `${diff.removed.length} capabilities removed`,
            dependencies: `${diff.dependencyChanges.length} dependency changes`,
            scope: candidate.manifest.scope,
          },
        }
        const consentDigest = yield* consentDigestOf(
          Schema.decodeUnknownSync(ConsentManifestSchema)(manifest),
        )
        const display: ConsentDisplay = {
          consentDigest,
          manifest,
          tokenHash: token.tokenHash,
          renderedAtUs: Math.floor(now * 1000),
          sessionId,
        }
        yield* append(
          CONSENT_COMPONENT,
          toJson(display),
          "frozen",
          "consent-surface",
          "coordination",
        )
        return { display, token: token.token, render: renderCandidateConsent(display) }
      })

    const approve: CandidateSupervisor["approve"] = (candidateId, gesture) =>
      Effect.gen(function* () {
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
          gesture.sessionId,
          "human_authority",
        )
        return verdict.success
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

    const activate: CandidateSupervisor["activate"] = (candidateId, health) =>
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
        if (candidate.manifest.kind === "executable") {
          if (Option.isNone(candidate.approval)) {
            return yield* Effect.fail(
              new CandidateError({ code: "approval_required", detail: candidateId }),
            )
          }
          const approval = candidate.approval.value
          if (approval.candidateDigest !== candidate.digest) {
            return yield* Effect.fail(
              new CandidateError({
                code: "digest_mismatch",
                detail: "the approval binds another candidate digest",
              }),
            )
          }
          if (approval.scope !== candidate.manifest.scope) {
            return yield* Effect.fail(
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
            return yield* Effect.fail(
              new CandidateError({
                code: "digest_mismatch",
                detail: "the approval binds another capability diff",
              }),
            )
          }
          const now = yield* Clock.currentTimeMillis
          if (Math.floor(now * 1000) >= approval.expiresAtUs) {
            return yield* Effect.fail(
              new CandidateError({ code: "approval_expired", detail: candidateId }),
            )
          }
        }
        const current = yield* pointers.read(candidate.manifest.scope).pipe(
          Effect.mapError((error) => mapFailure(error)),
        )
        if (health === "pass") {
          const receipt = yield* append(
            ACTIVATION_COMPONENT,
            toJson({
              candidateId,
              candidateDigest: candidate.digest,
              scope: candidate.manifest.scope,
              health: "pass" as const,
              ...(Option.isSome(current) ? { previous: current.value } : {}),
            }),
            "frozen",
            "activation-supervisor",
            "activation_receipt",
          )
          const pointer = yield* flip(
            candidate.manifest.scope,
            candidate,
            receipt.record.record_id,
            current,
          )
          return {
            candidateId,
            health: "pass" as const,
            receipt: receipt.record.record_id,
            pointer: Option.some(pointer),
            state: "active" as const,
          }
        }
        const failure = yield* append(
          FAILURE_COMPONENT,
          toJson({
            candidateId,
            candidateDigest: candidate.digest,
            scope: candidate.manifest.scope,
            health: "fail" as const,
            reason: "post-flip health failed",
          }),
          "frozen",
          "activation-supervisor",
          "activation_receipt",
        )
        // The failed candidate briefly holds the pointer, then the previous one is
        // restored; external effects stay in the ledger either way.
        yield* flip(candidate.manifest.scope, candidate, failure.record.record_id, current)
        if (Option.isSome(current)) {
          yield* pointers.publish(current.value).pipe(
            Effect.mapError((error) => mapFailure(error)),
          )
        } else {
          yield* pointers.clear(candidate.manifest.scope).pipe(
            Effect.mapError((error) => mapFailure(error)),
          )
        }
        return {
          candidateId,
          health: "fail" as const,
          receipt: failure.record.record_id,
          pointer: current,
          state: "failed_health" as const,
        }
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
        const target = current.value.prev
        const receipt = yield* append(
          ROLLBACK_COMPONENT,
          toJson({
            scope,
            from: current.value.candidateId,
            ...(target === undefined ? {} : { to: target.candidateId }),
            reason,
          }),
          "frozen",
          "activation-supervisor",
          "activation_receipt",
        )
        if (target === undefined) {
          yield* pointers.clear(scope).pipe(Effect.mapError((error) => mapFailure(error)))
          return { scope, receipt: receipt.record.record_id, pointer: Option.none() }
        }
        const restored = yield* find(target.candidateId).pipe(
          Effect.flatMap((candidate) =>
            flip(scope, candidate, receipt.record.record_id, Option.none())
          ),
        )
        return { scope, receipt: receipt.record.record_id, pointer: Option.some(restored) }
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
