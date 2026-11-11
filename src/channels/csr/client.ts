import { Clock, Context, Data, Effect, Layer, Option, Ref, Result, Schema } from "effect"
import { canonicalize } from "../../evidence/canonical.ts"
import type { CsrRequest, HostResponse, LeafCertificate } from "./schema.ts"
import { csrFieldsIssue, parseCsrRequest } from "./schema.ts"
import type { SessionKey } from "./key.ts"
import { exportPkcs8, generateSessionKey, isP256Spki, signText, verifyText } from "./key.ts"
import type { InstalledCertificate } from "./store.ts"
import { CertificateStore } from "./store.ts"

export const CSR_RENEWAL_WINDOW_MS = 30 * 24 * 60 * 60 * 1000

export interface CsrConfig {
  readonly commonName: string
  readonly keyAlgorithm: string
  readonly sans: ReadonlyArray<string>
  readonly renewalWindowMs: number
  readonly issuerPublicKeySpki: string
  readonly rootSubject: string
}

export class CsrTransportError extends Data.TaggedError("CsrTransportError")<{
  readonly reason: string
}> {}

export interface CsrTransport {
  readonly exchange: (request: CsrRequest) => Effect.Effect<HostResponse, CsrTransportError>
}

export const CsrTransport = Context.Service<CsrTransport>("kanbei/channel/csr/Transport")

export interface CsrAlert {
  readonly kind: "certificate_expired"
  readonly detail: string
  readonly notAfter: Option.Option<string>
  readonly at: string
}

export interface AlertSink {
  readonly emit: (alert: CsrAlert) => Effect.Effect<void>
}

export const AlertSink = Context.Service<AlertSink>("kanbei/channel/csr/AlertSink")

export type CsrState = "idle" | "requested" | "issued" | "refused" | "expired"

export type CsrErrorCode =
  | "csr_fields_refused"
  | "csr_not_due"
  | "csr_key_failed"
  | "csr_transport_failed"
  | "csr_response_refused"
  | "csr_store_failed"
  | "csr_reload_failed"

export class CsrError extends Data.TaggedError("CsrError")<{
  readonly code: CsrErrorCode
  readonly detail: string
}> {}

export interface CsrReceipt {
  readonly kind: "renewal_request" | "renewal_refused" | "reload"
  readonly commonName: string
  readonly keyAlgorithm: string
  readonly sans: ReadonlyArray<string>
  readonly outcome: "ok" | "refused"
  readonly detail: string
  readonly at: string
}

export interface CsrOutcome {
  readonly state: CsrState
  readonly renewed: boolean
  readonly active: Option.Option<InstalledCertificate>
  readonly staged: Option.Option<InstalledCertificate>
}

export interface CsrClient {
  readonly state: Effect.Effect<CsrState>
  readonly due: Effect.Effect<boolean, CsrError>
  readonly renew: Effect.Effect<CsrOutcome, CsrError>
  readonly reload: Effect.Effect<CsrOutcome, CsrError>
  readonly receipts: Effect.Effect<ReadonlyArray<CsrReceipt>>
  readonly alerts: Effect.Effect<ReadonlyArray<CsrAlert>>
}

export const CsrClient = Context.Service<CsrClient>("kanbei/channel/csr/Client")

const fail = (code: CsrErrorCode, detail: string): CsrError => new CsrError({ code, detail })

const now = (): Effect.Effect<number> => Clock.currentTimeMillis

const asJson = (value: unknown): Schema.Json => Schema.decodeUnknownSync(Schema.Json)(value)

interface RequestBody {
  readonly version: 1
  readonly commonName: string
  readonly keyAlgorithm: string
  readonly sans: ReadonlyArray<string>
  readonly publicKeySpki: string
}

