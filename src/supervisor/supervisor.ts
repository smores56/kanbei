import { Context, Effect, Layer, Ref, Result } from "effect"
import { canonicalize } from "../evidence/canonical.ts"
import { sha256Text } from "../evidence/digest.ts"
import type { ChannelId, SandboxRecord } from "../kernel/identity.ts"
import type { ObservedPosture, SandboxManifest } from "../kernel/manifest.ts"
import { verifyStartup } from "./startup.ts"
import type { StartupRefusal } from "./startup.ts"

export interface StartedSandbox {
  readonly sandboxId: string
  readonly startToken: string
  readonly channels: ReadonlyArray<ChannelId>
}

export interface RefusalRecord {
  readonly sandboxId: string
  readonly code: StartupRefusal["code"]
  readonly detail: string
}

export interface Supervisor {
  readonly start: (
    manifest: SandboxManifest,
    observed: ObservedPosture,
  ) => Effect.Effect<StartedSandbox, StartupRefusal>
  readonly records: Effect.Effect<ReadonlyArray<SandboxRecord>>
  readonly refusals: Effect.Effect<ReadonlyArray<RefusalRecord>>
}

export const Supervisor = Context.Service<Supervisor>("kanbei/supervisor/Supervisor")

/** Deterministic: the same sandbox, cgroup, and generation yield the same token. */
export const startTokenFor = (manifest: SandboxManifest): Effect.Effect<string> =>
  sha256Text(
    canonicalize({
      sandboxId: manifest.sandboxId,
      agentId: manifest.agentId,
      generation: manifest.generation,
      cgroupPath: manifest.cgroupPath,
      channels: [...manifest.channels].toSorted(),
    }),
  ).pipe(Effect.orDie)

export const supervisorLayer = (): Layer.Layer<Supervisor> =>
  Layer.effect(
    Supervisor,
    Effect.gen(function* () {
      const records = yield* Ref.make<ReadonlyArray<SandboxRecord>>([])
      const refusals = yield* Ref.make<ReadonlyArray<RefusalRecord>>([])

      const start: Supervisor["start"] = (manifest, observed) => {
        const verdict = verifyStartup(manifest, observed)
        if (Result.isFailure(verdict)) {
          const refusal = verdict.failure
          return Ref.update(refusals, (current) => [
            ...current,
            { sandboxId: refusal.sandboxId, code: refusal.code, detail: refusal.detail },
          ]).pipe(Effect.flatMap(() => Effect.fail(refusal)))
        }
        return Effect.gen(function* () {
          const startToken = yield* startTokenFor(manifest)
          const record: SandboxRecord = {
            sandboxId: manifest.sandboxId,
            agentId: manifest.agentId,
            generation: manifest.generation,
            cgroupPath: manifest.cgroupPath,
            channels: manifest.channels,
            startToken,
          }
          yield* Ref.update(records, (current) => [...current, record])
          return {
            sandboxId: manifest.sandboxId,
            startToken,
            channels: manifest.channels,
          }
        })
      }

      return {
        start,
        records: Ref.get(records),
        refusals: Ref.get(refusals),
      }
    }),
  )
