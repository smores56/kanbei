import { assertEquals } from "@std/assert"
import { Effect } from "effect"
import { encodeFrame } from "../../../src/channels/chat/frame.ts"
import { ChatClient, ChatClientError, chatClientLayer } from "../../../src/channels/chat/client.ts"
import {
  IDENTITY,
  memoryStream,
  ORIGIN,
  PASSKEY,
  request,
  responseOf,
  serveAll,
  serveChain,
  serveOne,
} from "./support.ts"

const codeOf = (exchange: Awaited<ReturnType<typeof serveOne>>): string | undefined =>
  responseOf(exchange).error?.code

Deno.test("a request without the passkey is denied even on loopback", async () => {
  const exchange = await serveOne(request({ passkey: undefined }), { peer: "loopback" })
  assertEquals(codeOf(exchange), "chat_unauthenticated")
  assertEquals(exchange.receipts.map((receipt) => receipt.outcome), ["denied"])
})

Deno.test("a foreign origin is denied before the passkey is judged", async () => {
  const exchange = await serveOne(request({ origin: "https://evil.invalid" }))
  assertEquals(codeOf(exchange), "chat_origin_denied")
  assertEquals(exchange.receipts[0]?.outcome, "denied")
})

Deno.test("a wrong passkey is denied", async () => {
  const exchange = await serveOne(request({ passkey: "wrong" }))
  assertEquals(codeOf(exchange), "chat_unauthenticated")
})

Deno.test("a minted session is bound to its passkey and origin", async () => {
  const batch = await serveChain([
    () => request({ sessionId: undefined }),
    (sessionId) => request({ sessionId, passkey: "wrong" }),
    (sessionId) => request({ sessionId, origin: "https://evil.invalid" }),
    (sessionId) => request({ sessionId }),
  ])
  const [first, wrong, origin, rebound] = batch.exchanges
  assertEquals(first === undefined ? undefined : responseOf(first).ok, true)
  assertEquals(wrong === undefined ? undefined : codeOf(wrong), "chat_unauthenticated")
  assertEquals(origin === undefined ? undefined : codeOf(origin), "chat_origin_denied")
  assertEquals(rebound === undefined ? undefined : responseOf(rebound).ok, true)
})

Deno.test("an unknown route, a static request, and an upgrade deny with receipts", async () => {
  const unknown = await serveOne(request({ route: "chat.delete" }))
  assertEquals(codeOf(unknown), "chat_route_unknown")

  const staticRequest = await serveOne(request({ route: "/assets/app.js" }))
  assertEquals(codeOf(staticRequest), "chat_static_denied")

  const upgrade = await serveOne(request({ route: "chat.turn", upgrade: "websocket" }))
  assertEquals(codeOf(upgrade), "chat_upgrade_denied")

  for (const exchange of [unknown, staticRequest, upgrade]) {
    assertEquals(exchange.receipts.length, 1)
    assertEquals(exchange.receipts[0]?.outcome, "denied")
  }
})

Deno.test("payloads are schema-locked and extra fields deny", async () => {
  const extra = await serveOne(request({
    route: "chat.turn",
    payload: { turnId: "t1", text: "hi", channel: "search" },
  }))
  assertEquals(codeOf(extra), "chat_schema_invalid")

  const limit = await serveOne(request({ route: "decision.list", payload: { limit: 0 } }))
  assertEquals(codeOf(limit), "chat_schema_invalid")

  const missing = await serveOne(request({ route: "chat.turn" }))
  assertEquals(codeOf(missing), "chat_schema_invalid")
})

Deno.test("an oversized frame, turn, or response denies within the configured bound", async () => {
  const oversized = await serveOne(request(), { settings: { maxRequestBytes: 32 } })
  assertEquals(codeOf(oversized), "chat_too_large")

  const longTurn = await serveOne(
    request({ route: "chat.turn", payload: { turnId: "t1", text: "x".repeat(201) } }),
  )
  assertEquals(codeOf(longTurn), "chat_too_large")

  const smallResponse = await serveOne(request(), { settings: { maxResponseBytes: 64 } })
  assertEquals(codeOf(smallResponse), "chat_response_too_large")
})

Deno.test("chat.turn streams start, delta, end in order before the response", async () => {
  const exchange = await serveOne(
    request({ route: "chat.turn", payload: { turnId: "turn-1", text: "hello" } }),
  )
  assertEquals(
    exchange.outbound.map((frame) =>
      frame.kind === "event" ? `${frame.event}:${frame.seq}` : "response"
    ),
    ["start:0", "delta:1", "end:2", "response"],
  )
  const response = responseOf(exchange)
  assertEquals(response.ok, true)
  assertEquals(response.payload, { turnId: "turn-1", text: "reply to turn-1: hello" })
  assertEquals(exchange.receipts[0]?.outcome, "ok")
})

