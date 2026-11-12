import { assert, assertEquals } from "@std/assert"
import { Effect, Layer } from "effect"
import { bootConfigFromEnv, channelLayers, kernelLayers, startChannels } from "../../src/main.ts"
import type { BootConfig } from "../../src/main.ts"
import {
  ChannelHost,
  HttpListenError,
  HttpServer,
  startChannelHost,
} from "../../src/channels/host.ts"
import { denoHttpServerLayer } from "../../src/adapters/http-listener.ts"
import type { ChannelHost as ChannelHostShape, HttpHandler } from "../../src/channels/host.ts"
import { withTempDir } from "../support.ts"

const TOKEN = "k".repeat(48)

const env = (values: Record<string, string>) => (name: string) => values[name]

const configFor = async (directory: string): Promise<BootConfig> => {
  await Deno.mkdir(`${directory}/state`, { recursive: true })
  await Deno.writeTextFile(`${directory}/relay-token`, `${TOKEN}\n`)
  return Effect.runPromise(
    bootConfigFromEnv(env({
      KANBEI_MANIFEST: `${directory}/manifest.json`,
      KANBEI_POSTURE: `${directory}/posture.json`,
      KANBEI_STATE_DIR: `${directory}/state`,
      KANBEI_INBOX_DIR: `${directory}/inbox`,
      KANBEI_IDENTITY: `${directory}/identity.txt`,
      KANBEI_TODO_DIR: `${directory}/todo`,
      KANBEI_CHAT_LISTEN: "127.0.0.1:0",
      KANBEI_STATUS_LISTEN: "127.0.0.1:0",
      KANBEI_CSR_SOCKET: `${directory}/csr.sock`,
      KANBEI_SEARCH_SOCKET: `${directory}/search.sock`,
      KANBEI_RELAY_TOKEN: `${directory}/relay-token`,
      KANBEI_CSR_STATE_DIR: `${directory}/tls`,
      KANBEI_SEARCH_BUDGET: `${directory}/search-budget.json`,
      KANBEI_POINTERS: `${directory}/pointers`,
      KANBEI_CHAT_ORIGIN: "https://kanbei.sammohr.dev",
      KANBEI_EPOCH: "epoch-1",
      KANBEI_ORIGINS: "tuta-mail,git",
    })),
  )
}

interface Served {
  readonly target: import("../../src/channels/host.ts").ListenTarget
  readonly handler: HttpHandler
}

/** A listener that records handlers instead of binding, like the Deno adapter. */
const recordingServer = (refuseAt?: number) => {
  const served: Array<Served> = []
  const layer = Layer.succeed(HttpServer)({
    serve: (target, handler) =>
      served.length === refuseAt
        ? Effect.fail(
          new HttpListenError({
            target: `${target.host}:${target.port}`,
            reason: "address already in use",
          }),
        )
        : Effect.sync(() => {
          served.push({ target, handler })
          return { target, stop: Effect.void }
        }),
  })
  return { served, layer }
}

const request = (url: string, init: RequestInit = {}): Request => new Request(url, init)

const bodyOf = async (response: Response): Promise<unknown> => await response.json()

Deno.test("the composition root serves the chat surface and health/status", async () => {
  await withTempDir("startup", async (directory) => {
    const config = await configFor(directory)
    const server = recordingServer()
    const kernel = kernelLayers(config)
    const channels = channelLayers(config).pipe(Layer.provide(kernel))
    const running = await Effect.runPromise(
      startChannels(config).pipe(
        Effect.provide(Layer.mergeAll(kernel, channels, server.layer)),
      ),
    )
    assertEquals(server.served.length, 2)
    const chat = server.served[0]?.handler
    const status = server.served[1]?.handler
    assert(chat !== undefined && status !== undefined)

    const health = await Effect.runPromise(
      chat(request("http://127.0.0.1/api/health")),
    )
    assertEquals(health.status, 200)
    assertEquals(await bodyOf(health), { ok: true })

    const unauthenticated = await Effect.runPromise(
      chat(
        request("http://127.0.0.1/api/chat", {
          method: "POST",
          headers: { "content-type": "application/json" },
          body: JSON.stringify({ message: "hello" }),
        }),
      ),
    )
    assertEquals(unauthenticated.status, 401)

    const live = await Effect.runPromise(status(request("http://127.0.0.1/healthz")))
    assertEquals(live.status, 200)
    assertEquals(await bodyOf(live), { ok: true })

    const detail = await Effect.runPromise(status(request("http://127.0.0.1/status")))
    assertEquals(detail.status, 200)
    const payload = await bodyOf(detail) as {
      readonly ok: boolean
      readonly sources: ReadonlyArray<{ origin: string; state: string }>
      readonly channels: ReadonlyArray<string>
    }
    assertEquals(payload.ok, true)
    // Both allowlisted sources are present and unknown, never current.
    assertEquals(
      payload.sources.map((source) => `${source.origin}:${source.state}`),
      ["git:unknown", "tuta-mail:unknown"],
    )
    assert(payload.channels.includes("chat"))
    await Effect.runPromise(running.stop)
  })
})