const buildRequest = (
  config: CsrConfig,
  key: SessionKey,
): Effect.Effect<CsrRequest, CsrError> =>
  Effect.gen(function* () {
    const body: RequestBody = {
      version: 1,
      commonName: config.commonName,
      keyAlgorithm: config.keyAlgorithm,
      sans: config.sans,
      publicKeySpki: key.publicKeySpki,
    }
    const signature = yield* signText(key, canonicalize(asJson(body))).pipe(
      Effect.mapError((error) => fail("csr_key_failed", error.reason)),
    )
    const candidate = { ...body, signature }
    const parsed = Result.try({
      try: () => parseCsrRequest(candidate),
      catch: (error) =>
        fail(
          "csr_fields_refused",
          error instanceof Error ? error.message.slice(0, 200) : String(error),
        ),
    })
    if (Result.isFailure(parsed)) return yield* Effect.fail(parsed.failure)
    return parsed.success
  })

const chainIssue = (response: HostResponse, rootSubject: string): string | undefined => {
  if (response.chain.length === 0) return "chain is empty"
  const first = response.chain[0]
  if (first === undefined || first.subject !== response.leaf.issuer) {
    return "leaf issuer is not the first chain subject"
  }
  const broken = response.chain.slice(0, -1).findIndex((current, index) =>
    current.issuer !== response.chain[index + 1]?.subject
  )
  if (broken !== -1) return `chain is broken at ${broken}`
  const last = response.chain.at(-1)
  if (last === undefined || last.issuer !== rootSubject) {
    return `chain root is not ${rootSubject}`
  }
  return undefined
}

const leafExpired = (leaf: LeafCertificate, at: number): boolean => {
  const notAfter = Date.parse(leaf.notAfter)
  return !Number.isFinite(notAfter) || notAfter <= at
}

const certificateOf = (
  response: HostResponse,
  installedAt: number,
  privateKeyPkcs8: string,
): InstalledCertificate => ({
  leaf: response.leaf,
  chain: response.chain,
  privateKeyPkcs8,
  installedAt: new Date(installedAt).toISOString(),
})

const dueAt = (
  config: CsrConfig,
  active: Option.Option<InstalledCertificate>,
  at: number,
): boolean =>
  Option.isNone(active) || Date.parse(active.value.leaf.notAfter) - at <= config.renewalWindowMs

const validateResponse = (
  config: CsrConfig,
  response: HostResponse,
  key: SessionKey,
  at: number,
): Effect.Effect<void, CsrError> =>
  Effect.gen(function* () {
    if (response.leaf.spki !== key.publicKeySpki) {
      return yield* Effect.fail(
        fail("csr_response_refused", "leaf SPKI does not match the session key"),
      )
    }
    if (
      !(yield* isP256Spki(response.leaf.spki).pipe(
        Effect.mapError((error) => fail("csr_key_failed", error.reason)),
      ))
    ) {
      return yield* Effect.fail(fail("csr_response_refused", "leaf SPKI is not P-256"))
    }
    const proofValid = yield* verifyText(
      config.issuerPublicKeySpki,
      response.proof,
      canonicalize(asJson(response.leaf)),
    ).pipe(Effect.mapError((error) => fail("csr_key_failed", error.reason)))
    if (!proofValid) {
      return yield* Effect.fail(fail("csr_response_refused", "leaf proof does not verify"))
    }
    const chain = chainIssue(response, config.rootSubject)
    if (chain !== undefined) {
      return yield* Effect.fail(fail("csr_response_refused", chain))
    }
    if (leafExpired(response.leaf, at)) {
      return yield* Effect.fail(
        fail("csr_response_refused", "returned certificate is already expired"),
      )
    }
  })

