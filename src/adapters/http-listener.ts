import { Effect, Layer } from "effect"
import { HttpListenError, HttpServer } from "../channels/host.ts"
import type { ListenTarget } from "../channels/host.ts"

const targetName = (target: ListenTarget): string => `${target.host}:${target.port}`

/**
 * The process-level HTTP listener. Binding happens synchronously, so `AddrInUse`
 * and a non-loopback host refuse the boot instead of leaving a half-served VM.
 */
export const denoHttpServerLayer: Layer.Layer<HttpServer> = Layer.succeed(HttpServer)({
  serve: (target, handler) =>
    Effect.try({
      try: () => {
        const abort = new AbortController()
        const server = Deno.serve({
          hostname: target.host,
          port: target.port,
          signal: abort.signal,
        }, (request) => Effect.runPromise(handler(request)))
        const bound: ListenTarget = {
          host: target.host,
          port: "port" in server.addr ? server.addr.port : target.port,
        }
        return {
          target: bound,
          stop: Effect.sync(() => {
            abort.abort()
            server.shutdown()
          }),
        }
      },
      catch: (error) =>
        new HttpListenError({
          target: targetName(target),
          reason: error instanceof Error ? error.message : String(error),
        }),
    }),
})
