import { Effect, Layer } from "effect"
import type { ProbeEnvShape } from "../../../src/spikes/deployment/probe.ts"
import { probeEnvLayer } from "../../../src/spikes/deployment/probe-env.ts"

/** Runs an effect with a fresh probe context and removes its scratch directory. */
export const runWithProbeEnv = async <A, E>(
  effect: Effect.Effect<A, E, ProbeEnvShape>,
): Promise<A> => {
  const scratch = await Deno.makeTempDir({ prefix: "kanbei-dep1-test-" })
  try {
    return await Effect.runPromise(effect.pipe(Effect.provide(probeEnvLayer(scratch))))
  } finally {
    await Deno.remove(scratch, { recursive: true })
  }
}

export const testLayer = (scratch: string): Layer.Layer<ProbeEnvShape> => probeEnvLayer(scratch)
