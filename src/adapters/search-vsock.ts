import { Effect, Layer, Result } from "effect"
import { SearchChannel, SearchTransportError } from "../channels/search/client.ts"
import type { SearchQuery, SearchReply } from "../channels/search/schema.ts"
import {
  encodeSearchWireRequest,
  parseSearchWireReply,
  SEARCH_MAX_ID_CHARS,
  SEARCH_MAX_RESPONSE_BYTES,
  SEARCH_MAX_RESULTS,
  SEARCH_MAX_SNIPPET_CHARS,
  SEARCH_MAX_TITLE_CHARS,
  SEARCH_MAX_URL_CHARS,
  toSearchReply,
} from "../channels/search/schema.ts"

/** The deployed relay's first line; searchd refuses anything else. */
export const AUTH_PREFIX = "KANBEI-AUTH 1 "
const TOKEN_PATTERN = /^[A-Za-z0-9._-]{32,128}$/u

export interface SearchVsockOptions {
  readonly path: string
  readonly token: string
  readonly maxResponseBytes?: number
  readonly id?: string
}

const encoder = new TextEncoder()
const decoder = new TextDecoder()

const error = (reason: string): SearchTransportError => new SearchTransportError({ reason })

const writeAll = async (
  connection: Deno.Conn,
  bytes: Uint8Array,
  offset = 0,
): Promise<void> => {
  if (offset >= bytes.length) return
  const written = await connection.write(bytes.subarray(offset))
  if (written === 0) throw new Error("short write")
  return writeAll(connection, bytes, offset + written)
}

const readLine = async (
  connection: Deno.Conn,
  maxBytes: number,
  buffer = new Uint8Array(maxBytes),
  offset = 0,
): Promise<string> => {
  if (offset >= maxBytes) throw new Error(`reply exceeds ${maxBytes} bytes`)
  const read = await connection.read(buffer.subarray(offset))
  if (read === null) throw new Error("search VM closed before a reply")
  const end = offset + read
  const newline = buffer.subarray(0, end).indexOf(0x0a)
  if (newline !== -1) return decoder.decode(buffer.subarray(0, newline))
  return readLine(connection, maxBytes, buffer, end)
}

const fieldIssues = (wire: ReturnType<typeof parseSearchWireReply>): ReadonlyArray<string> => {
  const issues: Array<string> = []
  if (wire.results.length > SEARCH_MAX_RESULTS) {
    issues.push(`${wire.results.length} results > ${SEARCH_MAX_RESULTS}`)
  }
  if (wire.id !== undefined && wire.id.length > SEARCH_MAX_ID_CHARS) {
    issues.push("reply id is too long")
  }
  for (const result of wire.results) {
    if (
      result.title.length > SEARCH_MAX_TITLE_CHARS ||
      result.snippet.length > SEARCH_MAX_SNIPPET_CHARS ||
      result.url.length > SEARCH_MAX_URL_CHARS
    ) {
      issues.push("reply result exceeds a host field cap")
    }
  }
  return issues
}

const exchangeOnce = async (
  options: SearchVsockOptions,
  query: SearchQuery,
  maxResponseBytes: number,
): Promise<SearchReply> => {
  if (!TOKEN_PATTERN.test(options.token)) {
    throw new Error("search token is not 32..128 URL-safe characters")
  }
  const connection = await Deno.connect({ transport: "unix", path: options.path })
  try {
    await writeAll(connection, encoder.encode(`${AUTH_PREFIX}${options.token}\n`))
    const request = encodeSearchWireRequest({
      v: 1,
      op: "search",
      query: query.query,
      ...(options.id === undefined ? {} : { id: options.id }),
    })
    await writeAll(connection, encoder.encode(`${request}\n`))
    const line = await readLine(connection, maxResponseBytes)
    const parsed = Result.try({
      try: () => parseSearchWireReply(JSON.parse(line)),
      catch: (failure) =>
        failure instanceof Error ? failure.message.slice(0, 200) : String(failure),
    })
    if (Result.isFailure(parsed)) throw new Error(`malformed search reply: ${parsed.failure}`)
    const issues = fieldIssues(parsed.success)
    if (issues.length > 0) throw new Error(issues.join("; "))
    return toSearchReply(parsed.success)
  } finally {
    connection.close()
  }
}

/** The only egress from Kanbei: one authenticated, capped query line to the Search VM. */
export const vsockSearchChannelLayer = (
  options: SearchVsockOptions,
): Layer.Layer<SearchChannel> =>
  Layer.succeed(SearchChannel)({
    search: (query) =>
      Effect.tryPromise({
        try: () =>
          exchangeOnce(options, query, options.maxResponseBytes ?? SEARCH_MAX_RESPONSE_BYTES),
        catch: (failure) => error(failure instanceof Error ? failure.message : String(failure)),
      }),
  })
