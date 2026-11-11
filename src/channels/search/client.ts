import { Clock, Context, Data, Effect, Layer, Option, Ref, Result, Schema, Semaphore } from "effect"
import { canonicalize } from "../../evidence/canonical.ts"
import { sha256Text } from "../../evidence/digest.ts"
import type { SearchBlockReason, SearchHit, SearchQuery, SearchReply } from "./schema.ts"
import {
  parseSearchReply,
  SEARCH_MAX_QUERY_CHARS,
  SEARCH_MAX_RESULTS,
  SEARCH_MAX_SNIPPET_CHARS,
} from "./schema.ts"
import { capText, stripMarkup } from "./text.ts"
import { SearchBudgetStore } from "./budget.ts"

export interface SearchConfig {
  readonly maxQueryChars: number
  readonly maxLimit: number
  readonly maxTextChars: number
  readonly queriesPerHour: number
  readonly queriesPerDay: number
}

export class SearchTransportError extends Data.TaggedError("SearchTransportError")<{
  readonly reason: string
}> {}

/** The Search VM seam: one capped request in, stripped text or a verdict back. */
export interface SearchChannel {
  readonly search: (query: SearchQuery) => Effect.Effect<SearchReply, SearchTransportError>
}

export const SearchChannel = Context.Service<SearchChannel>("kanbei/channel/search/Channel")

export interface SearchObservation {
  readonly queryDigest: string
  readonly title: string
  readonly locator: string
  readonly text: string
  readonly rank: number
}

export interface SearchReceipt {
  readonly queryDigest: string
  readonly queryChars: number
  readonly verdict: "hits" | "blocked"
  readonly reason: Option.Option<SearchBlockReason>
  readonly resultDigest: string
  readonly hits: number
  readonly at: string
}

export interface SearchOutcome {
  readonly verdict: "hits" | "blocked"
  readonly hits: ReadonlyArray<SearchObservation>
  readonly reason?: SearchBlockReason
  readonly receipt: SearchReceipt
}

export interface SearchClient {
  readonly query: (input: SearchQuery) => Effect.Effect<SearchOutcome>
  readonly receipts: Effect.Effect<ReadonlyArray<SearchReceipt>>
}

export const SearchClient = Context.Service<SearchClient>("kanbei/channel/search/Client")

const HOUR_MS = 60 * 60 * 1000
const DAY_MS = 24 * HOUR_MS

const configIssues = (config: SearchConfig): ReadonlyArray<string> => {
  const issues: Array<string> = []
  for (
    const [name, value] of Object.entries({
      maxQueryChars: config.maxQueryChars,
      maxLimit: config.maxLimit,
      maxTextChars: config.maxTextChars,
      queriesPerHour: config.queriesPerHour,
      queriesPerDay: config.queriesPerDay,
    })
  ) {
    if (!Number.isInteger(value) || value <= 0) {
      issues.push(`${name} must be a positive integer`)
    }
  }
  // The deployed searchd caps are the authority; a wider local config would
  // frame requests the host refuses.
  if (config.maxQueryChars > SEARCH_MAX_QUERY_CHARS) {
    issues.push(`maxQueryChars exceeds the host cap `)
  }
  if (config.maxLimit > SEARCH_MAX_RESULTS) {
    issues.push(`maxLimit exceeds the host cap `)
  }
  if (config.maxTextChars > SEARCH_MAX_SNIPPET_CHARS) {
    issues.push(`maxTextChars exceeds the host snippet cap `)
  }
  return issues
}

const chars = (value: string): number => [...value].length

const digestOf = (observations: ReadonlyArray<SearchObservation>): Effect.Effect<string> =>
  sha256Text(
    canonicalize(Schema.decodeUnknownSync(Schema.Json)(observations)),
  ).pipe(Effect.orDie)

const parseReply = (reply: SearchReply): Result.Result<SearchReply, string> => {
  try {
    return Result.succeed(parseSearchReply(reply))
  } catch (error) {
    return Result.fail(error instanceof Error ? error.message.slice(0, 200) : String(error))
  }
}

