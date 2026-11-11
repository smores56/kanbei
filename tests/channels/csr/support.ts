import { Clock, Effect, Layer, Option, Ref } from "effect"
import { canonicalize } from "../../../src/evidence/canonical.ts"
import { Schema } from "effect"
import type { CsrAlert, CsrConfig } from "../../../src/channels/csr/client.ts"
import {
  CSR_RENEWAL_WINDOW_MS,
  CsrClient,
  csrClientLayer,
} from "../../../src/channels/csr/client.ts"
import type { CsrRequest, HostResponse, LeafCertificate } from "../../../src/channels/csr/schema.ts"
import { CSR_COMMON_NAME, CSR_KEY_ALGORITHM, CSR_SANS } from "../../../src/channels/csr/schema.ts"
import { CsrTransport } from "../../../src/channels/csr/client.ts"
import { CertificateStore } from "../../../src/channels/csr/store.ts"
import type { InstalledCertificate } from "../../../src/channels/csr/store.ts"
import { AlertSink } from "../../../src/channels/csr/client.ts"
import type { SessionKey } from "../../../src/channels/csr/key.ts"
import { generateSessionKey, signText, verifyText } from "../../../src/channels/csr/key.ts"

const DAY = 24 * 60 * 60 * 1000

export const clockAt = (millis: number): Clock.Clock => ({
  currentTimeMillisUnsafe: () => millis,
  currentTimeMillis: Effect.succeed(millis),
  currentTimeNanosUnsafe: () => BigInt(millis) * 1_000_000n,
  currentTimeNanos: Effect.succeed(BigInt(millis) * 1_000_000n),
  monotonicTimeNanosUnsafe: () => 0n,
  monotonicTimeNanos: Effect.succeed(0n),
  sleep: () => Effect.void,
})

export interface MemoryStore {
  readonly layer: Layer.Layer<CertificateStore>
  readonly active: Ref.Ref<Option.Option<InstalledCertificate>>
  readonly staged: Ref.Ref<Option.Option<InstalledCertificate>>
}

export const memoryStore = (
  initial: Option.Option<InstalledCertificate>,
): Effect.Effect<MemoryStore> =>
  Effect.gen(function* () {
    const active = yield* Ref.make(initial)
    const staged = yield* Ref.make<Option.Option<InstalledCertificate>>(Option.none())
    const layer = Layer.succeed(CertificateStore)({
      active: Ref.get(active),
      staged: Ref.get(staged),
      stage: (certificate) => Ref.set(staged, Option.some(certificate)),
      promote: Effect.gen(function* () {
        const current = yield* Ref.get(staged)
        if (Option.isNone(current)) return Option.none()
        yield* Ref.set(active, current)
        yield* Ref.set(staged, Option.none())
        return current
      }),
    })
    return { layer, active, staged }
  })

export interface FakeSigner {
  readonly issuer: SessionKey
  readonly responder: (request: CsrRequest) => Effect.Effect<HostResponse, never>
}

const leafFor = (
  request: CsrRequest,
  now: number,
  validityMs: number,
): LeafCertificate => ({
  subject: CSR_COMMON_NAME,
  issuer: "fake-ca",
  spki: request.publicKeySpki,
  notBefore: new Date(now - DAY).toISOString(),
  notAfter: new Date(now + validityMs).toISOString(),
})

/** A fake CA: verifies the session signature and signs the leaf with a pinned key. */
export const fakeSigner = (
  now: number,
  validityMs = 90 * DAY,
): Effect.Effect<FakeSigner> =>
  Effect.gen(function* () {
    const issuer = yield* generateSessionKey().pipe(Effect.orDie)
    const respond = (request: CsrRequest): Effect.Effect<HostResponse> =>
      Effect.gen(function* () {
        const body = {
          version: request.version,
          commonName: request.commonName,
          keyAlgorithm: request.keyAlgorithm,
          sans: request.sans,
          publicKeySpki: request.publicKeySpki,
        }
        const valid = yield* verifyText(
          request.publicKeySpki,
          request.signature,
          canonicalize(Schema.decodeUnknownSync(Schema.Json)(body)),
        ).pipe(Effect.orDie)
        if (!valid) return yield* Effect.die("fake CA received an invalid signature")
        const leaf = leafFor(request, now, validityMs)
        const proof = yield* signText(
          issuer,
          canonicalize(Schema.decodeUnknownSync(Schema.Json)(leaf)),
        ).pipe(Effect.orDie)
        return {
          version: 1,
          leaf,
          chain: [
            { subject: "fake-ca", issuer: "root-ca", spki: issuer.publicKeySpki },
            { subject: "root-ca", issuer: "root-ca", spki: issuer.publicKeySpki },
          ],
          proof,
        }
      })
    return { issuer, responder: respond }
  })

