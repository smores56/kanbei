import { assertEquals, assertThrows } from "@std/assert"
import { Effect } from "effect"
import { SearchChannel } from "../../../src/channels/search/client.ts"
import { AUTH_PREFIX, vsockSearchChannelLayer } from "../../../src/adapters/search-vsock.ts"
import { parseSearchWireReply, toSearchReply } from "../../../src/channels/search/schema.ts"
import { withTempDir } from "../../support.ts"

const TOKEN = "search-token-0123456789abcdef-0123456789"

const readLines = async (
  connection: Deno.Conn,
  count: number,
  buffer = new Uint8Array(4096),
  text = "",
): Promise<string> => {
  const read = await connection.read(buffer)
  const next = read === null ? text : text + new TextDecoder().decode(buffer.subarray(0, read))
  return read === null || next.split("\n").length >= count
    ? next
    : readLines(connection, count, buffer, next)
}

const reply = (overrides: Record<string, unknown> = {}) => ({
  v: 1,
  op: "search",
  decision: "allow",
  reason: "ok",
  results: [{ title: "T", url: "https://example.invalid/a", snippet: "S" }],
  truncated: false,
  ...overrides,
})

Deno.test("the host reply maps url/snippet into locator/text and keeps blocks empty", () => {
  const allowed = toSearchReply(parseSearchWireReply(reply()))
  assertEquals(allowed, {
    verdict: "allowed",
    hits: [{ title: "T", locator: "https://example.invalid/a", text: "S" }],
  })
  const blocked = toSearchReply(
    parseSearchWireReply(reply({ decision: "block", reason: "dlp", results: [] })),
  )
  assertEquals(blocked, { verdict: "blocked", reason: "dlp" })
  const unknown = toSearchReply(
    parseSearchWireReply(reply({ decision: "block", reason: "novel", results: [] })),
  )
  assertEquals(unknown.verdict === "blocked" ? unknown.reason : "allowed", "error")
})

Deno.test("a reply with an extra field or another op is refused", () => {
  assertThrows(() => parseSearchWireReply(reply({ extra: true })))
  assertThrows(() => parseSearchWireReply(reply({ op: "other" })))
})

Deno.test("the adapter sends the auth line, one JSON request line, and maps the reply", async () => {
  await withTempDir("search-wire", async (directory) => {
    const path = `${directory}/search.sock`
    const listener = Deno.listen({ transport: "unix", path })
    const seen: Array<string> = []
    const server = (async () => {
      const connection = await listener.accept()
      const text = await readLines(connection, 3)
      seen.push(...text.split("\n").filter((line) => line !== ""))
      await connection.write(new TextEncoder().encode(`${JSON.stringify(reply())}\n`))
      connection.close()
      listener.close()
    })()

    const outcome = await Effect.runPromise(
      Effect.gen(function* () {
        const channel = yield* SearchChannel
        return yield* channel.search({ query: "weather", limit: 3 })
      }).pipe(Effect.provide(vsockSearchChannelLayer({ path, token: TOKEN }))),
    )
    await server
    assertEquals(seen[0], `${AUTH_PREFIX}${TOKEN}`)
    assertEquals(JSON.parse(seen[1] ?? ""), { v: 1, op: "search", query: "weather" })
    assertEquals(outcome.verdict, "allowed")
  })
})

Deno.test("an oversized reply line fails closed as a transport error", async () => {
  await withTempDir("search-wire", async (directory) => {
    const path = `${directory}/search.sock`
    const listener = Deno.listen({ transport: "unix", path })
    const server = (async () => {
      const connection = await listener.accept()
      await readLines(connection, 3)
      await connection.write(new TextEncoder().encode(`${"x".repeat(64)}\n`))
      connection.close()
      listener.close()
    })()

    const failure = await Effect.runPromise(
      Effect.gen(function* () {
        const channel = yield* SearchChannel
        return yield* Effect.flip(channel.search({ query: "weather", limit: 3 }))
      }).pipe(
        Effect.provide(vsockSearchChannelLayer({ path, token: TOKEN, maxResponseBytes: 32 })),
      ),
    )
    await server
    assertEquals(failure.reason.includes("exceeds 32"), true)
  })
})
