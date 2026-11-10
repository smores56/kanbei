import { Context, Effect, Ref } from "effect"

export interface VirtualClock {
  readonly nowMillis: Effect.Effect<number>
  readonly advanceMillis: (millis: number) => Effect.Effect<void>
  readonly setMillis: (millis: number) => Effect.Effect<void>
}

export const VirtualClock = Context.Service<VirtualClock>("kanbei/evidence/VirtualClock")

export const makeVirtualClock = (startMillis = 0): Effect.Effect<VirtualClock> =>
  Effect.gen(function* () {
    const now = yield* Ref.make(startMillis)
    return {
      nowMillis: Ref.get(now),
      advanceMillis: (millis) => Ref.update(now, (current) => current + millis),
      setMillis: (millis) => Ref.set(now, millis),
    }
  })
