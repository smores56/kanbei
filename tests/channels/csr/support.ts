import { Clock, Effect, Layer, Option, Ref } from "effect"
import type { CsrAlert, CsrConfig, CsrTransportReply } from "../../../src/channels/csr/client.ts"
import {
  AlertSink,
  CSR_RENEWAL_WINDOW_MS,
  CsrClient,
  csrClientLayer,
  CsrTransport,
  CsrTransportError,
} from "../../../src/channels/csr/client.ts"
import { CertificateStore } from "../../../src/channels/csr/store.ts"
import type { InstalledCertificate } from "../../../src/channels/csr/store.ts"
import {
  encodeDnsName,
  encodeIntegerValue,
  encodeOid,
  encodeSequence,
  encodeSet,
  encodeTlv,
  encodeUtf8,
  pemEncode,
} from "../../../src/channels/csr/der.ts"
import { CSR_COMMON_NAME } from "../../../src/channels/csr/schema.ts"
import { exportPkcs8, generateSessionKey } from "../../../src/channels/csr/key.ts"
import type { CsrKeyError } from "../../../src/channels/csr/key.ts"
import { parseCsrPem } from "../../../src/channels/csr/pkcs10.ts"

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

const utcTime = (millis: number): Uint8Array => {
  const text = new Date(millis).toISOString().replace(/[-:T]/gu, "").slice(2, 14)
  return encodeTlv(0x17, new TextEncoder().encode(`${text}Z`))
}

const name = (cn: string): Uint8Array =>
  encodeSequence(encodeSet(encodeSequence(encodeOid("2.5.4.3"), encodeUtf8(cn))))

/** A minimal but structurally valid leaf: the client checks names, SPKI, and time. */
export const certificateFor = (
  cn: string,
  spkiDer: Uint8Array,
  notBefore: number,
  notAfter: number,
): string => {
  const san = encodeSequence(
    encodeOid("2.5.29.17"),
    encodeTlv(0x04, encodeSequence(encodeDnsName(cn))),
  )
  const extensions = encodeTlv(0xa3, encodeSequence(san))
  const signatureAlgorithm = encodeSequence(encodeOid("1.2.840.10045.4.3.2"))
  const tbs = encodeSequence(
    encodeIntegerValue(1),
    signatureAlgorithm,
    name("kanbei-test-root"),
    encodeSequence(utcTime(notBefore), utcTime(notAfter)),
    name(cn),
    spkiDer,
    extensions,
  )
  const der = encodeSequence(
    tbs,
    signatureAlgorithm,
    encodeTlv(0x03, new Uint8Array(65)),
  )
  return pemEncode("CERTIFICATE", der)
}

export interface FakeTransport {
  readonly layer: Layer.Layer<CsrTransport>
  readonly calls: Ref.Ref<ReadonlyArray<string>>
}

export const fakeTransport = (
  responder: (csrPem: string) => Effect.Effect<CsrTransportReply, CsrTransportError>,
): Effect.Effect<FakeTransport> =>
  Effect.gen(function* () {
    const calls = yield* Ref.make<ReadonlyArray<string>>([])
    const layer = Layer.succeed(CsrTransport)({
      exchange: (csrPem) =>
        Ref.update(calls, (current) => [...current, csrPem]).pipe(
          Effect.flatMap(() => responder(csrPem)),
        ),
    })
    return { layer, calls }
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

export const installed = (notAfter: string): InstalledCertificate => ({
  chainPem: "-----BEGIN CERTIFICATE-----\nAA==\n-----END CERTIFICATE-----\n",
  leafPem: "-----BEGIN CERTIFICATE-----\nAA==\n-----END CERTIFICATE-----\n",
  privateKeyPkcs8: "AA==",
  spki: "AA==",
  notAfter,
  installedAt: new Date(Date.parse(notAfter) - 60 * DAY).toISOString(),
})

export const config = (overrides: Partial<CsrConfig> = {}): CsrConfig => ({
  commonName: CSR_COMMON_NAME,
  renewalWindowMs: CSR_RENEWAL_WINDOW_MS,
  ...overrides,
})

export const defaultResponder = (
  csrPem: string,
  now: number,
  validityMs: number,
  cn = CSR_COMMON_NAME,
): CsrTransportReply => {
  const parsed = parseCsrPem(csrPem, CSR_COMMON_NAME)
  const leaf = certificateFor(cn, parsed.spkiDer, now - DAY, now + validityMs)
  return {
    chain: leaf,
    notAfter: new Date(now + validityMs).toISOString().replace(/\.\d{3}Z$/u, "Z"),
  }
}

export interface Harness {
  readonly layer: Layer.Layer<CsrClient>
  readonly store: MemoryStore
  readonly transport: FakeTransport
  readonly alerts: Ref.Ref<ReadonlyArray<CsrAlert>>
}

export const harness = (
  responder: (csrPem: string) => Effect.Effect<CsrTransportReply, CsrTransportError>,
  initial: Option.Option<InstalledCertificate> = Option.none(),
  overrides: Partial<CsrConfig> = {},
): Effect.Effect<Harness> =>
  Effect.gen(function* () {
    const transport = yield* fakeTransport(responder)
    const store = yield* memoryStore(initial)
    const alerts = yield* Ref.make<ReadonlyArray<CsrAlert>>([])
    const sink = Layer.succeed(AlertSink)({
      emit: (alert) => Ref.update(alerts, (current) => [...current, alert]),
    })
    const layer = csrClientLayer(config(overrides)).pipe(
      Layer.provide(Layer.mergeAll(transport.layer, store.layer, sink)),
    )
    return { layer, store, transport, alerts }
  })

export const runWith = <A, E>(
  h: Harness,
  now: number,
  effect: Effect.Effect<A, E, CsrClient>,
): Promise<A> =>
  Effect.runPromise(
    effect.pipe(
      Effect.provide(h.layer),
      Effect.provideService(Clock.Clock, clockAt(now)),
    ),
  )

/** A real key/certificate pair, for staging tests that must pass validation. */
export const issuedPair = (
  now: number,
  validityMs = 90 * DAY,
): Effect.Effect<InstalledCertificate, CsrKeyError> =>
  Effect.gen(function* () {
    const key = yield* generateSessionKey()
    const leaf = certificateFor(
      CSR_COMMON_NAME,
      key.publicKeySpkiDer,
      now - DAY,
      now + validityMs,
    )
    const privateKeyPkcs8 = yield* exportPkcs8(key)
    return {
      chainPem: leaf,
      leafPem: leaf,
      privateKeyPkcs8,
      spki: key.publicKeySpki,
      notAfter: new Date(now + validityMs).toISOString().replace(/\.\d{3}Z$/u, "Z"),
      installedAt: new Date(now).toISOString(),
    }
  })

export interface MovableCsrClock {
  readonly clock: Clock.Clock
  readonly advance: (millis: number) => void
}

export const movableCsrClock = (start: number): MovableCsrClock => {
  let current = start
  return {
    advance: (millis) => {
      current += millis
    },
    clock: {
      currentTimeMillisUnsafe: () => current,
      currentTimeMillis: Effect.sync(() => current),
      currentTimeNanosUnsafe: () => BigInt(current) * 1_000_000n,
      currentTimeNanos: Effect.sync(() => BigInt(current) * 1_000_000n),
      monotonicTimeNanosUnsafe: () => BigInt(current) * 1_000_000n,
      monotonicTimeNanos: Effect.sync(() => BigInt(current) * 1_000_000n),
      sleep: () => Effect.void,
    },
  }
}
