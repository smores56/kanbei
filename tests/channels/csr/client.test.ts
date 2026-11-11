import { assertEquals } from "@std/assert"
import { Effect, Option, Ref } from "effect"
import { CsrClient, CsrTransportError } from "../../../src/channels/csr/client.ts"
import type { CsrReceipt } from "../../../src/channels/csr/client.ts"
import { certificateFor, defaultResponder, harness, installed, runWith } from "./support.ts"
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

Deno.test("a due renewal emits a host-parsable PEM CSR and stages without promoting", async () => {
  const h = await Effect.runPromise(
    harness((csr) => Effect.succeed(defaultResponder(csr, NOW, 90 * DAY))),
  )
  const result = await runRenew(h)
  assertEquals(result.outcome.renewed, true)
  assertEquals(result.state, "issued")
  assertEquals(Option.isNone(result.outcome.active), true)
  const staged = Option.getOrThrow(result.outcome.staged)
  assertEquals(staged.notAfter, "2027-02-09T00:00:00Z")
  assertEquals(staged.chainPem.includes("BEGIN CERTIFICATE"), true)
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

Deno.test("reload promotes the staged certificate", async () => {
  const h = await Effect.runPromise(
    harness((csr) => Effect.succeed(defaultResponder(csr, NOW, 90 * DAY))),
  )
  await runRenew(h)
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
