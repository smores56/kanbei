import { Clock, Context, Data, Effect, Layer, Option, Ref } from "effect"
import type { SessionKey } from "./key.ts"
import { exportPkcs8, generateSessionKey, signDerBytes } from "./key.ts"
import {
  buildCsrPem,
  CsrPolicyError,
  parseCsrPem,
  splitChain,
  verifyIssuedChain,
} from "./pkcs10.ts"
import { pemEncode } from "./der.ts"
import type { InstalledCertificate } from "./store.ts"
import { CertificateStore } from "./store.ts"
import { CSR_COMMON_NAME, csrFieldsIssue } from "./schema.ts"

export const CSR_RENEWAL_WINDOW_MS = 30 * 24 * 60 * 60 * 1000

export interface CsrConfig {
  readonly commonName: string
  readonly renewalWindowMs: number
}

export class CsrTransportError extends Data.TaggedError("CsrTransportError")<{
  readonly reason: string
}> {}

export interface CsrTransportReply {
  readonly chain: string
  readonly notAfter: string
}

export interface CsrTransport {
  readonly exchange: (csrPem: string) => Effect.Effect<CsrTransportReply, CsrTransportError>
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
  readonly keyAlgorithm: "ec-p256"
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

const KEY_ALGORITHM = "ec-p256" as const

const fail = (code: CsrErrorCode, detail: string): CsrError => new CsrError({ code, detail })

const now = (): Effect.Effect<number> => Clock.currentTimeMillis

const policy = (error: unknown): string =>
  error instanceof CsrPolicyError || error instanceof Error
    ? error.message.slice(0, 300)
    : String(error)

const configIssues = (config: CsrConfig): ReadonlyArray<string> => {
  const issues: Array<string> = []
  if (!Number.isFinite(config.renewalWindowMs) || config.renewalWindowMs < 0) {
    issues.push("renewalWindowMs must not be negative")
  }
  return issues
}

const dueAt = (
  config: CsrConfig,
  active: Option.Option<InstalledCertificate>,
  at: number,
): boolean =>
  Option.isNone(active) ||
  !(Date.parse(active.value.notAfter) - at > config.renewalWindowMs)

export const csrClientLayer = (
  config: CsrConfig,
): Layer.Layer<CsrClient, never, CsrTransport | CertificateStore | AlertSink> =>
  Layer.effect(
    CsrClient,
    Effect.gen(function* () {
      const issues = configIssues(config)
      if (issues.length > 0) {
        return yield* Effect.die(new Error(`invalid csr config: ${issues.join("; ")}`))
      }
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

      const receipt = (
        kind: CsrReceipt["kind"],
        outcome: CsrReceipt["outcome"],
        detail: string,
      ): Effect.Effect<void> =>
        Effect.gen(function* () {
          const at = new Date(yield* now()).toISOString()
          yield* Ref.update(receipts, (current) => [
            ...current,
            {
              kind,
              commonName: config.commonName,
              keyAlgorithm: KEY_ALGORITHM,
              sans: [config.commonName],
              outcome,
              detail,
              at,
            },
          ])
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

      const buildRequest = (key: SessionKey): Effect.Effect<string, CsrError> =>
        Effect.tryPromise({
          try: () =>
            buildCsrPem({
              commonName: config.commonName,
              spkiDer: key.publicKeySpkiDer,
              sign: (info) => signDerBytes(key, info),
            }),
          catch: (error) => fail("csr_key_failed", policy(error)),
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
        if (Option.isSome(activeBefore) && Date.parse(activeBefore.value.notAfter) <= at) {
          yield* raise({
            kind: "certificate_expired",
            detail: `certificate expired at ${activeBefore.value.notAfter}`,
            notAfter: Option.some(activeBefore.value.notAfter),
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
        const csrPem = yield* refuseOnError(buildRequest(key))
        // The host parser is the authority; never send a CSR it would refuse.
        yield* refuseOnError(
          Effect.try({
            try: () => parseCsrPem(csrPem, CSR_COMMON_NAME),
            catch: (error) => fail("csr_fields_refused", policy(error)),
          }).pipe(Effect.asVoid),
        )
        const response = yield* refuseOnError(
          transport.exchange(csrPem).pipe(
            Effect.mapError((error) => fail("csr_transport_failed", error.reason)),
          ),
        )
        const verified = yield* refuseOnError(
          Effect.try({
            try: () => verifyIssuedChain(response.chain, key.publicKeySpkiDer, CSR_COMMON_NAME, at),
            catch: (error) => fail("csr_response_refused", policy(error)),
          }),
        )
        if (verified.notAfter !== response.notAfter) {
          return yield* refuseOnError(
            Effect.fail(
              fail("csr_response_refused", "response not_after disagrees with the leaf"),
            ),
          )
        }
        const privateKeyPkcs8 = yield* refuseOnError(
          exportPkcs8(key).pipe(Effect.mapError((error) => fail("csr_key_failed", error.reason))),
        )
        const leafDer = splitChain(response.chain)[0]
        if (leafDer === undefined) {
          return yield* refuseOnError(
            Effect.fail(fail("csr_response_refused", "chain holds no leaf")),
          )
        }
        const certificate: InstalledCertificate = {
          chainPem: response.chain,
          leafPem: pemEncode("CERTIFICATE", leafDer),
          privateKeyPkcs8,
          spki: key.publicKeySpki,
          notAfter: verified.notAfter,
          installedAt: new Date(at).toISOString(),
        }
        yield* refuseOnError(
          store.stage(certificate).pipe(
            Effect.mapError((error) => fail("csr_store_failed", error.reason)),
          ),
        )
        yield* receipt("renewal_request", "ok", `issued until ${verified.notAfter}`)
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
