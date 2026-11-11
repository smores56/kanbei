import { Clock, Effect, Layer, Option, Result, Schema } from "effect"
import { fileLedgerStoreLayer } from "../adapters/ledger-file.ts"
import { todoFilesLayer } from "../adapters/todo-files.ts"
import type { TodoLimits } from "../channels/todo/writer.ts"
import { TodoWriter, todoWriterLayer } from "../channels/todo/writer.ts"
import type { SearchConfig, SearchOutcome } from "../channels/search/client.ts"
import { SearchChannel, SearchClient, searchClientLayer } from "../channels/search/client.ts"
import type { SearchQuery, SearchReply } from "../channels/search/schema.ts"
import { SearchQuery as SearchQuerySchema } from "../channels/search/schema.ts"
import { Ledger, ledgerLayer } from "../kernel/ledger.ts"
import type { SandboxManifest, SandboxMount } from "../kernel/manifest.ts"
import {
  MountClass,
  SandboxLimits,
  SandboxManifest as SandboxManifestSchema,
} from "../kernel/manifest.ts"
import type { ObservedPosture } from "../kernel/manifest.ts"
import { ChannelId } from "../kernel/identity.ts"
import { verifyStartup } from "../supervisor/startup.ts"
import {
  CONTENT_SECURITY_POLICY,
  CONTENT_SECURITY_POLICY_HEADER,
  shellResponseHeaders,
} from "../ui/csp.ts"
import type { ViewDocument } from "../ui/model.ts"
import { DISCLOSURE_FIELDS } from "../ui/model.ts"
import { renderView } from "../ui/render.ts"
import { makeSyntheticNetwork } from "../evidence/synthetic-network.ts"
import { agreement, currentAdvice, deriveAdvice } from "./advisor.ts"
import type { AdviceInput } from "./advisor.ts"
import { appendMemoryEvents, readMemory } from "./memory.ts"
import type { JourneyStepOutcome } from "./schema.ts"
import type { JourneyFixture, JourneySource } from "./schema.ts"
import { makeClock, runWorld, sourceStates } from "./world.ts"
import type { World } from "./world.ts"

export interface StepContext {
  readonly fixtureDirectory: string
  readonly fixture: JourneyFixture
  readonly source: JourneySource
}

const check = (
  checks: Array<string>,
  condition: boolean,
  message: string,
): void => {
  if (!condition) checks.push(message)
}

const json = (value: unknown): Schema.Json => Schema.decodeUnknownSync(Schema.Json)(value)

const adviceInputs = (world: World): ReadonlyArray<AdviceInput> => {
  const states = sourceStates(world.reportAfter)
  return world.observations.map((observation) => ({
    origin: observation.origin,
    resourceKey: observation.resourceKey,
    text: observation.body,
    state: states.get(observation.origin) === "stale"
      ? "stale"
      : states.get(observation.origin) === "unknown"
      ? "unknown"
      : "current",
  }))
}

const adviceKeys = (world: World, fixture: JourneyFixture) =>
  deriveAdvice(adviceInputs(world), {
    startIso: fixture.clock.start,
    horizonDays: fixture.advice.horizonDays,
  })

