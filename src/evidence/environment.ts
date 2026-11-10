import { Context, Effect, Layer } from "effect"
import { canonicalize } from "./canonical.ts"
import { sha256Text } from "./digest.ts"
import type { DigestError } from "./errors.ts"
import type { EnvironmentFingerprint } from "./types.ts"

export interface EnvironmentProbe {
  readonly fingerprint: Effect.Effect<EnvironmentFingerprint, DigestError>
}

export const EnvironmentProbe = Context.Service<EnvironmentProbe>(
  "kanbei/evidence/EnvironmentProbe",
)

const hostFacts = {
  hostname: Deno.hostname(),
  os: Deno.build.os,
  arch: Deno.build.arch,
  release: Deno.osRelease(),
  cpus: navigator.hardwareConcurrency,
}

export const liveEnvironmentProbeLayer = Layer.succeed(EnvironmentProbe, {
  fingerprint: Effect.gen(function* () {
    const hostFingerprint = yield* sha256Text(canonicalize(hostFacts))
    return {
      hostFingerprint,
      kernel: `${Deno.build.os}-${Deno.osRelease()}`,
      runtime: `deno@${Deno.version.deno}`,
      sandboxPosture: "unverified",
      brokerIdentity: "unconfigured",
    }
  }),
})

export const fixedEnvironmentProbeLayer = (
  fingerprint: EnvironmentFingerprint,
): Layer.Layer<EnvironmentProbe> =>
  Layer.succeed(EnvironmentProbe, { fingerprint: Effect.succeed(fingerprint) })
