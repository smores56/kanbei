import { Schema } from "effect"

export const SearchBlockReason = Schema.Literals([
  "dlp",
  "judge",
  "budget",
  "rate",
  "schema",
  "error",
])
export type SearchBlockReason = typeof SearchBlockReason.Type

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
  Schema.decodeUnknownSync(SearchReply, { onExcessProperty: "error" })(value)