export const csrClientLayer = (
  config: CsrConfig,
): Layer.Layer<CsrClient, never, CsrTransport | CertificateStore | AlertSink> =>
  Layer.effect(
    CsrClient,
    Effect.gen(function* () {
      const transport = yield* CsrTransport
      const store = yield* CertificateStore
      const alerts = yield* AlertSink
      const state = yield* Ref.make<CsrState>("idle")
      const receipts = yield* Ref.make<ReadonlyArray<CsrReceipt>>([])
      const alertLog = yield* Ref.make<ReadonlyArray<CsrAlert>>([])

      const readActive = store.active.pipe(
        Effect.mapError((error) => fail("csr_store_failed", error.reason)),
      )
      const readStaged = store.staged.pipe(
        Effect.mapError((error) => fail("csr_store_failed", error.reason)),
      )

      const record = (receipt: CsrReceipt): Effect.Effect<void> =>
        Ref.update(receipts, (current) => [...current, receipt])

      const receipt = (
        kind: CsrReceipt["kind"],
        outcome: CsrReceipt["outcome"],
        detail: string,
      ): Effect.Effect<void> =>
        Effect.gen(function* () {
          yield* record({
            kind,
            commonName: config.commonName,
            keyAlgorithm: config.keyAlgorithm,
            sans: config.sans,
            outcome,
            detail,
            at: new Date(yield* now()).toISOString(),
          })
        })

      const refuseOnError = <A>(effect: Effect.Effect<A, CsrError>): Effect.Effect<A, CsrError> =>
        effect.pipe(
          Effect.tapError((error) =>
            receipt("renewal_refused", "refused", `${error.code}: ${error.detail}`).pipe(
              Effect.flatMap(() => Ref.set(state, "refused")),
            )
          ),
        )

      const raise = (alert: CsrAlert): Effect.Effect<void> =>
        Effect.gen(function* () {
          yield* Ref.update(alertLog, (current) => [...current, alert])
          yield* alerts.emit(alert)
        })

      const outcome = (
        next: CsrState,
        renewed: boolean,
      ): Effect.Effect<CsrOutcome, CsrError> =>
        Effect.gen(function* () {
          const active = yield* readActive
          const staged = yield* readStaged
          yield* Ref.set(state, next)
          return { state: next, renewed, active, staged }
        })

      const renew: CsrClient["renew"] = Effect.gen(function* () {
        const fields = csrFieldsIssue(config)
        const activeBefore = yield* readActive
        if (fields !== undefined) {
          yield* receipt("renewal_refused", "refused", `csr_fields_refused: ${fields}`)
          yield* Ref.set(state, "refused")
          return yield* Effect.fail(fail("csr_fields_refused", fields))
        }
        const at = yield* now()
        if (Option.isSome(activeBefore) && Date.parse(activeBefore.value.leaf.notAfter) <= at) {
          yield* raise({
            kind: "certificate_expired",
            detail: `certificate expired at ${activeBefore.value.leaf.notAfter}`,
            notAfter: Option.some(activeBefore.value.leaf.notAfter),
            at: new Date(at).toISOString(),
          })
          yield* Ref.set(state, "expired")
        }
        if (!dueAt(config, activeBefore, at)) {
          return yield* Effect.fail(
            fail("csr_not_due", "certificate is outside the renewal window"),
          )
        }
        yield* Ref.set(state, "requested")
        const key = yield* refuseOnError(
          generateSessionKey().pipe(
            Effect.mapError((error) => fail("csr_key_failed", error.reason)),
          ),
        )
        const built = yield* refuseOnError(buildRequest(config, key))
        const response = yield* refuseOnError(
          transport.exchange(built).pipe(
            Effect.mapError((error) => fail("csr_transport_failed", error.reason)),
          ),
        )
        yield* refuseOnError(validateResponse(config, response, key, at))
        const privateKeyPkcs8 = yield* refuseOnError(
          exportPkcs8(key).pipe(Effect.mapError((error) => fail("csr_key_failed", error.reason))),
        )
        const certificate = certificateOf(response, at, privateKeyPkcs8)
        yield* refuseOnError(
          store.stage(certificate).pipe(
            Effect.mapError((error) => fail("csr_store_failed", error.reason)),
          ),
        )
        yield* receipt(
          "renewal_request",
          "ok",
          `issued until ${response.leaf.notAfter}`,
        )
        return yield* outcome("issued", true)
      })

      return {
        state: Ref.get(state),
        due: Effect.gen(function* () {
          const at = yield* now()
          return dueAt(config, yield* readActive, at)
        }),
        renew,
        reload: Effect.gen(function* () {
          const staged = yield* readStaged
          if (Option.isNone(staged)) {
            return yield* Effect.fail(fail("csr_reload_failed", "no staged certificate"))
          }
          yield* store.promote.pipe(
            Effect.mapError((error) => fail("csr_store_failed", error.reason)),
          )
          yield* receipt("reload", "ok", "staged certificate is active")
          return yield* outcome("issued", false)
        }),
        receipts: Ref.get(receipts),
        alerts: Ref.get(alertLog),
      }
    }),
  )
