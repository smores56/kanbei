import { assertEquals } from "@std/assert"
import { Effect, Layer } from "effect"
import type { ChatHttpRequest, ChatHttpResponse } from "../../../src/channels/chat/http.ts"
import {
  ChatHttp,
  chatHttpLayer,
  ChatUpstream,
  defaultChatHttpConfig,
  makeChatHttp,
  makeSessionStore,
  SessionStore,
} from "../../../src/channels/chat/http.ts"
import { ChatHttpClient, httpChatClientLayer } from "../../../src/adapters/chat-http-client.ts"

const encoder = new TextEncoder()
const decoder = new TextDecoder()

interface Fixture {
  readonly service: ChatHttp["handle"]
  readonly token: () => Promise<string>
}

const fixture = (
  upstream?: (message: string) => Effect.Effect<string, { readonly reason: string }>,
): Fixture => {
  const sessions = makeSessionStore(() => Date.now())
  const service = makeChatHttp(
    { ...defaultChatHttpConfig, upstream: upstream !== undefined },
    sessions,
    (message) =>
      upstream === undefined ? Promise.resolve("stub") : Effect.runPromise(upstream(message)),
  )
  return {
    service: service.handle,
    token: () => Effect.runPromise(sessions.issue),
  }
}

interface CallInput {
  readonly method?: ChatHttpRequest["method"]
  readonly path?: string
  readonly headers?: Readonly<Record<string, string>>
  readonly body?: unknown
  readonly relayAuthenticated?: boolean
}

const call = async (
  service: ChatHttp["handle"],
  request: CallInput,
): Promise<ChatHttpResponse> => {
  const body = request.body === undefined ? new Uint8Array() : encoder.encode(
    typeof request.body === "string" ? request.body : JSON.stringify(request.body),
  )
  return await Effect.runPromise(service({
    method: request.method ?? "POST",
    path: request.path ?? "/api/chat",
    headers: request.headers ?? { "content-type": "application/json" },
    body,
    relayAuthenticated: request.relayAuthenticated ?? true,
  }))
}

const json = (response: ChatHttpResponse): Record<string, unknown> =>
  JSON.parse(decoder.decode(response.body))

Deno.test("the relay token gates /api and health answers 200", async () => {
  const { service } = fixture()
  const denied = await call(service, {
    method: "GET",
    path: "/api/health",
    relayAuthenticated: false,
  })
  assertEquals(denied.status, 401)
  const ok = await call(service, { method: "GET", path: "/api/health" })
  assertEquals(ok.status, 200)
  assertEquals(json(ok), { ok: true })
})

Deno.test("chat requires a bearer passkey session", async () => {
  const { service } = fixture()
  const response = await call(service, { body: { message: "hello" } })
  assertEquals(response.status, 401)
  assertEquals(json(response)["error"], "a passkey session is required")
})

Deno.test("content type, JSON shape, and message bounds match the host", async () => {
  const { service, token } = fixture()
  const session = await token()
  const headers = { "content-type": "application/json", authorization: `Bearer ${session}` }
  assertEquals(
    (await call(service, {
      body: { message: "hi" },
      headers: { authorization: headers.authorization },
    })).status,
    415,
  )
  assertEquals((await call(service, { body: "not json", headers })).status, 400)
  assertEquals((await call(service, { body: [], headers })).status, 400)
  assertEquals((await call(service, { body: {}, headers })).status, 400)
  assertEquals((await call(service, { body: { message: "   " }, headers })).status, 400)
  assertEquals((await call(service, { body: { message: "x".repeat(8001) }, headers })).status, 413)
  assertEquals((await call(service, { body: new Uint8Array(64 * 1024 + 1), headers })).status, 413)
})

Deno.test("a session returns the stub reply when no lane is attached", async () => {
  const { service, token } = fixture()
  const response = await call(service, {
    body: { message: "hello" },
    headers: { "content-type": "application/json", authorization: `Bearer ${await token()}` },
  })
  assertEquals(response.status, 200)
  const payload = json(response)
  assertEquals(payload["lane"], "stub")
  assertEquals(typeof payload["reply"], "string")
})

