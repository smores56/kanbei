import { Effect, Schema } from "effect"
import { nodeHost } from "../adapters/node.ts"
import { armConfigSchema, armModeSchema, runArm } from "../arm.ts"
import type { ArmConfig, ArmMode } from "../arm.ts"

const args = process.argv.slice(2)

const mode: ArmMode = Schema.decodeUnknownSync(armModeSchema)(args[0])
const config: ArmConfig = Schema.decodeUnknownSync(Schema.fromJsonString(armConfigSchema))(
  args[1] ?? "null",
)

await Effect.runPromise(runArm(nodeHost, mode, config))