export const stepIngest = async (context: StepContext): Promise<JourneyStepOutcome> => {
  const world = await runWorld(context)
  const checks: Array<string> = []
  check(checks, world.poll.accepted === context.fixture.ingest.acceptedBatches.length, "accepted")
  check(checks, world.poll.duplicates === 1, "duplicate-batch-not-idempotent")
  check(checks, world.poll.quarantined === 0, "unexpected-quarantine")
  // Counts come from the canonical ledger and the declared batch set, never from
  // constants in the harness.
  const accepted = context.fixture.ingest.acceptedBatches.length
  const originsSeen = new Set(world.state.receipts.map((entry) => entry.origin))
  check(checks, world.state.receipts.length === accepted, "coverage-receipt-count")
  check(
    checks,
    world.state.observations.length === world.ledger.captureCount,
    "observation-ledger-divergence",
  )
  check(checks, world.ledger.recordCount === world.ledger.headSeq, "ledger-head-divergence")
  check(checks, world.state.heartbeats.length === originsSeen.size, "heartbeat-count")
  check(
    checks,
    world.state.observations.every((entry) =>
      entry.digest.startsWith("sha256:") && entry.batchDigest.startsWith("sha256:")
    ),
    "missing-provenance",
  )
  check(
    checks,
    world.state.observations.every((entry) => entry.resourceKey !== ""),
    "missing-origin-or-resource",
  )
  const gap = world.state.gaps.find((entry) => entry.kind === "gap")
  check(
    checks,
    gap?.origin === context.fixture.ingest.expectedGap.origin &&
      gap?.missing === context.fixture.ingest.expectedGap.missing &&
      gap?.sequence === context.fixture.ingest.expectedGap.sequence,
    "gap-not-recorded",
  )
  const suppression = world.state.gaps.find((entry) => entry.kind === "suppression")
  check(
    checks,
    suppression?.origin === context.fixture.ingest.expectedSuppression.origin &&
      suppression?.missing === context.fixture.ingest.expectedSuppression.missing,
    "suppression-not-recorded",
  )
  const finance = world.state.receipts.find((entry) => entry.origin === "finance")
  check(checks, finance?.coverage.complete === false, "coverage-honesty")
  return {
    testId: "J2-INGEST",
    outcome: checks.length === 0 ? "pass" : "fail",
    checks,
    metrics: {
      batchAccepted: world.poll.accepted,
      batchDuplicates: world.poll.duplicates,
      observations: world.state.observations.length,
      ledgerRecords: world.ledger.recordCount,
      ledgerCaptures: world.ledger.captureCount,
      receipts: world.state.receipts.length,
      heartbeats: world.state.heartbeats.length,
      gaps: world.state.gaps.filter((entry) => entry.kind === "gap").length,
      suppressions: world.state.gaps.filter((entry) => entry.kind === "suppression").length,
    },
    detail: json({
      poll: world.poll,
      gaps: world.state.gaps,
      receipts: world.state.receipts,
    }),
  }
}

export const stepFreshness = async (context: StepContext): Promise<JourneyStepOutcome> => {
  const world = await runWorld(context)
  const checks: Array<string> = []
  const before = sourceStates(world.reportBefore)
  const after = sourceStates(world.reportAfter)
  const stale = context.fixture.sources.staleOrigin
  const unknown = context.fixture.sources.unknownOrigin
  check(checks, before.get(stale) === "current", "fresh-before-window")
  check(checks, after.get(stale) === "stale", "stale-after-window")
  check(checks, after.get(unknown) === "unknown", "missing-heartbeat-not-unknown")
  check(checks, after.get("tuta-mail") === "current", "unrelated-source-marked-stale")
  const expired = world.alerts.find((alert) => alert.origin === stale && alert.kind === "expired")
  check(checks, expired !== undefined, "stale-alert-missing")
  const missing = world.alerts.find((alert) => alert.origin === unknown && alert.kind === "missing")
  check(checks, missing !== undefined, "unknown-alert-missing")
  const advice = adviceKeys(world, context.fixture)
  const staleAsFresh = currentAdvice(advice).filter((entry) => entry.origin === stale).length
  const missingAsCurrent = currentAdvice(advice).filter((entry) => entry.origin === unknown).length
  check(checks, staleAsFresh === 0, "stale-as-current")
  check(checks, missingAsCurrent === 0, "unknown-as-current")
  return {
    testId: "J2-FRESHNESS",
    outcome: checks.length === 0 ? "pass" : "fail",
    checks,
    metrics: {
      staleSources: [...after.values()].filter((state) => state === "stale").length,
      unknownSources: [...after.values()].filter((state) => state === "unknown").length,
      alerts: world.alerts.length,
      staleAsFresh,
      missingAsCurrent,
    },
    detail: json({
      before: [...before],
      after: [...after],
      alerts: world.alerts.map((alert) => ({
        origin: alert.origin,
        kind: alert.kind,
        lastSequence: Option.isSome(alert.lastSequence) ? alert.lastSequence.value : null,
        windowMs: alert.windowMs,
        at: alert.at,
      })),
    }),
  }
}