Deno.test("decision and status routes return their narrow shapes", async () => {
  const decisions = await serveOne(request({ route: "decision.list", payload: { limit: 2 } }))
  const list = responseOf(decisions).payload
  assertEquals(
    Array.isArray(list) ? false : typeof list === "object" && list !== null && "decisions" in list,
    true,
  )

  const link = await serveOne(
    request({ route: "decision.ruling_link", payload: { decisionId: "dec-1" } }),
  )
  const linkPayload = responseOf(link).payload
  assertEquals(
    typeof linkPayload === "object" && linkPayload !== null && "link" in linkPayload
      ? linkPayload.link
      : undefined,
    "/consent/decisions/dec-1",
  )

  const status = await serveOne(request({ route: "status" }))
  assertEquals(responseOf(status).ok, true)
})

Deno.test("a decision that does not exist is not found", async () => {
  const exchange = await serveOne(
    request({ route: "decision.ruling_link", payload: { decisionId: "dec-404" } }),
  )
  assertEquals(codeOf(exchange), "chat_not_found")
})

Deno.test("the request ceiling denies the next authenticated request", async () => {
  const batch = await serveChain([() => request(), () => request()], {
    settings: { requestsPerHour: 1 },
  })
  const [first, second] = batch.exchanges
  assertEquals(first === undefined ? undefined : responseOf(first).ok, true)
  assertEquals(second === undefined ? undefined : codeOf(second), "chat_rate_limited")
})

Deno.test("the client adapter returns ordered events and the terminal response", async () => {
  const exchange = await serveOne(
    request({ route: "chat.turn", payload: { turnId: "turn-9", text: "ping" } }),
  )
  const memory = await Effect.runPromise(memoryStream(exchange.outbound.map(encodeFrame)))
  const outcome = await Effect.runPromise(
    Effect.gen(function* () {
      const client = yield* ChatClient
      return yield* client.request(memory.stream, {
        route: "chat.turn",
        accessIdentity: IDENTITY,
        passkey: PASSKEY,
        origin: ORIGIN,
        payload: { turnId: "turn-9", text: "ping" },
      })
    }).pipe(Effect.provide(chatClientLayer(1_000_000)), Effect.orDie),
  )
  assertEquals(outcome.events.map((event) => event.event), ["start", "delta", "end"])
  assertEquals(outcome.response.ok, true)
  assertEquals(outcome.sessionId, responseOf(exchange).sessionId)
})

Deno.test("the client adapter rejects an out-of-order event stream", async () => {
  const outOfOrder = [
    { kind: "event", seq: 1, route: "chat.turn", event: "delta", text: "late" },
    { kind: "response", route: "chat.turn", ok: true, payload: { turnId: "t", text: "x" } },
  ]
  const memory = await Effect.runPromise(memoryStream(outOfOrder.map(encodeFrame)))
  const failure = await Effect.runPromise(
    Effect.gen(function* () {
      const client = yield* ChatClient
      return yield* Effect.flip(client.request(memory.stream, { route: "status" }))
    }).pipe(Effect.provide(chatClientLayer(1_000_000)), Effect.orDie),
  )
  assertEquals(failure instanceof ChatClientError, true)
  if (failure instanceof ChatClientError) {
    assertEquals(failure.code, "chat_client_out_of_order")
  }
})

Deno.test("every served request, including denials, leaves a receipt", async () => {
  const batch = await serveAll([request({ passkey: "wrong" }), request()])
  const [denied, allowed] = batch.exchanges
  assertEquals(batch.receipts.length, 2)
  assertEquals(denied?.receipts[0]?.outcome, "denied")
  assertEquals(allowed?.receipts[1]?.outcome, "ok")
  assertEquals(denied?.receipts[0]?.requestDigest.startsWith("sha256:"), true)
})

Deno.test("the fixture requests all serve as valid JSON routes", async () => {
  const fixture = JSON.parse(Deno.readTextFileSync("fixtures/chat-v1/requests.json")) as {
    requests: ReadonlyArray<{ route: string }>
  }
  const batch = await serveAll(fixture.requests.map((entry) => request(entry)))
  assertEquals(batch.receipts.length, fixture.requests.length)
  assertEquals(batch.exchanges.every((exchange) => responseOf(exchange).ok), true)
})

Deno.test("a closed stream is closed after one request", async () => {
  const exchange = await serveOne(request())
  assertEquals(exchange.closed, true)
})

Deno.test("the rate limit runs before authentication and is per peer", async () => {
  const batch = await serveAll(
    [
      request({ passkey: "wrong" }),
      request({ passkey: "wrong" }),
      request({ passkey: "wrong" }),
    ],
    { settings: { requestsPerHour: 1 }, peers: ["relay", "relay", "loopback"] },
  )
  const [first, second, other] = batch.exchanges
  assertEquals(first === undefined ? undefined : codeOf(first), "chat_unauthenticated")
  assertEquals(second === undefined ? undefined : codeOf(second), "chat_rate_limited")
  assertEquals(other === undefined ? undefined : codeOf(other), "chat_unauthenticated")
})

Deno.test("a receipt digest never covers the passkey", async () => {
  const batch = await serveAll([
    request({ passkey: PASSKEY }),
    request({ passkey: "a different guess" }),
  ])
  const [first, second] = batch.receipts
  assertEquals(first === undefined ? undefined : first.requestDigest, second?.requestDigest)
  assertEquals(batch.receipts.length, 2)
})