Deno.test("an attached lane answers locally and a lane failure is a 502", async () => {
  const ok = fixture((message) => Effect.succeed(`local: ${message}`))
  const response = await call(ok.service, {
    body: { message: "hello" },
    headers: { "content-type": "application/json", authorization: `Bearer ${await ok.token()}` },
  })
  assertEquals(response.status, 200)
  assertEquals(json(response)["lane"], "local")
  assertEquals(json(response)["reply"], "local: hello")

  const bad = fixture(() => Effect.fail({ reason: "down" }))
  const failed = await call(bad.service, {
    body: { message: "hello" },
    headers: { "content-type": "application/json", authorization: `Bearer ${await bad.token()}` },
  })
  assertEquals(failed.status, 502)
})

Deno.test("unknown routes are 404 and never proxied", async () => {
  const { service, token } = fixture()
  const headers = { "content-type": "application/json", authorization: `Bearer ${await token()}` }
  assertEquals((await call(service, { path: "/api/other", body: {}, headers })).status, 404)
  assertEquals((await call(service, { method: "GET", path: "/", headers })).status, 404)
})

Deno.test("the HTTP client speaks the same route, body, and error contract", async () => {
  const server = Deno.serve({ hostname: "127.0.0.1", port: 0 }, async (req) => {
    if (req.method === "GET" && new URL(req.url).pathname === "/api/health") {
      return Response.json({ ok: true })
    }
    if (req.headers.get("authorization") !== "Bearer good") {
      return Response.json({ error: "a passkey session is required" }, { status: 401 })
    }
    const body = await req.json()
    return Response.json({ reply: `echo ${body.message}`, lane: "local" })
  })
  try {
    const port = server.addr.port
    const layer = httpChatClientLayer({
      baseUrl: `http://127.0.0.1:${port}`,
      session: () => Effect.succeed("good"),
    })
    const result = await Effect.runPromise(
      Effect.gen(function* () {
        const client = yield* ChatHttpClient
        return {
          health: yield* client.health,
          chat: yield* client.chat("hello"),
        }
      }).pipe(Effect.provide(layer)),
    )
    assertEquals(result.health.ok, true)
    assertEquals(result.chat, { reply: "echo hello", lane: "local" })

    const denied = await Effect.runPromise(
      Effect.gen(function* () {
        const client = yield* ChatHttpClient
        return yield* Effect.flip(client.chat("hello"))
      }).pipe(Effect.provide(httpChatClientLayer({
        baseUrl: `http://127.0.0.1:${port}`,
        session: () => Effect.succeed("bad"),
      }))),
    )
    assertEquals(denied.status, 401)
  } finally {
    await server.shutdown()
  }
})

Deno.test("the layer-built service carries the same contract as the factory", async () => {
  const sessions = makeSessionStore(() => Date.now())
  const layer = chatHttpLayer({ ...defaultChatHttpConfig, upstream: true }).pipe(
    Layer.provide(Layer.merge(
      Layer.succeed(SessionStore)(sessions),
      Layer.succeed(ChatUpstream)({ reply: (message) => Effect.succeed(`lane ${message}`) }),
    )),
  )
  const response = await Effect.runPromise(
    Effect.gen(function* () {
      const http = yield* ChatHttp
      return yield* http.handle({
        method: "POST",
        path: "/api/chat",
        headers: {
          "content-type": "application/json",
          authorization: `Bearer ${yield* sessions.issue}`,
        },
        body: encoder.encode(JSON.stringify({ message: "hi" })),
        relayAuthenticated: true,
      })
    }).pipe(Effect.provide(layer)),
  )
  assertEquals(response.status, 200)
  assertEquals(JSON.parse(decoder.decode(response.body)), { reply: "lane hi", lane: "local" })
})