export const stepAdvice = async (context: StepContext): Promise<JourneyStepOutcome> => {
  const world = await runWorld(context)
  const advice = adviceKeys(world, context.fixture)
  const current = currentAdvice(advice)
  const expectedCurrent = new Set(context.fixture.advice.expected.map((entry) => entry.key))
  const expectedStale = new Set(context.fixture.advice.expectedStale.map((entry) => entry.key))
  const producedCurrent = new Set(current.map((entry) => entry.key))
  const producedStale = new Set(
    advice.filter((entry) => entry.state === "stale").map((entry) => entry.key),
  )
  const currentAgreement = agreement(expectedCurrent, producedCurrent)
  const staleAgreement = agreement(expectedStale, producedStale)
  const staleAsFresh = [...expectedStale].filter((key) => producedCurrent.has(key)).length
  const checks: Array<string> = []
  check(checks, currentAgreement.precision === 1, "advice-precision")
  check(checks, currentAgreement.recall === 1, "advice-recall")
  check(checks, staleAgreement.precision === 1, "stale-advice-precision")
  check(checks, staleAgreement.recall === 1, "stale-advice-recall")
  check(checks, staleAsFresh === 0, "stale-advice-presented-as-current")
  for (const expected of context.fixture.advice.expected) {
    const found = advice.find((entry) => entry.key === expected.key)
    check(checks, found?.dueAt === expected.dueAt, `advice-due-date:${expected.key}`)
  }
  return {
    testId: "J2-ADVICE",
    outcome: checks.length === 0 ? "pass" : "fail",
    checks,
    metrics: {
      advisoryPrecision: currentAgreement.precision,
      advisoryRecall: currentAgreement.recall,
      advisoryCurrent: current.length,
      advisoryStale: producedStale.size,
      staleAsFresh,
    },
    detail: json({
      advice,
      currentKeys: [...producedCurrent].toSorted(),
      staleKeys: [...producedStale].toSorted(),
    }),
  }
}

export const stepMemory = async (context: StepContext): Promise<JourneyStepOutcome> => {
  const directory = await Deno.makeTempDir({ prefix: "kanbei-journey-memory-" })
  const checks: Array<string> = []
  try {
    const result = await Effect.runPromise(
      Effect.gen(function* () {
        yield* appendMemoryEvents(context.fixture.memory.claims, context.fixture.memory.retractions)
        const ledger = yield* Ledger
        const records = yield* ledger.records
        return { memory: readMemory(records), head: yield* ledger.head }
      }).pipe(
        Effect.provide(
          ledgerLayer("journey-v2").pipe(
            Layer.provide(fileLedgerStoreLayer(`${directory}/ledger.jsonl`)),
          ),
        ),
        Effect.provideService(
          Clock.Clock,
          makeClock({ now: Date.parse(context.fixture.clock.start) }),
        ),
      ),
    )
    const states = new Map(result.memory.map((claim) => [claim.claimId, claim.state]))
    const expected = context.fixture.memory.expected
    const computed = new Map(
      result.memory.map((claim) => [
        claim.claimId,
        claim.state === "suspect" ? "suspect" : claim.state,
      ]),
    )
    const stateAgreement = agreement(
      new Set(Object.entries(expected).map(([id, state]) => `${id}=${state}`)),
      new Set([...computed].map(([id, state]) => `${id}=${state}`)),
    )
    check(checks, stateAgreement.precision === 1, "memory-precision")
    check(checks, stateAgreement.recall === 1, "memory-recall")
    check(
      checks,
      result.memory.find((claim) => claim.claimId === "old-landlord")?.state === "suspect",
      "retracted-claim-still-current",
    )
    check(checks, result.head.seq === 5, "memory-record-count")
    return {
      testId: "J2-MEMORY",
      outcome: checks.length === 0 ? "pass" : "fail",
      checks,
      metrics: {
        memoryPrecision: stateAgreement.precision,
        memoryRecall: stateAgreement.recall,
        claims: result.memory.length,
        contested: result.memory.filter((claim) => claim.state === "contested").length,
        suspect: result.memory.filter((claim) => claim.state === "suspect").length,
      },
      detail: json({
        memory: result.memory,
        head: result.head,
        headStates: [...states].map(([id, state]) => `${id}=${state}`).toSorted(),
      }),
    }
  } finally {
    await Deno.remove(directory, { recursive: true })
  }
}

