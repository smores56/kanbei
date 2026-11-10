import { Context, Effect, Ref } from "effect"

export interface SyntheticRequest {
  readonly method: string
  readonly url: string
  readonly headers?: Readonly<Record<string, string>>
  readonly body?: string
}

export interface SyntheticResponse {
  readonly status: number
  readonly headers: Readonly<Record<string, string>>
  readonly body: string
}

export interface SyntheticCall {
  readonly request: SyntheticRequest
  readonly response: SyntheticResponse
}

export interface SyntheticNetwork {
  readonly serve: (url: string, response: SyntheticResponse) => Effect.Effect<void>
  readonly request: (
    request: SyntheticRequest,
  ) => Effect.Effect<SyntheticResponse, SyntheticNetworkError>
  readonly calls: Effect.Effect<ReadonlyArray<SyntheticCall>>
}

export class SyntheticNetworkError extends Error {
  readonly _tag = "SyntheticNetworkError"
}

export const SyntheticNetwork = Context.Service<SyntheticNetwork>(
  "kanbei/evidence/SyntheticNetwork",
)

export const makeSyntheticNetwork = (): Effect.Effect<SyntheticNetwork> =>
  Effect.gen(function* () {
    const routes = yield* Ref.make<Readonly<Record<string, SyntheticResponse>>>({})
    const calls = yield* Ref.make<ReadonlyArray<SyntheticCall>>([])

    return {
      serve: (url, response) => Ref.update(routes, (current) => ({ ...current, [url]: response })),
      request: (request) =>
        Effect.gen(function* () {
          const table = yield* Ref.get(routes)
          const response = table[request.url]
          if (response === undefined) {
            return yield* Effect.fail(
              new SyntheticNetworkError(`no synthetic route for ${request.url}`),
            )
          }
          yield* Ref.update(calls, (current) => [...current, { request, response }])
          return response
        }),
      calls: Ref.get(calls),
    }
  })
