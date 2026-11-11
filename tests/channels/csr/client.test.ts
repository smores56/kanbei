import { assertEquals } from "@std/assert"
import { Clock, Effect, Option, Ref } from "effect"
import { CsrClient, CsrTransportError } from "../../../src/channels/csr/client.ts"
import type { CsrReceipt } from "../../../src/channels/csr/client.ts"
import {
  certificateFor,
  defaultResponder,
  harness,
  installed,
  issuedPair,
  movableCsrClock,
  runWith,
} from "./support.ts"
import type { Harness } from "./support.ts"
import { parseCsrPem } from "../../../src/channels/csr/pkcs10.ts"

const NOW = Date.parse("2026-11-11T00:00:00.000Z")
const DAY = 24 * 60 * 60 * 1000

const runRenew = (h: Harness) =>
  runWith(
    h,
    NOW,
    Effect.gen(function* () {
      const client = yield* CsrClient
      const outcome = yield* client.renew
      return { outcome, receipts: yield* client.receipts, state: yield* client.state }
    }),
  )

const runRefusal = (h: Harness) =>
  runWith(
    h,
    NOW,
    Effect.gen(function* () {
      const client = yield* CsrClient
      const failure = yield* Effect.flip(client.renew)
      return { failure, receipts: yield* client.receipts, state: yield* client.state }
    }),
  )

Deno.test("a due renewal emits a host-parsable PEM CSR and promotes it", async () => {
  const h = await Effect.runPromise(
    harness((csr) => Effect.succeed(defaultResponder(csr, NOW, 90 * DAY))),
  )
  const result = await runRenew(h)
  assertEquals(result.outcome.renewed, true)
  assertEquals(result.state, "issued")
  const active = Option.getOrThrow(result.outcome.active)
  assertEquals(Option.isNone(result.outcome.staged), true)
  assertEquals(active.notAfter, "2027-02-09T00:00:00Z")
  assertEquals(active.chainPem.includes("BEGIN CERTIFICATE"), true)
  const calls = await Effect.runPromise(Ref.get(h.transport.calls))
  const parsed = parseCsrPem(calls[0] ?? "", "kanbei.sammohr.dev")
  assertEquals(parsed.keyType, "ec")
  assertEquals(parsed.keyBits, 256)
  assertEquals(result.receipts.map((receipt: CsrReceipt) => receipt.kind), ["renewal_request"])
})

Deno.test("a certificate outside the renewal window is not re-requested", async () => {
  const h = await Effect.runPromise(
    harness(
      (csr) => Effect.succeed(defaultResponder(csr, NOW, 90 * DAY)),
      Option.some(
        installed(new Date(NOW + 60 * DAY).toISOString()),
      ),
    ),
  )
  const result = await runRefusal(h)
  assertEquals(result.failure.code, "csr_not_due")
  assertEquals((await Effect.runPromise(Ref.get(h.transport.calls))).length, 0)
})

Deno.test("an expired active certificate raises an alert and is replaced", async () => {
  const h = await Effect.runPromise(
    harness(
      (csr) => Effect.succeed(defaultResponder(csr, NOW, 90 * DAY)),
      Option.some(
        installed(new Date(NOW - DAY).toISOString()),
      ),
    ),
  )
  const result = await runRenew(h)
  assertEquals(result.state, "issued")
  const alerts = await Effect.runPromise(Ref.get(h.alerts))
  assertEquals(alerts[0]?.kind, "certificate_expired")
})

Deno.test("a chain carrying another public key is refused", async () => {
  const other = certificateFor(
    "kanbei.sammohr.dev",
    Uint8Array.from([0x30, 0x00]),
    NOW - DAY,
    NOW + 90 * DAY,
  )
  const h = await Effect.runPromise(
    harness(() =>
      Effect.succeed({
        chain: other,
        notAfter: new Date(NOW + 90 * DAY).toISOString().replace(/\.\d{3}Z$/u, "Z"),
      })
    ),
  )
  const result = await runRefusal(h)
  assertEquals(result.failure.code, "csr_response_refused")
  assertEquals(result.state, "refused")
  assertEquals(result.receipts.at(-1)?.outcome, "refused")
})

Deno.test("a chain with the wrong CN is refused", async () => {
  const h = await Effect.runPromise(
    harness((csr) => Effect.succeed(defaultResponder(csr, NOW, 90 * DAY, "evil.example"))),
  )
  const result = await runRefusal(h)
  assertEquals(result.failure.code, "csr_response_refused")
})

Deno.test("a transport failure is receipted and refuses the renewal", async () => {
  const h = await Effect.runPromise(
    harness(() => Effect.fail(new CsrTransportError({ reason: "down" }))),
  )
  const result = await runRefusal(h)
  assertEquals(result.failure.code, "csr_transport_failed")
  assertEquals(result.state, "refused")
})