export interface FakeTransport {
  readonly layer: Layer.Layer<CsrTransport>
  readonly calls: Ref.Ref<ReadonlyArray<CsrRequest>>
}

export const fakeTransport = (
  signer: FakeSigner,
  mutate: (response: HostResponse) => HostResponse = (response) => response,
): Effect.Effect<FakeTransport> =>
  Effect.gen(function* () {
    const calls = yield* Ref.make<ReadonlyArray<CsrRequest>>([])
    const layer = Layer.succeed(CsrTransport)({
      exchange: (request) =>
        Ref.update(calls, (current) => [...current, request]).pipe(
          Effect.flatMap(() => signer.responder(request)),
          Effect.map(mutate),
        ),
    })
    return { layer, calls }
  })

export interface AlertRecorder {
  readonly layer: Layer.Layer<AlertSink>
  readonly alerts: Ref.Ref<ReadonlyArray<CsrAlert>>
}

export const alertRecorder = (): Effect.Effect<AlertRecorder> =>
  Effect.gen(function* () {
    const alerts = yield* Ref.make<ReadonlyArray<CsrAlert>>([])
    const layer = Layer.succeed(AlertSink)({
      emit: (alert) => Ref.update(alerts, (current) => [...current, alert]),
    })
    return { layer, alerts }
  })

export interface Harness {
  readonly clients: Layer.Layer<CsrClient>
  readonly transport: FakeTransport
  readonly store: MemoryStore
  readonly alerts: AlertRecorder
  readonly config: CsrConfig
}

export const harness = (
  options: {
    readonly now: number
    readonly validityMs?: number
    readonly config?: Partial<CsrConfig>
    readonly active?: Option.Option<InstalledCertificate>
    readonly mutate?: (response: HostResponse) => HostResponse
  },
): Effect.Effect<Harness> =>
  Effect.gen(function* () {
    const signer = yield* fakeSigner(options.now, options.validityMs)
    const transport = yield* fakeTransport(signer, options.mutate)
    const store = yield* memoryStore(options.active ?? Option.none())
    const alerts = yield* alertRecorder()
    const config: CsrConfig = {
      commonName: CSR_COMMON_NAME,
      keyAlgorithm: CSR_KEY_ALGORITHM,
      sans: CSR_SANS,
      renewalWindowMs: CSR_RENEWAL_WINDOW_MS,
      issuerPublicKeySpki: signer.issuer.publicKeySpki,
      rootSubject: "root-ca",
      ...options.config,
    }
    const clients = csrClientLayer(config).pipe(
      Layer.provide(Layer.mergeAll(transport.layer, store.layer, alerts.layer)),
    )
    return { clients, transport, store, alerts, config }
  })

export const installed = (
  notAfter: string,
  spki = "MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAE",
): InstalledCertificate => ({
  leaf: {
    subject: CSR_COMMON_NAME,
    issuer: "fake-ca",
    spki,
    notBefore: new Date(Date.parse(notAfter) - 90 * DAY).toISOString(),
    notAfter,
  },
  chain: [],
  installedAt: new Date(0).toISOString(),
})

export const runWith = <A, E>(
  harnessValue: Harness,
  now: number,
  effect: Effect.Effect<A, E, CsrClient>,
): Promise<A> =>
  Effect.runPromise(
    effect.pipe(
      Effect.provide(harnessValue.clients),
      Effect.provideService(Clock.Clock, clockAt(now)),
      Effect.orDie,
    ),
  )

export const requestSignatureValid = (
  request: CsrRequest,
): Effect.Effect<boolean> =>
  verifyText(
    request.publicKeySpki,
    request.signature,
    canonicalize(
      Schema.decodeUnknownSync(Schema.Json)({
        version: request.version,
        commonName: request.commonName,
        keyAlgorithm: request.keyAlgorithm,
        sans: request.sans,
        publicKeySpki: request.publicKeySpki,
      }),
    ),
  ).pipe(Effect.orDie)
