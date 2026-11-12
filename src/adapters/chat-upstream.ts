import { Effect, Layer } from "effect"
import { ChatUpstream, ChatUpstreamError } from "../channels/chat/http.ts"

const REPLY_MAX_CHARS = 32 * 1024

const decodeReply = (value: unknown): string | undefined => {
  if (typeof value !== "object" || value === null) return undefined
  const choices = (value as { readonly choices?: unknown }).choices
  if (!Array.isArray(choices) || choices.length === 0) return undefined
  const first: unknown = choices[0]
  if (typeof first !== "object" || first === null) return undefined
  const message = (first as { readonly message?: unknown }).message
  if (typeof message !== "object" || message === null) return undefined
  const content = (message as { readonly content?: unknown }).content
  return typeof content === "string" ? content.slice(0, REPLY_MAX_CHARS) : undefined
}

/**
 * The local llama.cpp lane (`nix-config modules/lib/kanbei-inference.nix`,
 * loopback `:8090`). No remote model, no fallback provider: a failed lane is an
 * error the chat surface turns into a 502, never a silent stub.
 */
export const chatUpstreamLayer = (endpoint: string): Layer.Layer<ChatUpstream> =>
  Layer.succeed(ChatUpstream)({
    reply: (message) =>
      Effect.tryPromise({
        try: async () => {
          const response = await fetch(endpoint, {
            method: "POST",
            headers: { "content-type": "application/json" },
            body: JSON.stringify({
              model: "kanbei",
              stream: false,
              messages: [{ role: "user", content: message }],
            }),
            signal: AbortSignal.timeout(300_000),
          })
          if (!response.ok) throw new Error(`lane answered ${response.status}`)
          const reply = decodeReply(await response.json())
          if (reply === undefined) throw new Error("lane reply has no message content")
          return reply
        },
        catch: (error) =>
          new ChatUpstreamError({
            reason: error instanceof Error ? error.message : String(error),
          }),
      }),
  })
