import { Clock, Context, Data, Effect, Layer, Ref, Result } from "effect"
import type { ChannelDenial, ChannelId, ProcessIdentity, SandboxRecord } from "./identity.ts"
import { verifyCaller } from "./identity.ts"
import type { Attribution } from "./identity.ts"

export interface ChannelDenialReceipt {
  readonly sandboxId: string
  readonly agentId: string
  readonly channel: ChannelId
  readonly code: ChannelDenial["code"]
  readonly detail: string
  readonly at: string
}

export class ChannelAccessError extends Data.TaggedError("ChannelAccessError")<{
  readonly code: ChannelDenial["code"]
  readonly channel: ChannelId
  readonly detail: string
}> {}

/** A channel host's admission gate; a denial is always receipted. */
export interface ChannelGate {
  readonly authorize: (
    record: SandboxRecord,
    caller: ProcessIdentity,
    channel: ChannelId,
  ) => Effect.Effect<Attribution, ChannelAccessError>
  readonly receipts: Effect.Effect<ReadonlyArray<ChannelDenialReceipt>>
}

export const ChannelGate = Context.Service<ChannelGate>("kanbei/kernel/ChannelGate")

export const channelGateLayer: Layer.Layer<ChannelGate> = Layer.effect(
  ChannelGate,
  Effect.gen(function* () {
    const receipts = yield* Ref.make<ReadonlyArray<ChannelDenialReceipt>>([])

    const deny = (
      record: SandboxRecord,
      channel: ChannelId,
      denial: ChannelDenial,
    ): Effect.Effect<never, ChannelAccessError> =>
      Effect.gen(function* () {
        const now = yield* Clock.currentTimeMillis
        yield* Ref.update(receipts, (current) => [
          ...current,
          {
            sandboxId: record.sandboxId,
            agentId: record.agentId,
            channel,
            code: denial.code,
            detail: denial.detail,
            at: new Date(now).toISOString(),
          },
        ])
        return yield* Effect.fail(
          new ChannelAccessError({
            code: denial.code,
            channel,
            detail: denial.detail,
          }),
        )
      })

    const authorize: ChannelGate["authorize"] = (record, caller, channel) => {
      const verdict = verifyCaller(record, caller, channel)
      return Result.isFailure(verdict)
        ? deny(record, channel, verdict.failure)
        : Effect.succeed(verdict.success)
    }

    return { authorize, receipts: Ref.get(receipts) }
  }),
)
