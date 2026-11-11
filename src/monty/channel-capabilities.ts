import { Effect, Option, Schema } from "effect"
import type { InboxReader } from "../channels/inbox/reader.ts"
import type { TodoWriter } from "../channels/todo/writer.ts"
import type { VaultReader } from "../channels/vault/reader.ts"
import type { SearchClient } from "../channels/search/client.ts"
import type { CsrClient } from "../channels/csr/client.ts"
import type { CapabilityBinding } from "./capability-host.ts"
import { CapabilityCallFailure } from "./capability-host.ts"

const fail = (capability: string, detail: string): CapabilityCallFailure =>
  new CapabilityCallFailure({ code: "capability_failed", capability, detail })

const parseArgs = <A>(
  capability: string,
  schema: Schema.ConstraintDecoder<A>,
  args: ReadonlyArray<unknown>,
): Effect.Effect<A, CapabilityCallFailure> =>
  Effect.try({
    try: () => Schema.decodeUnknownSync(schema, { onExcessProperty: "error" })(args[0] ?? {}),
    catch: (error) =>
      fail(capability, error instanceof Error ? error.message.slice(0, 200) : String(error)),
  })

const emptyArgs = Schema.Struct({})

/** Inbox access is a summary: the agent sees heartbeats, never raw batch bytes. */
export const inboxReadBinding = (inbox: InboxReader): CapabilityBinding => ({
  capability: "channel.inbox.read",
  run: (args) =>
    Effect.gen(function* () {
      yield* parseArgs("channel.inbox.read", emptyArgs, args)
      const state = yield* inbox.state
      return {
        observations: state.observations.length,
        receipts: state.receipts.length,
        gaps: state.gaps.length,
        heartbeats: state.heartbeats.map((heartbeat) => ({
          origin: heartbeat.origin,
          sequence: heartbeat.sequence,
          state: heartbeat.state,
        })),
      }
    }),
})

export const vaultReadBinding = (vault: VaultReader): CapabilityBinding => ({
  capability: "channel.vault.read",
  run: (args) =>
    Effect.gen(function* () {
      const input = yield* parseArgs(
        "channel.vault.read",
        Schema.Struct({ path: Schema.String }),
        args,
      )
      const read = yield* vault.read(input.path).pipe(
        Effect.mapError((error) => fail("channel.vault.read", `${error.code}: ${error.detail}`)),
      )
      return {
        path: read.path,
        digest: read.digest,
        bytes: read.bytes,
        modifiedAt: read.modifiedAt,
        contentBase64: new Uint8Array(read.content).toBase64(),
      }
    }),
})

export const todoReadBinding = (todo: TodoWriter): CapabilityBinding => ({
  capability: "channel.todo.read",
  run: (args) =>
    Effect.gen(function* () {
      yield* parseArgs("channel.todo.read", emptyArgs, args)
      const read = yield* todo.read.pipe(
        Effect.mapError((error) => fail("channel.todo.read", `${error.code}: ${error.detail}`)),
      )
      return {
        path: read.path,
        text: read.text,
        digest: read.digest,
        bytes: read.bytes,
        remainingBytes: read.remainingBytes,
      }
    }),
})

const TodoWriteArgs = Schema.Struct({
  text: Schema.String,
  path: Schema.optional(Schema.String),
  expectedDigest: Schema.optional(Schema.String),
  dryRun: Schema.optional(Schema.Boolean),
})

export const todoWriteBinding = (todo: TodoWriter): CapabilityBinding => ({
  capability: "channel.todo.write",
  run: (args) =>
    Effect.gen(function* () {
      const input = yield* parseArgs("channel.todo.write", TodoWriteArgs, args)
      const receipt = yield* todo.write({
        text: input.text,
        ...(input.path === undefined ? {} : { path: input.path }),
        ...(input.expectedDigest === undefined ? {} : { expectedDigest: input.expectedDigest }),
        ...(input.dryRun === undefined ? {} : { dryRun: input.dryRun }),
      }).pipe(
        Effect.mapError((error) => fail("channel.todo.write", `${error.code}: ${error.detail}`)),
      )
      return receipt
    }),
})

const SearchArgs = Schema.Struct({ query: Schema.String, limit: Schema.Number })

export const searchQueryBinding = (search: SearchClient): CapabilityBinding => ({
  capability: "channel.search.query",
  run: (args) =>
    Effect.gen(function* () {
      const input = yield* parseArgs("channel.search.query", SearchArgs, args)
      const outcome = yield* search.query(input)
      return {
        verdict: outcome.verdict,
        reason: Option.isSome(outcome.receipt.reason) ? outcome.receipt.reason.value : null,
        hits: outcome.hits.map((hit) => ({
          title: hit.title,
          locator: hit.locator,
          text: hit.text,
          rank: hit.rank,
        })),
      }
    }),
})

export const csrRenewBinding = (csr: CsrClient): CapabilityBinding => ({
  capability: "channel.csr.renew",
  run: (args) =>
    Effect.gen(function* () {
      yield* parseArgs("channel.csr.renew", emptyArgs, args)
      const outcome = yield* csr.renew.pipe(
        Effect.mapError((error) => fail("channel.csr.renew", `${error.code}: ${error.detail}`)),
      )
      return {
        state: outcome.state,
        renewed: outcome.renewed,
        staged: Option.isSome(outcome.staged),
      }
    }),
})

export interface ChannelCapabilityServices {
  readonly inbox: InboxReader
  readonly vault: VaultReader
  readonly todo: TodoWriter
  readonly search?: SearchClient
  readonly csr?: CsrClient
}

/** The only closures an agent gets; each one is a channel adapter call. */
export const channelCapabilityBindings = (
  services: ChannelCapabilityServices,
): ReadonlyArray<CapabilityBinding> => [
  inboxReadBinding(services.inbox),
  vaultReadBinding(services.vault),
  todoReadBinding(services.todo),
  todoWriteBinding(services.todo),
  ...(services.search === undefined ? [] : [searchQueryBinding(services.search)]),
  ...(services.csr === undefined ? [] : [csrRenewBinding(services.csr)]),
]