const shapeHits = (
  config: SearchConfig,
  queryDigest: string,
  hits: ReadonlyArray<SearchHit>,
  limit: number,
): ReadonlyArray<SearchObservation> =>
  hits.slice(0, limit).flatMap((hit, index) => {
    const text = capText(stripMarkup(hit.text), config.maxTextChars)
    if (text === "") return []
    return [{
      queryDigest,
      title: capText(stripMarkup(hit.title), config.maxTextChars),
      locator: hit.locator,
      text,
      rank: hit.rank ?? index,
    }]
  })

export const searchClientLayer = (
  config: SearchConfig,
): Layer.Layer<SearchClient, never, SearchChannel | SearchBudgetStore> =>
  Layer.effect(
    SearchClient,
    Effect.gen(function* () {
      const issues = configIssues(config)
      if (issues.length > 0) {
        return yield* Effect.die(new Error(`invalid search config: ${issues.join("; ")}`))
      }
      const channel = yield* SearchChannel
      const budget = yield* SearchBudgetStore
      const receipts = yield* Ref.make<ReadonlyArray<SearchReceipt>>([])
      // One admit at a time: a concurrent burst must not read the same window twice.
      const admit = yield* Semaphore.make(1)

      const blocked = (
        query: SearchQuery,
        queryDigest: string,
        reason: SearchBlockReason,
        at: number,
      ): Effect.Effect<SearchOutcome> =>
        Effect.gen(function* () {
          const receipt: SearchReceipt = {
            queryDigest,
            queryChars: chars(query.query),
            verdict: "blocked",
            reason: Option.some(reason),
            resultDigest: yield* digestOf([]),
            hits: 0,
            at: new Date(at).toISOString(),
          }
          yield* Ref.update(receipts, (current) => [...current, receipt])
          return { verdict: "blocked" as const, hits: [], reason, receipt }
        })

      const query: SearchClient["query"] = (input) =>
        Effect.gen(function* () {
          const at = yield* Clock.currentTimeMillis
          const queryDigest = yield* sha256Text(input.query).pipe(Effect.orDie)
          if (chars(input.query) > config.maxQueryChars) {
            return yield* blocked(input, queryDigest, "schema", at)
          }
          if (!Number.isInteger(input.limit) || input.limit < 1 || input.limit > config.maxLimit) {
            return yield* blocked(input, queryDigest, "schema", at)
          }
          const verdict = yield* admit.withPermits(1)(
            Effect.gen(function* () {
              const stored = yield* Effect.result(budget.load)
              if (Result.isFailure(stored)) return "error" as const
              const retained = stored.success.filter((stamp) => at - stamp < DAY_MS)
              const hourly = retained.filter((stamp) => at - stamp < HOUR_MS)
              if (hourly.length >= config.queriesPerHour) return "rate" as const
              if (retained.length >= config.queriesPerDay) return "budget" as const
              const saved = yield* Effect.result(budget.save([...retained, at]))
              return Result.isFailure(saved) ? "error" as const : "admitted" as const
            }),
          )
          if (verdict !== "admitted") {
            return yield* blocked(input, queryDigest, verdict, at)
          }

          const reply = yield* channel.search({ query: input.query, limit: input.limit }).pipe(
            Effect.result,
          )
          if (Result.isFailure(reply)) {
            return yield* blocked(input, queryDigest, "error", at)
          }
          const parsed = parseReply(reply.success)
          if (Result.isFailure(parsed)) {
            return yield* blocked(input, queryDigest, "schema", at)
          }
          if (parsed.success.verdict === "blocked") {
            return yield* blocked(input, queryDigest, parsed.success.reason, at)
          }
          const hits = shapeHits(config, queryDigest, parsed.success.hits, input.limit)
          const resultDigest = yield* digestOf(hits)
          const receipt: SearchReceipt = {
            queryDigest,
            queryChars: chars(input.query),
            verdict: "hits",
            reason: Option.none(),
            resultDigest,
            hits: hits.length,
            at: new Date(at).toISOString(),
          }
          yield* Ref.update(receipts, (current) => [...current, receipt])
          return { verdict: "hits" as const, hits, receipt }
        })

      return { query, receipts: Ref.get(receipts) }
    }),
  )
