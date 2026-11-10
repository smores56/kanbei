import { Effect, Schema } from "effect"
import { denoHost } from "../adapters/deno.ts"
import { armConfigSchema, armModeSchema, runArm } from "../arm.ts"
import type { ArmConfig, ArmMode } from "../arm.ts"

const mode: ArmMode = Schema.decodeUnknownSync(armModeSchema)(Deno.args[0])
const config: ArmConfig = Schema.decodeUnknownSync(Schema.fromJsonString(armConfigSchema))(
  Deno.args[1] ?? "null",
)

await Effect.runPromise(runArm(denoHost, mode, config))