export const stepTodo = async (context: StepContext): Promise<JourneyStepOutcome> => {
  const directory = await Deno.makeTempDir({ prefix: "kanbei-journey-todo-" })
  const limits: TodoLimits = context.fixture.todo.limits
  const program = Effect.gen(function* () {
    const writer = yield* TodoWriter
    const created = yield* writer.write({ text: context.fixture.todo.create })
    const completed = yield* writer.write({ text: context.fixture.todo.complete })
    const read = yield* writer.read
    const overSizeText = Array.from(
      { length: 3 },
      () => "x".repeat(Math.floor(context.fixture.todo.overSize / 3)),
    ).join("\n")
    const overSize = yield* Effect.flip(writer.write({ text: overSizeText }))
    const original = yield* writer.read
    yield* Effect.promise(() =>
      Deno.writeTextFile(`${directory}/todo.txt`, context.fixture.todo.conflictEdit)
    )
    const conflict = yield* Effect.flip(
      writer.write({ text: "agent edit\n", expectedDigest: original.digest }),
    )
    const afterConflict = yield* Effect.promise(() => Deno.readTextFile(`${directory}/todo.txt`))
    const receipts = yield* writer.receipts
    return { created, completed, read, overSize, conflict, afterConflict, receipts }
  })
  try {
    const result = await Effect.runPromise(
      program.pipe(
        Effect.provide(todoWriterLayer(limits).pipe(Layer.provide(todoFilesLayer(directory)))),
        Effect.provideService(
          Clock.Clock,
          makeClock({ now: Date.parse(context.fixture.clock.start) }),
        ),
      ),
    )
    const checks: Array<string> = []
    check(checks, result.created.applied, "create-not-applied")
    check(checks, result.completed.applied, "complete-not-applied")
    check(checks, result.read.text === context.fixture.todo.complete, "todo-read-back")
    check(checks, result.overSize.code === "todo_cap_exceeded", "todo-cap-not-refused")
    check(checks, result.conflict.code === "todo_conflict", "todo-conflict-not-refused")
    check(
      checks,
      result.afterConflict === context.fixture.todo.conflictEdit,
      "owner-edit-overwritten",
    )
    check(
      checks,
      result.read.remainingBytes >= 0 && result.read.bytes <= limits.maxBytes,
      "todo-within-cap",
    )
    check(
      checks,
      result.receipts.filter((receipt) => receipt.kind === "todo_write").length === 2,
      "todo-write-receipts",
    )
    return {
      testId: "J2-TODO",
      outcome: checks.length === 0 ? "pass" : "fail",
      checks,
      metrics: {
        todoWrites: result.receipts.filter((receipt) => receipt.kind === "todo_write").length,
        capViolations: result.overSize.code === "todo_cap_exceeded" ? 0 : 1,
        conflictOverwrites: result.afterConflict === context.fixture.todo.conflictEdit ? 0 : 1,
        remainingBytes: result.read.remainingBytes,
      },
      detail: json({
        created: result.created,
        completed: result.completed,
        read: result.read,
        capCode: result.overSize.code,
        conflictCode: result.conflict.code,
        receipts: result.receipts,
      }),
    }
  } finally {
    await Deno.remove(directory, { recursive: true })
  }
}

