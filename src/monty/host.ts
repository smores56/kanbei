import { Context, Effect, Layer, Ref } from "effect"
import { Scope } from "effect"
import type { MontyEngine, SessionLimits } from "./engine.ts"
import { MontyEngine as MontyEngineTag } from "./engine.ts"
import { MontyFailure } from "./errors.ts"
import { isValidStubName } from "./stubs.ts"
import type { HostStub } from "./stubs.ts"

export interface ProcessLimits {
  readonly maxSessions: number
  readonly maxWorkers: number
  readonly totalMemoryBytes: number
  readonly perSessionMemoryBytes: number
  readonly perSessionFeedMs: number
  readonly checkoutTimeoutMs: number
}

export type HostCallOutcome = "ok" | "failed" | "denied"

export interface HostCallReceipt {
  readonly agentId: string
  readonly stub: string
  readonly outcome: HostCallOutcome
}

export interface OpenRequest {
  readonly agentId: string
  readonly stubs: ReadonlyArray<HostStub>
}

export interface MontySession {
  readonly agentId: string
  readonly run: (code: string) => Effect.Effect<unknown, MontyFailure>
}

export interface MontyHost {
  readonly open: (request: OpenRequest) => Effect.Effect<MontySession, MontyFailure, Scope.Scope>
  readonly receipts: Effect.Effect<ReadonlyArray<HostCallReceipt>>
}

export const MontyHost = Context.Service<MontyHost>("kanbei/monty/MontyHost")

const positive = (value: number): boolean => Number.isFinite(value) && value > 0

const limitsIssues = (limits: ProcessLimits): ReadonlyArray<string> => {
  const issues: Array<string> = []
  if (!Number.isInteger(limits.maxSessions) || limits.maxSessions < 1) {
    issues.push("maxSessions must be a positive integer")
  }
  if (!Number.isInteger(limits.maxWorkers) || limits.maxWorkers < 1) {
    issues.push("maxWorkers must be a positive integer")
  }
  if (!positive(limits.totalMemoryBytes) || !positive(limits.perSessionMemoryBytes)) {
    issues.push("memory limits must be positive")
  }
  if (limits.perSessionMemoryBytes * limits.maxSessions > limits.totalMemoryBytes) {
    issues.push("per-session memory times maxSessions exceeds the process budget")
  }
  if (!positive(limits.perSessionFeedMs) || !positive(limits.checkoutTimeoutMs)) {
    issues.push("duration limits must be positive")
  }
  return issues
}

const sessionLimits = (limits: ProcessLimits): SessionLimits => ({
  maxMemoryBytes: limits.perSessionMemoryBytes,
  maxFeedMs: limits.perSessionFeedMs,
})

const NAME_ERROR = /name '([^']+)' is not defined/u

const unknownName = (failure: MontyFailure): string | undefined => {
  if (failure.exceptionType !== "NameError") return undefined
  return NAME_ERROR.exec(failure.detail)?.[1]
}

const receipt = (
  agentId: string,
  name: string,
  outcome: HostCallOutcome,
): HostCallReceipt => ({ agentId, stub: name, outcome })

/**
 * One session per loop execution attempt. Host functions are closures over the
 * agent's own stubs, so a program can only reach the capabilities in its
 * manifest; unknown names never resolve to another agent's closure.
 */
export const montyHostLayer = (
  limits: ProcessLimits,
): Layer.Layer<MontyHost, never, MontyEngine> =>
  Layer.effect(
    MontyHost,
    Effect.gen(function* () {
      const issues = limitsIssues(limits)
      if (issues.length > 0) {
        return yield* Effect.die(new Error(`invalid Monty process limits: ${issues.join("; ")}`))
      }
      const engine = yield* MontyEngineTag
      const active = yield* Ref.make(0)
      const receipts = yield* Ref.make<ReadonlyArray<HostCallReceipt>>([])
      const record = (entry: HostCallReceipt): Effect.Effect<void> =>
        Ref.update(receipts, (current) => [...current, entry])

      const open: MontyHost["open"] = (request) =>
        Effect.gen(function* () {
          const names = request.stubs.map((entry) => entry.name)
          const invalid = names.filter((name) => !isValidStubName(name))
          if (invalid.length > 0) {
            return yield* Effect.fail(
              new MontyFailure({
                reason: "stub_invalid",
                detail: `invalid stub names: ${invalid.join(",")}`,
              }),
            )
          }
          if (new Set(names).size !== names.length) {
            return yield* Effect.fail(
              new MontyFailure({ reason: "stub_invalid", detail: "duplicate stub name" }),
            )
          }
          yield* Effect.acquireRelease(
            Effect.gen(function* () {
              const admitted = yield* Ref.modify(
                active,
                (count) =>
                  count >= limits.maxSessions
                    ? [false, count] as const
                    : [true, count + 1] as const,
              )
              if (!admitted) {
                return yield* Effect.fail(
                  new MontyFailure({
                    reason: "session_budget_exceeded",
                    detail: `all ${limits.maxSessions} sessions are open`,
                  }),
                )
              }
              return true
            }),
            () => Ref.update(active, (count) => count - 1),
          )
          const engineSession = yield* Effect.acquireRelease(
            engine.checkout(sessionLimits(limits)),
            (session) => session.close,
          )
          const declared = new Set(names)
          const lookup = Object.fromEntries(
            request.stubs.map((entry) => [
              entry.name,
              (args: ReadonlyArray<unknown>) =>
                entry.run(args).pipe(
                  Effect.tap(() => record(receipt(request.agentId, entry.name, "ok"))),
                  Effect.tapError(() => record(receipt(request.agentId, entry.name, "failed"))),
                ),
            ]),
          )
          const run = (code: string): Effect.Effect<unknown, MontyFailure> =>
            engineSession.run(code, lookup).pipe(
              Effect.tapError((failure) =>
                Effect.gen(function* () {
                  const name = unknownName(failure)
                  if (name !== undefined && !declared.has(name)) {
                    yield* record(receipt(request.agentId, name, "denied"))
                  }
                })
              ),
            )
          return { agentId: request.agentId, run }
        })

      return { open, receipts: Ref.get(receipts) }
    }),
  )
