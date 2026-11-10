import { Data } from "effect"

/** The probe could not be executed on this host; it is evidence of absence, never a pass. */
export class ProbeUnavailable extends Data.TaggedError("ProbeUnavailable")<{
  readonly reason: string
  readonly detail?: string
}> {}

export class CommandError extends Data.TaggedError("CommandError")<{
  readonly program: string
  readonly reason: string
}> {}

/** A capability the probe needs was never established, so the dependent effect is denied. */
export class CapabilityDenied extends Data.TaggedError("CapabilityDenied")<{
  readonly capability: string
  readonly reason: string
}> {}
