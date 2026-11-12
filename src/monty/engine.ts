import { Context, Effect } from "effect"
import type { Scope } from "effect"
import type { MontyFailure } from "./errors.ts"

export interface SessionLimits {
  readonly maxMemoryBytes: number
  readonly maxFeedMs: number
}

/** One effectful host call the engine may expose to a program. */
export type EngineLookup = Readonly<
  Record<string, (args: ReadonlyArray<unknown>) => Effect.Effect<unknown, MontyFailure>>
>

export interface EngineSession {
  readonly run: (
    code: string,
    lookup: EngineLookup,
  ) => Effect.Effect<unknown, MontyFailure>
  readonly close: Effect.Effect<void>
}

/**
 * The narrow engine seam. The WASM adapter is the only implementation; the
 * domain sees effectful calls and classified failures, never a session object.
 */
export interface MontyEngine {
  readonly checkout: (
    limits: SessionLimits,
  ) => Effect.Effect<EngineSession, MontyFailure, Scope.Scope>
}

export const MontyEngine = Context.Service<MontyEngine>("kanbei/monty/MontyEngine")