const outcomeOf = (outcome: SearchOutcome): string =>
  outcome.verdict === "hits" ? "hits" : (outcome.reason ?? "blocked")

export const stepSearch = async (context: StepContext): Promise<JourneyStepOutcome> => {
  const fixture = context.fixture.search
  const baseConfig: SearchConfig = { ...fixture.config }
  const calls: Array<SearchQuery> = []
  const replyFor = (query: SearchQuery): SearchReply => {
    if (query.query === fixture.allowed.query) {
      return { verdict: "allowed", hits: fixture.allowed.hits }
    }
    if (query.query === fixture.dlp.query) {
      return { verdict: "blocked", reason: fixture.dlp.reason }
    }
    if (query.query === fixture.judge.query) {
      return { verdict: "blocked", reason: fixture.judge.reason }
    }
    if (query.query === fixture.markup.query) {
      return { verdict: "allowed", hits: fixture.markup.hits }
    }
    if (fixture.chunked.queries.includes(query.query)) {
      return {
        verdict: "allowed",
        hits: [{ title: "hit", locator: "https://example.invalid/h", text: "ok", rank: 0 }],
      }
    }
    return { verdict: "blocked", reason: "schema" }
  }
  const handler = (query: SearchQuery): SearchReply => {
    calls.push(query)
    return replyFor(query)
  }
  const channel = Layer.succeed(SearchChannel)({
    search: (query) => Effect.succeed(handler(query)),
  })
  const clientFor = (config: SearchConfig) => searchClientLayer(config).pipe(Layer.provide(channel))
  const run = <A>(
    config: SearchConfig,
    effect: Effect.Effect<A, never, SearchClient>,
  ): Promise<A> => Effect.runPromise(effect.pipe(Effect.provide(clientFor(config))))

  const allowed = await run(
    baseConfig,
    Effect.gen(function* () {
      const client = yield* SearchClient
      return yield* client.query({ query: fixture.allowed.query, limit: fixture.allowed.limit })
    }),
  )
  const dlp = await run(
    baseConfig,
    Effect.gen(function* () {
      const client = yield* SearchClient
      return yield* client.query({ query: fixture.dlp.query, limit: fixture.dlp.limit })
    }),
  )
  const judge = await run(
    baseConfig,
    Effect.gen(function* () {
      const client = yield* SearchClient
      return yield* client.query({ query: fixture.judge.query, limit: fixture.judge.limit })
    }),
  )
  const markup = await run(
    baseConfig,
    Effect.gen(function* () {
      const client = yield* SearchClient
      return yield* client.query({ query: fixture.markup.query, limit: fixture.markup.limit })
    }),
  )
  const chunkedConfig: SearchConfig = {
    ...baseConfig,
    queriesPerDay: fixture.chunked.expected.length - 1,
  }
  const chunked = await run(
    chunkedConfig,
    Effect.gen(function* () {
      const client = yield* SearchClient
      return yield* Effect.forEach(
        fixture.chunked.queries,
        (query) => client.query({ query, limit: 1 }),
        { concurrency: 1 },
      )
    }),
  )
  const chunkedOutcomes = chunked.map(outcomeOf)
  const callsBeforeForged = calls.length
  const forgedAccepted = fixture.forgedFields.filter((field) => {
    const attempt: Record<string, unknown> = { query: "water", limit: 1 }
    attempt[field] = "forged"
    return Result.isSuccess(
      Result.try(() =>
        Schema.decodeUnknownSync(SearchQuerySchema, { onExcessProperty: "error" })(attempt)
      ),
    )
  })
  const forwarded = calls.slice(callsBeforeForged).filter((query) =>
    Object.keys(query).toSorted().join(",") !== "limit,query"
  )
  const allowedText = allowed.hits.map((hit) => hit.text).join(" ")
  const markupText = markup.hits.map((hit) => hit.text).join(" ")
  const checks: Array<string> = []
  check(checks, allowed.verdict === "hits", "allowed-query-blocked")
  check(checks, allowed.receipt.verdict === "hits", "allowed-receipt")
  check(checks, dlp.reason === "dlp", "dlp-not-blocked")
  check(checks, judge.reason === "judge", "judge-not-blocked")
  check(checks, dlp.receipt.verdict === "blocked", "dlp-receipt")
  check(checks, judge.receipt.verdict === "blocked", "judge-receipt")
  check(checks, !allowedText.includes("http"), "allowed-hit-kept-url")
  check(
    checks,
    !markupText.includes("<") && !markupText.includes("https://"),
    "markup-not-stripped",
  )
  check(
    checks,
    chunkedOutcomes.join(",") === fixture.chunked.expected.join(","),
    "chunked-budget",
  )
  check(checks, forgedAccepted.length === 0, "forged-field-accepted")
  check(checks, forwarded.length === 0, "forged-field-forwarded")
  const blockedRecall = ([dlp, judge].every((outcome) => outcome.verdict === "blocked") &&
      chunkedOutcomes.at(-1) === "budget")
    ? 1
    : 0
  return {
    testId: "J2-SEARCH",
    outcome: checks.length === 0 ? "pass" : "fail",
    checks,
    metrics: {
      blockedRecall,
      budgetOverruns: chunkedOutcomes[fixture.chunked.expected.length - 1] === "budget" ? 0 : 1,
      forgedFieldsForwarded: forwarded.length,
      searchCalls: calls.length,
      strippedHits: allowed.hits.length + markup.hits.length,
    },
    detail: json({
      allowed: { verdict: outcomeOf(allowed), hits: allowed.hits },
      dlp: outcomeOf(dlp),
      judge: outcomeOf(judge),
      markup: markup.hits,
      chunked: chunkedOutcomes,
      forgedAccepted,
      calls: calls.map((query) => Object.keys(query).toSorted()),
    }),
  }
}

