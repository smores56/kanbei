import { Effect, Layer } from "effect"
import { Monty, MontyCrashedError, MontyRuntimeError } from "@pydantic/monty/wasm"
import type { EngineLookup, MontyEngine, SessionLimits } from "./engine.ts"
import { MontyEngine as MontyEngineTag } from "./engine.ts"
import { MontyFailure } from "./errors.ts"
import type { ProcessLimits } from "./host.ts"

const classify = (error: unknown): MontyFailure => {
  if (error instanceof MontyCrashedError) {
    return new MontyFailure({
      reason: error.timedOut ? "timeout" : "engine_crash",
      detail: error.message,
    })
  }
  if (error instanceof MontyRuntimeError) {
    const exceptionType = error.exception?.typeName
    const detail = error.exception?.message ?? error.message
    if (exceptionType === "PermissionError" || exceptionType === "ModuleNotFoundError") {
      return new MontyFailure({ reason: "capability_denied", detail, exceptionType })
    }
    if (exceptionType === "TimeoutError") {
      return new MontyFailure({ reason: "timeout", detail, exceptionType })
    }
    if (exceptionType === "MemoryError") {
      return new MontyFailure({ reason: "memory_exhausted", detail, exceptionType })
    }
    return new MontyFailure({
      reason: "program_error",
      detail,
      ...(exceptionType === undefined ? {} : { exceptionType }),
    })
  }
  return new MontyFailure({
    reason: "engine_unavailable",
    detail: error instanceof Error ? error.message : String(error),
  })
}

const asLookup = (lookup: EngineLookup): Record<string, (...args: unknown[]) => unknown> =>
  Object.fromEntries(
    Object.entries(lookup).map(([name, run]) => [
      name,
      (...args: unknown[]) => Effect.runPromise(run(args)),
    ]),
  )

const checkoutLimits = (limits: SessionLimits) => ({
  limits: {
    maxMemory: limits.maxMemoryBytes,
    maxFeedDurationSecs: limits.maxFeedMs / 1_000,
  },
})

/**
 * The WASM backend runs in Deno worker threads inside this one process. No
 * OS-per-agent isolation exists here; the VM is the security boundary (S-13).
 */
export const wasmEngineLayer = (
  limits: ProcessLimits,
): Layer.Layer<MontyEngine, MontyFailure> =>
  Layer.effect(
    MontyEngineTag,
    Effect.acquireRelease(
      Effect.tryPromise({
        try: () =>
          Monty.create({
            maxProcesses: limits.maxWorkers,
            checkoutTimeout: limits.checkoutTimeoutMs / 1_000,
            requestTimeout: Math.max(30, limits.perSessionFeedMs / 1_000 + 30),
          }),
        catch: classify,
      }),
      (pool) =>
        Effect.tryPromise(() => pool.close()).pipe(
          Effect.catch(() => Effect.void),
        ),
    ).pipe(
      Effect.map((pool): MontyEngine => ({
        checkout: (sessionLimits) =>
          Effect.gen(function* () {
            const raw = yield* Effect.tryPromise({
              try: () => pool.checkout(checkoutLimits(sessionLimits)),
              catch: classify,
            })
            const session = yield* Effect.acquireRelease(
              Effect.succeed(raw),
              (value) =>
                Effect.tryPromise(() => value.close()).pipe(
                  Effect.catch(() => Effect.void),
                ),
            )
            const run = (code: string, lookup: EngineLookup) =>
              Effect.tryPromise({
                try: () =>
                  session.feedRun(code, {
                    externalLookup: asLookup(lookup),
                    printCallback: () => {},
                  }),
                catch: classify,
              })
            return { run, close: Effect.void }
          }),
      })),
    ),
  )
