import { assertEquals } from "@std/assert"
import { Effect, Option, Ref } from "effect"
import { CsrClient } from "../../../src/channels/csr/client.ts"
import type {
  CsrError,
  CsrOutcome,
  CsrReceipt,
  CsrState,
} from "../../../src/channels/csr/client.ts"
import { generateSessionKey, isP256Spki } from "../../../src/channels/csr/key.ts"
import {
  CSR_COMMON_NAME,
  CSR_KEY_ALGORITHM,
  CSR_SANS,
  csrFieldsIssue,
  parseCsrRequest,
} from "../../../src/channels/csr/schema.ts"
import type { HostResponse } from "../../../src/channels/csr/schema.ts"
import { harness, installed, requestSignatureValid, runWith } from "./support.ts"
import type { Harness } from "./support.ts"

const NOW = Date.parse("2026-11-11T00:00:00.000Z")
const DAY = 24 * 60 * 60 * 1000

interface RenewRun {
  readonly outcome: CsrOutcome
  readonly receipts: ReadonlyArray<CsrReceipt>
  readonly state: CsrState
}

const runRenew = (h: Harness): Promise<RenewRun> =>
  runWith(
    h,
    NOW,
    Effect.gen(function* () {
      const client = yield* CsrClient
      const outcome = yield* client.renew
      return {
        outcome,
        receipts: yield* client.receipts,
        state: yield* client.state,
      }
    }),
  )

interface RefusalRun {
  readonly failure: CsrError
  readonly receipts: ReadonlyArray<CsrReceipt>
  readonly state: CsrState
}

const runRefusal = (h: Harness): Promise<RefusalRun> =>
  runWith(
    h,
    NOW,
    Effect.gen(function* () {
      const client = yield* CsrClient
      const failure = yield* Effect.flip(client.renew)
      return {
        failure,
        receipts: yield* client.receipts,
        state: yield* client.state,
      }
    }),
  )

const runDue = (h: Harness): Promise<boolean> =>
  runWith(
    h,
    NOW,
    Effect.gen(function* () {
      const client = yield* CsrClient
      return yield* client.due
    }),
  )

const callsOf = (h: Harness): Promise<ReadonlyArray<unknown>> =>
  Effect.runPromise(Ref.get(h.transport.calls))

Deno.test("a due renewal emits the fixed CSR and stages without promoting", async () => {
  const h = await Effect.runPromise(harness({ now: NOW }))
  const { outcome, receipts, state } = await runRenew(h)
  assertEquals(state, "issued")
  assertEquals(outcome.renewed, true)
  assertEquals(Option.isNone(outcome.active), true)
  assertEquals(Option.isSome(outcome.staged), true)

  const calls = await callsOf(h)
  assertEquals(calls.length, 1)
  const request = parseCsrRequest(calls[0])
  assertEquals(request.commonName, CSR_COMMON_NAME)
  assertEquals(request.keyAlgorithm, CSR_KEY_ALGORITHM)
  assertEquals(request.sans, [CSR_COMMON_NAME])
  assertEquals(await Effect.runPromise(requestSignatureValid(request)), true)
  assertEquals(await Effect.runPromise(isP256Spki(request.publicKeySpki)), true)

  assertEquals(receipts.map((receipt) => `${receipt.kind}:${receipt.outcome}`), [
    "renewal_request:ok",
  ])
  assertEquals(receipts[0]?.sans, CSR_SANS)
})

Deno.test("a different CN, an extra SAN, and a different key algorithm refuse before the CA", async () => {
  const cases = [
    { commonName: "evil.example" },
    { sans: [CSR_COMMON_NAME, "evil.example"] },
    { keyAlgorithm: "rsa-2048" },
  ]
  const outcomes = await Promise.all(
    cases.map(async (override) => {
      const h = await Effect.runPromise(harness({ now: NOW, config: override }))
      const { failure, receipts, state } = await runRefusal(h)
      return {
        failure,
        lastReceipt: receipts.at(-1)?.kind,
        calls: (await callsOf(h)).length,
        state,
      }
    }),
  )
  for (const outcome of outcomes) {
    assertEquals(outcome.failure.code, "csr_fields_refused")
    assertEquals(outcome.calls, 0)
    assertEquals(outcome.lastReceipt, "renewal_refused")
    assertEquals(outcome.state, "refused")
  }
})