const cspDocument = (fixture: JourneyFixture): ViewDocument => ({
  schemaVersion: 1,
  viewId: "journey-v2-csp",
  revision: "rev-1",
  route: "graph",
  title: "Journey",
  sections: [{
    id: "advice",
    title: "Advice",
    rows: [{
      id: "advice-1",
      anchor: "anchor:advice-1",
      cells: [{ label: "subject", text: fixture.csp.planted }],
      badges: [{ kind: "current", label: "current" }],
      actions: [],
      provenance: {
        observationId: "obs:1",
        span: "bytes 0-120",
        derivation: "capture",
        scope: "personal",
      },
    }],
  }],
  anchors: ["anchor:advice-1"],
  consentLink: "consent:decision-1",
  decisionAffordance: "action:rule",
  disclosure: DISCLOSURE_FIELDS.map((field) => ({ field, mode: "include" as const })),
  freshness: {
    state: "current",
    observedAt: "2026-11-11T00:00:00.000Z",
    windowMs: 3_600_000,
    pending: 0,
    watermarkSeq: 1,
  },
})

export const stepCsp = async (context: StepContext): Promise<JourneyStepOutcome> => {
  const network = await Effect.runPromise(makeSyntheticNetwork())
  await Effect.runPromise(
    network.serve("https://example.invalid/x.png", { status: 200, headers: {}, body: "image" }),
  )
  const rendered = renderView(cspDocument(context.fixture))
  const calls = await Effect.runPromise(network.calls)
  const checks: Array<string> = []
  check(checks, Result.isSuccess(rendered), "view-render-failed")
  if (Result.isSuccess(rendered)) {
    const text = rendered.success.text
    check(checks, rendered.success.mediaType === "text/plain; charset=utf-8", "media-type")
    check(checks, !rendered.success.lines.some((line) => line.startsWith("<")), "html-injected")
    check(checks, text.includes("https://example.invalid/x.png"), "planted-url-not-inert")
  }
  check(checks, calls.length === 0, "remote-request-made")
  check(
    checks,
    shellResponseHeaders[CONTENT_SECURITY_POLICY_HEADER] === CONTENT_SECURITY_POLICY,
    "csp-header",
  )
  check(checks, CONTENT_SECURITY_POLICY.includes("default-src 'none'"), "csp-default")
  check(checks, !/https?:/u.test(CONTENT_SECURITY_POLICY), "csp-allows-origin")
  return {
    testId: "J2-CSP",
    outcome: checks.length === 0 ? "pass" : "fail",
    checks,
    metrics: {
      cspRemoteRequests: calls.length,
      neutralized: Result.isSuccess(rendered) ? rendered.success.neutralized : 0,
    },
    detail: json({
      policy: CONTENT_SECURITY_POLICY,
      text: Result.isSuccess(rendered) ? rendered.success.lines : [],
      requests: calls.length,
    }),
  }
}

