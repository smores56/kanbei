import { Data } from "effect"

export type MontyFailureReason =
  | "capability_denied"
  | "timeout"
  | "memory_exhausted"
  | "program_error"
  | "engine_crash"
  | "engine_unavailable"
  | "session_budget_exceeded"
  | "limits_invalid"
  | "stub_invalid"

/** A classified Monty outcome: no failure is ever reported as success. */
export class MontyFailure extends Data.TaggedError("MontyFailure")<{
  readonly reason: MontyFailureReason
  readonly detail: string
  readonly exceptionType?: string
}> {}