Deno.test("a listener that cannot bind fails the boot closed", async () => {
  await withTempDir("startup-refuse", async (directory) => {
    const config = await configFor(directory)
    // Refuse the second (status) bind: chat must not be left serving alone.
    const server = recordingServer(1)
    const kernel = kernelLayers(config)
    const channels = channelLayers(config).pipe(Layer.provide(kernel))
    const refused = await Effect.runPromise(
      Effect.flip(
        startChannels(config).pipe(
          Effect.provide(Layer.mergeAll(kernel, channels, server.layer)),
        ),
      ),
    )
    assertEquals(refused.variable, "KANBEI_STATUS_START")
  })
})

Deno.test("a channel that cannot answer its readiness check fails the boot closed", async () => {
  const host: ChannelHostShape = {
    chat: {
      handle: () => Effect.succeed({ status: 200, headers: {}, body: new Uint8Array() }),
    },
    csr: {
      state: Effect.die(new Error("certificate store unreadable")),
      due: Effect.succeed(false),
      renew: Effect.die(new Error("unused")),
      reload: Effect.die(new Error("unused")),
      receipts: Effect.succeed([]),
      alerts: Effect.succeed([]),
    },
    search: { query: () => Effect.die(new Error("unused")), receipts: Effect.succeed([]) },
    todo: {
      read: Effect.die(new Error("unused")),
      write: () => Effect.die(new Error("unused")),
      receipts: Effect.succeed([]),
    },
    candidates: {
      candidates: Effect.succeed([]),
      stage: () => Effect.die(new Error("unused")),
      validate: () => Effect.die(new Error("unused")),
      requestApproval: () => Effect.die(new Error("unused")),
      approve: () => Effect.die(new Error("unused")),
      activate: () => Effect.die(new Error("unused")),
      rollback: () => Effect.die(new Error("unused")),
      candidate: () => Effect.die(new Error("unused")),
      pointer: () => Effect.die(new Error("unused")),
    },
    status: {
      snapshot: Effect.succeed({
        ok: true,
        sources: [],
        inbox: {
          observations: 0,
          receipts: 0,
          heartbeats: 0,
          gaps: 0,
          quarantined: 0,
          duplicates: 0,
        },
        candidates: 0,
        channels: ["chat", "csr", "search", "todo", "inbox", "status"],
      }),
    },
  }
  const server = recordingServer()
  const refused = await Effect.runPromise(
    Effect.flip(
      startChannelHost({
        chat: { host: "127.0.0.1", port: 0 },
        status: { host: "127.0.0.1", port: 1 },
      }).pipe(
        Effect.provide(
          Layer.mergeAll(Layer.succeed(ChannelHost)(host), server.layer),
        ),
      ),
    ),
  )
  assertEquals(refused.channel, "csr")
})

Deno.test("the Deno listener binds loopback and answers its handler", async () => {
  const running = await Effect.runPromise(
    Effect.gen(function* () {
      const server = yield* HttpServer
      return yield* server.serve(
        { host: "127.0.0.1", port: 0 },
        () => Effect.succeed(new Response("ok")),
      )
    }).pipe(Effect.provide(denoHttpServerLayer)),
  )
  assert(running.target.port > 0)
  const response = await fetch(`http://127.0.0.1:${running.target.port}/healthz`)
  assertEquals(response.status, 200)
  assertEquals(await response.text(), "ok")
  await Effect.runPromise(running.stop)
})
