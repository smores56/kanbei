import { Schema } from "effect"

/**
 * The deployed Search VM contract (`nix-config modules/lib/search/searchd.py`).
 * One connection carries a `KANBEI-AUTH 1 <token>` line, then one JSON request
 * line `{v,op:"search",query,id?}`; the reply is one JSON line. The constants
 * below mirror that file and are the only values this side may assume.
 */
export const SEARCH_PROTOCOL_VERSION = 1
export const SEARCH_MAX_LINE = 8192
export const SEARCH_MAX_RESPONSE_BYTES = 8192
export const SEARCH_MAX_QUERY_CHARS = 320
export const SEARCH_MAX_RESULTS = 5
export const SEARCH_MAX_TITLE_CHARS = 200
export const SEARCH_MAX_SNIPPET_CHARS = 400
export const SEARCH_MAX_URL_CHARS = 300
export const SEARCH_MAX_ID_CHARS = 64

export const SearchBlockReason = Schema.Literals([
  "dlp",
  "judge",
  "budget",
  "rate",
  "schema",
  "error",
  "request",
  "state",
  "length",
  "upstream",
  "internal",
])
export type SearchBlockReason = typeof SearchBlockReason.Type

const strict = { onExcessProperty: "error" } as const

/** The exact request line searchd accepts; nothing else is on the wire. */
export const SearchWireRequest = Schema.Struct({
  v: Schema.Literal(SEARCH_PROTOCOL_VERSION),
  op: Schema.Literal("search"),
  query: Schema.String,
  id: Schema.optional(Schema.String),
})
export type SearchWireRequest = typeof SearchWireRequest.Type

export const SearchWireResult = Schema.Struct({
  title: Schema.String,
  url: Schema.String,
  snippet: Schema.String,
})
export type SearchWireResult = typeof SearchWireResult.Type

export const SearchWireReply = Schema.Struct({
  v: Schema.Literal(SEARCH_PROTOCOL_VERSION),
  op: Schema.Literal("search"),
  decision: Schema.Literals(["allow", "block"]),
  reason: Schema.String,
  results: Schema.Array(SearchWireResult),
  truncated: Schema.Boolean,
  id: Schema.optional(Schema.String),
})
export type SearchWireReply = typeof SearchWireReply.Type

export const parseSearchWireReply = (value: unknown): SearchWireReply =>
  Schema.decodeUnknownSync(SearchWireReply, strict)(value)

export const encodeSearchWireRequest = (request: SearchWireRequest): string =>
  JSON.stringify(Schema.encodeSync(SearchWireRequest)(request))

/** The whole request shape: no URL, provider, key, or shaping field exists. */
export const SearchQuery = Schema.Struct({
  query: Schema.String,
  limit: Schema.Number,
})
export type SearchQuery = typeof SearchQuery.Type

export const SearchHit = Schema.Struct({
  title: Schema.String,
  locator: Schema.String,
  text: Schema.String,
  rank: Schema.optional(Schema.Number),
})
export type SearchHit = typeof SearchHit.Type

export const SearchReply = Schema.Union([
  Schema.Struct({
    verdict: Schema.Literal("allowed"),
    hits: Schema.Array(SearchHit),
  }),
  Schema.Struct({
    verdict: Schema.Literal("blocked"),
    reason: SearchBlockReason,
    detail: Schema.optional(Schema.String),
  }),
])
export type SearchReply = typeof SearchReply.Type

export const parseSearchReply = (value: unknown): SearchReply =>
  Schema.decodeUnknownSync(SearchReply, strict)(value)

const HOST_REASONS: ReadonlySet<string> = new Set([
  "dlp",
  "judge",
  "budget",
  "rate",
  "schema",
  "request",
  "state",
  "length",
  "upstream",
  "internal",
])

/** Unknown host reasons fail closed to `error`, never to an allow. */
export const blockReasonOf = (reason: string): SearchBlockReason =>
  HOST_REASONS.has(reason) ? reason as SearchBlockReason : "error"

/**
 * Map the host reply onto the kernel shape. A blocked reply carries no text, so
 * the caller cannot accidentally forward a partial result set.
 */
export const toSearchReply = (wire: SearchWireReply): SearchReply =>
  wire.decision === "allow"
    ? {
      verdict: "allowed",
      hits: wire.results.map((result) => ({
        title: result.title,
        locator: result.url,
        text: result.snippet,
      })),
    }
    : { verdict: "blocked", reason: blockReasonOf(wire.reason) }