const mountsFor = (fixture: JourneyFixture): ReadonlyArray<SandboxMount> =>
  fixture.boundary.requiredMountClasses.map((name) => {
    const mountClass = Schema.decodeUnknownSync(MountClass)(name)
    return {
      path: `/run/kanbei/${name}`,
      access: mountClass === "inbox" || mountClass === "vault" || mountClass === "image" ||
          mountClass === "mutable_code" || mountClass === "runtime"
        ? "ro" as const
        : "rw" as const,
      class: mountClass,
    }
  })

export const stepBoundary = (context: StepContext): Promise<JourneyStepOutcome> => {
  const channels = Schema.decodeUnknownSync(Schema.Array(ChannelId))(
    context.fixture.boundary.declaredChannels,
  )
  const limits = Schema.decodeUnknownSync(SandboxLimits)({
    wallMs: 60_000,
    memoryBytes: 512_000_000,
    pidsMax: 64,
    cpuQuota: 100_000,
    openFiles: 256,
    todoBytes: 2_048,
  })
  const manifest = Schema.decodeUnknownSync(SandboxManifestSchema)({
    sandboxId: "journey-v2",
    agentId: "life-admin",
    generation: "gen-journey-v2",
    cgroupPath: "/kanbei/journey-v2",
    enforcementRequired: "process_cgroup",
    mounts: mountsFor(context.fixture),
    writableRoots: mountsFor(context.fixture)
      .filter((mount) => mount.access === "rw")
      .map((mount) => mount.path),
    channels,
    limits,
  })
  const observed: ObservedPosture = {
    enforcement: "process_cgroup",
    generation: manifest.generation,
    addresses: ["lo"],
    channelSockets: channels,
    mounts: manifest.mounts,
    writableRoots: manifest.writableRoots,
  }
  const declared = verifyStartup(manifest, observed)
  const leakManifest: SandboxManifest = {
    ...manifest,
    channels: Schema.decodeUnknownSync(Schema.Array(ChannelId))(
      context.fixture.boundary.leakManifestChannels,
    ),
  }
  const leaked = verifyStartup(leakManifest, observed)
  const checks: Array<string> = []
  check(checks, Result.isSuccess(declared), "declared-posture-refused")
  check(
    checks,
    Result.isFailure(leaked) && leaked.failure.code === "undeclared_channel",
    "undeclared-channel-not-refused",
  )
  return Promise.resolve({
    testId: "J2-BOUNDARY",
    outcome: checks.length === 0 ? "pass" : "fail",
    checks,
    metrics: {
      channelLeaks: Result.isSuccess(declared) ? 0 : 1,
      channels: channels.length,
      leakRefusals: Result.isFailure(leaked) ? 1 : 0,
    },
    detail: json({
      manifest,
      verdict: Result.isSuccess(declared) ? "started" : declared.failure.code,
      leakVerdict: Result.isFailure(leaked) ? leaked.failure.code : "not-refused",
      mountClasses: Schema.decodeUnknownSync(Schema.Array(MountClass))(
        context.fixture.boundary.requiredMountClasses,
      ),
    }),
  })
}

export const journeySteps = [
  stepIngest,
  stepFreshness,
  stepAdvice,
  stepMemory,
  stepTodo,
  stepSearch,
  stepCsp,
  stepBoundary,
] as const
