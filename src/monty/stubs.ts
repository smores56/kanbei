import { Effect } from "effect"
import type { MontyFailure } from "./errors.ts"

export interface HostStub {
  readonly name: string
  readonly run: (args: ReadonlyArray<unknown>) => Effect.Effect<unknown, MontyFailure>
}

const STUB_NAME = /^[a-z_][a-z0-9_]*$/u

export const isValidStubName = (name: string): boolean => STUB_NAME.test(name)

/** A capability the agent may call by name; nothing else is reachable. */
export const stub = (
  name: string,
  run: HostStub["run"],
): HostStub => ({ name, run })
