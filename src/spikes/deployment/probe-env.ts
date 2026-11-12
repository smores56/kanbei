import { DenoServices } from "@effect/platform-deno"
import { Effect, FileSystem, Layer, Path } from "effect"
import { HostCommand, hostCommandLayer } from "./host.ts"
import { ProbeEnv } from "./probe.ts"
import type { ProbeEnvShape } from "./probe.ts"

/** Builds the probe context over a scratch directory; the caller owns that directory. */
export const probeEnvLayer = (scratch: string): Layer.Layer<ProbeEnvShape> =>
  Layer.effect(
    ProbeEnv,
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem
      const path = yield* Path.Path
      const command = yield* HostCommand
      return { fs, path, command, scratch }
    }),
  ).pipe(Layer.provide(Layer.mergeAll(DenoServices.layer, hostCommandLayer)))
