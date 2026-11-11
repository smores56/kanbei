import { Clock, Data, Effect, Ref, Schema, Scope } from "effect"
import { canonicalize } from "../evidence/canonical.ts"
import { sha256Text } from "../evidence/digest.ts"
import type { CapabilityManifest, CapabilityName } from "../kernel/capabilities.ts"
import { capabilityManifestIssues, isCapabilityName, stubNameFor } from "../kernel/capabilities.ts"
import { MontyFailure } from "./errors.ts"
import type { HostStub } from "./stubs.ts"
import { stub } from "./stubs.ts"

export type CapabilityRefusalCode =
  | "manifest_invalid"
  | "capability_undeclared"
  | "capability_unbound"
  | "binding_invalid"

export class CapabilityRefusal extends Data.TaggedError("CapabilityRefusal")<{
  readonly code: CapabilityRefusalCode
  readonly detail: string
}> {}

export type CapabilityCallFailureCode =
  | "capability_denied"
  | "capability_failed"
  | "capability_revoked"

export class CapabilityCallFailure extends Data.TaggedError("CapabilityCallFailure")<{
  readonly code: CapabilityCallFailureCode
  readonly capability: string
  readonly detail: string
}> {}

/** One closure over a channel adapter, named by the capability it implements. */
export interface CapabilityBinding {
  readonly capability: CapabilityName
  readonly run: (
    args: ReadonlyArray<unknown>,
  ) => Effect.Effect<unknown, CapabilityCallFailure>
}

export type CapabilityReceiptOutcome = "ok" | "failed" | "denied" | "revoked"

export interface CapabilityReceipt {
  readonly agentId: string
  readonly sandboxId: string
  readonly capability: string
  readonly outcome: CapabilityReceiptOutcome
  readonly requestDigest: string
  readonly at: string
}

export interface MediationRequest {
  readonly manifest: CapabilityManifest
  readonly bindings: ReadonlyArray<CapabilityBinding>
}

export interface MediatedCapabilities {
  readonly agentId: string
  readonly sandboxId: string
  readonly capabilities: ReadonlyArray<CapabilityName>
  /** Stubs generated from the manifest; nothing outside it exists. */
  readonly stubs: ReadonlyArray<HostStub>
  readonly invoke: (
    capability: string,
    args: ReadonlyArray<unknown>,
  ) => Effect.Effect<unknown, CapabilityCallFailure>
  readonly receipts: Effect.Effect<ReadonlyArray<CapabilityReceipt>>
  readonly revoked: Effect.Effect<boolean>
}

const refusal = (code: CapabilityRefusalCode, detail: string): CapabilityRefusal =>
  new CapabilityRefusal({ code, detail })

const digestArgs = (args: ReadonlyArray<unknown>): Effect.Effect<string> =>
  Effect.try({
    try: () => canonicalize(Schema.decodeUnknownSync(Schema.Json)(args)),
    catch: () => "<unencodable>",
  }).pipe(
    Effect.flatMap(sha256Text),
    Effect.orDie,
  )

const at = (): Effect.Effect<string> =>
  Clock.currentTimeMillis.pipe(Effect.map((now) => new Date(now).toISOString()))

const toMonty = (failure: CapabilityCallFailure, capability: string): MontyFailure =>
  new MontyFailure({
    reason: failure.code === "capability_denied" || failure.code === "capability_revoked"
      ? "capability_denied"
      : "program_error",
    detail: `${capability}: ${failure.detail}`,
  })