Deno.test("reload validates and promotes a staged certificate", async () => {
  const pair = await Effect.runPromise(issuedPair(NOW))
  const h = await Effect.runPromise(
    harness(() => Effect.die("reload must not touch the CA"), Option.none(), {
      issuerCommonName: "kanbei-test-root",
    }),
  )
  await Effect.runPromise(Ref.set(h.store.staged, Option.some(pair)))
  const reloaded = await runWith(
    h,
    NOW,
    Effect.gen(function* () {
      const client = yield* CsrClient
      return yield* client.reload
    }),
  )
  assertEquals(Option.isSome(reloaded.active), true)
  assertEquals(Option.isNone(reloaded.staged), true)
})

Deno.test("a fabricated success without a chain refuses", async () => {
  const h: Harness = await Effect.runPromise(
    harness(() => Effect.succeed({ chain: "", notAfter: "" })),
  )
  const result = await runRefusal(h)
  assertEquals(result.failure.code, "csr_response_refused")
})

Deno.test("an agent-triggered renewal is rate-limited and backs off after a failure", async () => {
  const clock = movableCsrClock(NOW)
  const h = await Effect.runPromise(
    harness(
      () => Effect.fail(new CsrTransportError({ reason: "CA down" })),
      Option.none(),
      { minRenewalIntervalMs: 60_000, maxBackoffMs: 10 * 60_000 },
    ),
  )
  const outcome = await Effect.runPromise(
    Effect.gen(function* () {
      const client = yield* CsrClient
      const first = yield* Effect.flip(client.renew)
      const second = yield* Effect.flip(client.renew)
      clock.advance(61_000)
      const third = yield* Effect.flip(client.renew)
      clock.advance(20 * 60_000)
      const fourth = yield* Effect.flip(client.renew)
      return { first, second, third, fourth }
    }).pipe(Effect.provide(h.layer), Effect.provideService(Clock.Clock, clock.clock)),
  )
  assertEquals(outcome.first.code, "csr_transport_failed")
  assertEquals(outcome.second.code, "csr_backoff")
  assertEquals(outcome.third.code, "csr_backoff")
  assertEquals(outcome.fourth.code, "csr_transport_failed")
})

Deno.test("reload refuses a staged certificate whose key does not match", async () => {
  const pair = await Effect.runPromise(issuedPair(NOW))
  const foreign = await Effect.runPromise(issuedPair(NOW))
  const h = await Effect.runPromise(
    harness(() => Effect.die("reload must not touch the CA"), Option.none(), {
      issuerCommonName: "kanbei-test-root",
    }),
  )
  await Effect.runPromise(
    Ref.set(h.store.staged, Option.some({ ...pair, privateKeyPkcs8: foreign.privateKeyPkcs8 })),
  )
  const error = await runWith(
    h,
    NOW,
    Effect.gen(function* () {
      const client = yield* CsrClient
      return yield* Effect.flip(client.reload)
    }),
  )
  assertEquals(error.code, "csr_reload_failed")
  assertEquals(Option.isNone(await Effect.runPromise(Ref.get(h.store.active))), true)
})

Deno.test("reload refuses an expired or wrongly issued staged certificate", async () => {
  const expired = await Effect.runPromise(issuedPair(NOW - 100 * DAY))
  const h = await Effect.runPromise(
    harness(() => Effect.die("reload must not touch the CA"), Option.none(), {
      issuerCommonName: "kanbei-test-root",
    }),
  )
  await Effect.runPromise(Ref.set(h.store.staged, Option.some(expired)))
  const expiry = await runWith(
    h,
    NOW,
    Effect.gen(function* () {
      const client = yield* CsrClient
      return yield* Effect.flip(client.reload)
    }),
  )
  assertEquals(expiry.code, "csr_reload_failed")
  assertEquals(expiry.detail.includes("expired") || expiry.detail.includes("notAfter"), true)

  const pair = await Effect.runPromise(issuedPair(NOW))
  const strict = await Effect.runPromise(
    harness(() => Effect.die("reload must not touch the CA"), Option.none(), {
      issuerCommonName: "another-intermediate",
    }),
  )
  await Effect.runPromise(Ref.set(strict.store.staged, Option.some(pair)))
  const issuer = await runWith(
    strict,
    NOW,
    Effect.gen(function* () {
      const client = yield* CsrClient
      return yield* Effect.flip(client.reload)
    }),
  )
  assertEquals(issuer.code, "csr_reload_failed")
  assertEquals(issuer.detail.includes("issuer"), true)
})

Deno.test("a second renewal inside the floor is rate-limited", async () => {
  const clock = movableCsrClock(NOW)
  const h = await Effect.runPromise(
    harness((csr) => Effect.succeed(defaultResponder(csr, NOW, 90 * DAY)), Option.none(), {
      minRenewalIntervalMs: 60_000,
    }),
  )
  const outcome = await Effect.runPromise(
    Effect.gen(function* () {
      const client = yield* CsrClient
      yield* client.renew
      const second = yield* Effect.flip(client.renew)
      clock.advance(61_000)
      const third = yield* client.renew.pipe(Effect.flip)
      return { second, third }
    }).pipe(Effect.provide(h.layer), Effect.provideService(Clock.Clock, clock.clock)),
  )
  assertEquals(outcome.second.code, "csr_rate_limited")
  // Past the floor the attempt is allowed through, then refused as not due.
  assertEquals(outcome.third.code, "csr_not_due")
})
