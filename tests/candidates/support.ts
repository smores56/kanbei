import { Clock, Context, Effect, Layer, Option, Ref } from "effect"
import { fileBlobSinkLayer } from "../../src/adapters/blob-sink.ts"
import { fileLedgerStoreLayer } from "../../src/adapters/ledger-file.ts"
import { pointerFileLayer } from "../../src/adapters/pointer-file.ts"
import { CandidateSupervisor, candidateSupervisorLayer } from "../../src/candidates/supervisor.ts"
import type { CandidateWorkspace } from "../../src/candidates/manifest.ts"
import {
  ConsentDelivery,
  consentDeliveryLayer,
  HumanSessions,
  rulingCommitmentOf,
} from "../../src/candidates/consent.ts"
import type { CandidateApprovalReceipt, HumanSession } from "../../src/candidates/consent.ts"
import { contentStoreLayer } from "../../src/kernel/cas.ts"
import { Ledger, ledgerLayer } from "../../src/kernel/ledger.ts"
import { passingInputs } from "../../src/candidates/validation.ts"
import type { PointerStore } from "../../src/candidates/pointer.ts"

export const EPOCH = "epoch-1"
export const OWNER_ORIGIN = "https://kanbei.sammohr.dev"

export interface DeliveredToken {
  readonly sessionId: string
  readonly origin: string
  readonly token: string
}

export class ConsentLog extends Context.Service<
  ConsentLog,
  {
    readonly tokens: Effect.Effect<ReadonlyArray<DeliveredToken>>
    readonly latest: (sessionId: string) => Effect.Effect<DeliveredToken>
  }
>()("test/ConsentLog") {}

const consentLogLayer = (log: Ref.Ref<ReadonlyArray<DeliveredToken>>): Layer.Layer<ConsentLog> =>
  Layer.succeed(ConsentLog)({
    tokens: Ref.get(log),
    latest: (sessionId) =>
      Ref.get(log).pipe(
        Effect.flatMap((tokens) => {
          const found = tokens.findLast((entry) => entry.sessionId === sessionId)
          return found === undefined
            ? Effect.die(new Error(`no consent token delivered to ${sessionId}`))
            : Effect.succeed(found)
        }),
      ),
  })

export const humanSessionsLayer: Layer.Layer<HumanSessions> = Layer.succeed(HumanSessions)({
  verify: (sessionId, origin) =>
    Effect.succeed(
      sessionId.startsWith("human-") && origin === OWNER_ORIGIN
        ? Option.some<HumanSession>({ sessionId, origin, verifiedAtUs: 0 })
        : Option.none<HumanSession>(),
    ),
})

export interface HarnessOptions {
  readonly pointer?: (directory: string) => Layer.Layer<PointerStore>
  readonly clock?: Clock.Clock
}

export const movableClock = (
  start: number,
): {
  readonly clock: Clock.Clock
  readonly advance: (millis: number) => void
  readonly now: () => number
} => {
  let current = start
  return {
    now: () => current,
    advance: (millis) => {
      current += millis
    },
    clock: {
      currentTimeMillisUnsafe: () => current,
      currentTimeMillis: Effect.sync(() => current),
      currentTimeNanosUnsafe: () => BigInt(current) * 1_000_000n,
      currentTimeNanos: Effect.sync(() => BigInt(current) * 1_000_000n),
      monotonicTimeNanosUnsafe: () => BigInt(current) * 1_000_000n,
      monotonicTimeNanos: Effect.sync(() => BigInt(current) * 1_000_000n),
      sleep: () => Effect.void,
    },
  }
}

export const layers = (
  directory: string,
  options: HarnessOptions = {},
  delivery: Layer.Layer<ConsentDelivery> = consentDeliveryLayer(() => {}),
) => {
  const ledger = ledgerLayer(EPOCH).pipe(
    Layer.provide(fileLedgerStoreLayer(`${directory}/ledger.jsonl`)),
  )
  const cas = contentStoreLayer.pipe(Layer.provide(fileBlobSinkLayer(`${directory}/cas`)))
  const pointers = (options.pointer ?? ((root) => pointerFileLayer(`${root}/pointers`)))(directory)
  const supervisor = candidateSupervisorLayer.pipe(
    Layer.provide(
      Layer.mergeAll(
        ledger,
        cas,
        pointers,
        humanSessionsLayer,
        delivery,
      ),
    ),
  )
  return Layer.mergeAll(supervisor, ledger)
}

export const run = <A, E>(
  directory: string,
  effect: Effect.Effect<A, E, CandidateSupervisor | Ledger | ConsentLog>,
  options: HarnessOptions = {},
): Promise<A> => {
  const log = Effect.runSync(Ref.make<ReadonlyArray<DeliveredToken>>([]))
  const delivery = Layer.effect(
    ConsentDelivery,
    Effect.succeed({
      deliver: (session: HumanSession, token: string) =>
        Ref.update(log, (current) => [...current, { ...session, token }]).pipe(Effect.asVoid),
    }),
  )
  return Effect.runPromise(
    effect.pipe(
      Effect.provide(
        Layer.mergeAll(layers(directory, options, delivery), consentLogLayer(log)),
      ),
      options.clock === undefined
        ? (value) => value
        : Effect.provideService(Clock.Clock, options.clock),
    ),
  )
}

export const workspace = (
  overrides: Partial<CandidateWorkspace> = {},
): CandidateWorkspace => ({
  candidateId: "cand-1",
  scope: "project/kanbei",
  generation: 1,
  kind: "executable",
  capabilities: [{ capability: "channel.inbox.read", scope: "narrow" }],
  dependencies: [{ id: "dep.a", version: "1.0.0" }],
  schema: { contractVersion: "kb/1.0.0", dataVersion: 0 },
  ...overrides,
})

export const artifact = (text = "plugin bundle"): Uint8Array => new TextEncoder().encode(text)

export const inputs = passingInputs({ resolvedArtifacts: ["dep.a@1.0.0"] })

export const staged = (
  workspaceValue: CandidateWorkspace = workspace(),
  binary = artifact(),
  validationInputs = inputs,
) =>
  Effect.gen(function* () {
    const supervisor = yield* CandidateSupervisor
    const stagedRecord = yield* supervisor.stage(workspaceValue, binary)
    return yield* supervisor.validate(stagedRecord.manifest.candidateId, validationInputs)
  })

/** Requests approval and rules with the token the human channel received. */
export const approveFor = (sessionId: string, candidateId = "cand-1") =>
  Effect.gen(function* () {
    const supervisor = yield* CandidateSupervisor
    const log = yield* ConsentLog
    const request = yield* supervisor.requestApproval(candidateId, {
      sessionId,
      origin: OWNER_ORIGIN,
      verifiedAtUs: 0,
    }, 60_000_000)
    const delivered = yield* log.latest(sessionId)
    const receipt: CandidateApprovalReceipt = yield* supervisor.approve(candidateId, {
      token: delivered.token,
      consentDigest: request.display.consentDigest,
      candidateDigest: request.display.manifest.candidateDigest,
      sessionId,
      origin: OWNER_ORIGIN,
      ruling: "approve",
      rulingCommitment: yield* rulingCommitmentOf(
        request.display.consentDigest,
        "approve",
        request.display.tokenHash,
      ),
    })
    return { request, receipt }
  })
