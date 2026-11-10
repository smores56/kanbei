import { Schema } from "effect"

/** Supervisor-to-instance requests. One worker owns one agent instance. */
export const WorkerRequest = Schema.Union([
  Schema.Struct({ kind: Schema.Literal("ping") }),
  Schema.Struct({ kind: Schema.Literal("spin"), iterations: Schema.Number }),
  Schema.Struct({ kind: Schema.Literal("hostCalls"), count: Schema.Number }),
  Schema.Struct({ kind: Schema.Literal("boom") }),
  Schema.Struct({ kind: Schema.Literal("alloc"), pages: Schema.Number }),
  Schema.Struct({ kind: Schema.Literal("hang") }),
  Schema.Struct({ kind: Schema.Literal("denoProbe") }),
])
export type WorkerRequest = typeof WorkerRequest.Type

export const WorkerResponse = Schema.Union([
  Schema.Struct({
    kind: Schema.Literal("ready"),
    compileMs: Schema.Number,
    instantiateMs: Schema.Number,
    memoryPages: Schema.Number,
    imports: Schema.Number,
  }),
  Schema.Struct({
    kind: Schema.Literal("computed"),
    checksum: Schema.Number,
    wallMs: Schema.Number,
    hostCalls: Schema.Number,
  }),
  Schema.Struct({ kind: Schema.Literal("trapped"), message: Schema.String }),
  Schema.Struct({
    kind: Schema.Literal("allocated"),
    previousPages: Schema.Number,
    memoryPages: Schema.Number,
  }),
  Schema.Struct({ kind: Schema.Literal("pong") }),
  Schema.Struct({ kind: Schema.Literal("denoDenied"), denied: Schema.Boolean }),
  Schema.Struct({ kind: Schema.Literal("failed"), message: Schema.String }),
])
export type WorkerResponse = typeof WorkerResponse.Type
