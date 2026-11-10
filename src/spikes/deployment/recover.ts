import { Effect, Result } from "effect"

/**
 * Recovers from any typed error with a fallback effect of the same success type.
 * Effect v4 has no `catchAll`; `result` keeps the failure typed and never
 * swallows defects, so a defect still surfaces instead of becoming a fallback.
 */
export const recover =
  <E, A, R2>(handler: (error: E) => Effect.Effect<A, never, R2>) =>
  <R>(self: Effect.Effect<A, E, R>): Effect.Effect<A, never, R | R2> =>
    Effect.flatMap(
      Effect.result(self),
      (result): Effect.Effect<A, never, R | R2> =>
        Result.isSuccess(result) ? Effect.succeed(result.success) : handler(result.failure),
    )
