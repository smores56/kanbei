import { Data } from "effect"

export class CommandError extends Data.TaggedError("CommandError")<{
  readonly argv: ReadonlyArray<string>
  readonly reason: string
}> {}

export class ArmError extends Data.TaggedError("ArmError")<{
  readonly arm: string
  readonly reason: string
}> {}

export class ProbeOutputError extends Data.TaggedError("ProbeOutputError")<{
  readonly variant: string
  readonly reason: string
}> {}

export class ResultError extends Data.TaggedError("ResultError")<{
  readonly reason: string
}> {}