Deno.test("a host leaf with a foreign SPKI is refused and nothing is staged", async () => {
  const foreign = await Effect.runPromise(generateSessionKey())
  const h = await Effect.runPromise(
    harness({
      now: NOW,
      mutate: (response: HostResponse) => ({
        ...response,
        leaf: { ...response.leaf, spki: foreign.publicKeySpki },
      }),
    }),
  )
  const { failure, receipts } = await runRefusal(h)
  assertEquals(failure.code, "csr_response_refused")
  assertEquals(Option.isNone(await Effect.runPromise(Ref.get(h.store.staged))), true)
  assertEquals(receipts.at(-1)?.outcome, "refused")
})

Deno.test("a bad leaf proof is refused", async () => {
  const h = await Effect.runPromise(
    harness({ now: NOW, mutate: (response) => ({ ...response, proof: "AAAA" }) }),
  )
  assertEquals((await runRefusal(h)).failure.code, "csr_response_refused")
})

Deno.test("a broken or unrouted chain is refused", async () => {
  const empty = await Effect.runPromise(
    harness({ now: NOW, mutate: (response) => ({ ...response, chain: [] }) }),
  )
  assertEquals((await runRefusal(empty)).failure.code, "csr_response_refused")

  const wrongRoot = await Effect.runPromise(
    harness({
      now: NOW,
      mutate: (response) => ({
        ...response,
        chain: response.chain.map((entry) => ({ ...entry, issuer: "other-root" })),
      }),
    }),
  )
  assertEquals((await runRefusal(wrongRoot)).failure.code, "csr_response_refused")
})

Deno.test("the old certificate stays active until a reload completes", async () => {
  const old = installed(new Date(NOW + 29 * DAY).toISOString())
  const h = await Effect.runPromise(harness({ now: NOW, active: Option.some(old) }))
  const result = await runWith(
    h,
    NOW,
    Effect.gen(function* () {
      const client = yield* CsrClient
      const renewed = yield* client.renew
      const reloaded = yield* client.reload
      return {
        before: Option.getOrThrow(renewed.active),
        active: reloaded.active,
        receipts: yield* client.receipts,
        state: yield* client.state,
      }
    }),
  )
  assertEquals(result.before, old)
  const active = Option.getOrThrow(result.active)
  assertEquals(active.leaf.notAfter, new Date(NOW + 90 * DAY).toISOString())
  assertEquals(result.receipts.map((receipt) => receipt.kind), ["renewal_request", "reload"])
  assertEquals(result.state, "issued")
})

Deno.test("the renewal window boundary is inclusive at 30 days", async () => {
  const boundary = await Effect.runPromise(
    harness({ now: NOW, active: Option.some(installed(new Date(NOW + 30 * DAY).toISOString())) }),
  )
  assertEquals(await runDue(boundary), true)
  assertEquals((await runRenew(boundary)).state, "issued")

  const outside = await Effect.runPromise(
    harness({
      now: NOW,
      active: Option.some(installed(new Date(NOW + 30 * DAY + 1).toISOString())),
    }),
  )
  assertEquals(await runDue(outside), false)
  assertEquals((await runRefusal(outside)).failure.code, "csr_not_due")
})

Deno.test("an expired certificate raises a host alert before renewal", async () => {
  const expired = installed(new Date(NOW - DAY).toISOString())
  const h = await Effect.runPromise(harness({ now: NOW, active: Option.some(expired) }))
  const result = await runWith(
    h,
    NOW,
    Effect.gen(function* () {
      const client = yield* CsrClient
      yield* client.renew
      return yield* client.alerts
    }),
  )
  assertEquals(result.map((alert) => alert.kind), ["certificate_expired"])
  assertEquals(result[0]?.notAfter, Option.some(expired.leaf.notAfter))
})

Deno.test("each renewal generates a fresh P-256 session key", async () => {
  const first = await Effect.runPromise(harness({ now: NOW }))
  const second = await Effect.runPromise(harness({ now: NOW }))
  await runRenew(first)
  await runRenew(second)
  const [a] = await callsOf(first)
  const [b] = await callsOf(second)
  assertEquals(
    parseCsrRequest(a).publicKeySpki === parseCsrRequest(b).publicKeySpki,
    false,
  )
})

Deno.test("the request fixture matches the fixed contract", () => {
  const fixture = JSON.parse(Deno.readTextFileSync("fixtures/csr-v1/request.json")) as {
    request: unknown
    renewalWindowMs: number
  }
  const request = parseCsrRequest(fixture.request)
  assertEquals(request.version, 1)
  assertEquals(request.commonName, CSR_COMMON_NAME)
  assertEquals(csrFieldsIssue(request), undefined)
  assertEquals(fixture.renewalWindowMs, 30 * DAY)
})