const validateBindings = (
  manifest: CapabilityManifest,
  bindings: ReadonlyArray<CapabilityBinding>,
): CapabilityRefusal | undefined => {
  const names = bindings.map((binding) => binding.capability)
  if (new Set(names).size !== names.length) {
    return refusal("binding_invalid", "duplicate capability binding")
  }
  const unknown = names.filter((name) => !isCapabilityName(name))
  if (unknown.length > 0) {
    return refusal("binding_invalid", `unknown capability binding: ${unknown.join(",")}`)
  }
  const declared = new Set<CapabilityName>(manifest.capabilities)
  const extra = names.filter((name) => !declared.has(name))
  if (extra.length > 0) {
    return refusal(
      "capability_undeclared",
      `not declared by ${manifest.agentId}: ${extra.join(",")}`,
    )
  }
  const bound = new Set(names)
  const missing = manifest.capabilities.filter((name) => !bound.has(name))
  if (missing.length > 0) {
    return refusal("capability_unbound", `declared without a binding: ${missing.join(",")}`)
  }
  return undefined
}

/**
 * Per-agent mediation: the manifest names the only closures a program may
 * reach, every call is receipted, and the finalizer revokes them at session end.
 */
export const mediateCapabilities = (
  request: MediationRequest,
): Effect.Effect<MediatedCapabilities, CapabilityRefusal, Scope.Scope> =>
  Effect.gen(function* () {
    const issues = capabilityManifestIssues(request.manifest)
    if (issues.length > 0) {
      return yield* Effect.fail(refusal("manifest_invalid", issues.join("; ")))
    }
    const invalid = validateBindings(request.manifest, request.bindings)
    if (invalid !== undefined) return yield* Effect.fail(invalid)

    const revoked = yield* Ref.make(false)
    const receipts = yield* Ref.make<ReadonlyArray<CapabilityReceipt>>([])
    yield* Effect.addFinalizer(() => Ref.set(revoked, true))

    const record = (receipt: CapabilityReceipt): Effect.Effect<void> =>
      Ref.update(receipts, (current) => [...current, receipt])

    const receipt = (
      capability: string,
      outcome: CapabilityReceiptOutcome,
      requestDigest: string,
    ): Effect.Effect<void> =>
      Effect.gen(function* () {
        yield* record({
          agentId: request.manifest.agentId,
          sandboxId: request.manifest.sandboxId,
          capability,
          outcome,
          requestDigest,
          at: yield* at(),
        })
      })

    const invoke = (
      capability: string,
      args: ReadonlyArray<unknown>,
    ): Effect.Effect<unknown, CapabilityCallFailure> =>
      Effect.gen(function* () {
        const requestDigest = yield* digestArgs(args)
        if (yield* Ref.get(revoked)) {
          yield* receipt(capability, "revoked", requestDigest)
          return yield* Effect.fail(
            new CapabilityCallFailure({
              code: "capability_revoked",
              capability,
              detail: "the session ended and its capabilities were revoked",
            }),
          )
        }
        const declared = request.manifest.capabilities.find((name) => name === capability)
        if (declared === undefined) {
          yield* receipt(capability, "denied", requestDigest)
          return yield* Effect.fail(
            new CapabilityCallFailure({
              code: "capability_denied",
              capability,
              detail: "capability is not in the agent manifest",
            }),
          )
        }
        const binding = request.bindings.find((entry) => entry.capability === declared)
        if (binding === undefined) {
          yield* receipt(capability, "denied", requestDigest)
          return yield* Effect.fail(
            new CapabilityCallFailure({
              code: "capability_denied",
              capability,
              detail: "declared capability has no binding",
            }),
          )
        }
        return yield* binding.run(args).pipe(
          Effect.tap(() => receipt(capability, "ok", requestDigest)),
          Effect.tapError(() => receipt(capability, "failed", requestDigest)),
        )
      })

    const stubs = request.manifest.capabilities.map((name) =>
      stub(stubNameFor(name), (args) =>
        invoke(name, args).pipe(Effect.mapError((error) => toMonty(error, name))))
    )

    return {
      agentId: request.manifest.agentId,
      sandboxId: request.manifest.sandboxId,
      capabilities: request.manifest.capabilities,
      stubs,
      invoke,
      receipts: Ref.get(receipts),
      revoked: Ref.get(revoked),
    }
  })
