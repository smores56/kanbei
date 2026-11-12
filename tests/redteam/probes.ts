import { assert, assertEquals } from "@std/assert"
import { Clock, Effect, Layer, Option, Ref, Result, Schema } from "effect"
import { ageDecryptorLayer } from "../../src/adapters/age.ts"
import { fileBlobSinkLayer } from "../../src/adapters/blob-sink.ts"
import { fileInboxCheckpointLayer } from "../../src/adapters/inbox-checkpoint.ts"
import { fileInboxQuarantineLayer } from "../../src/adapters/inbox-quarantine.ts"
import { fileLedgerStoreLayer } from "../../src/adapters/ledger-file.ts"
import { contentStoreLayer } from "../../src/kernel/cas.ts"
import { ledgerLayer } from "../../src/kernel/ledger.ts"
import { directoryLayer } from "../../src/adapters/inbox-directory.ts"
import { todoFilesLayer } from "../../src/adapters/todo-files.ts"
import { vaultFilesLayer } from "../../src/adapters/vault-files.ts"
import { InboxReader, inboxReaderLayer } from "../../src/channels/inbox/reader.ts"
import type { InboxConfig } from "../../src/channels/inbox/reader.ts"
import { InboxDirectory } from "../../src/channels/inbox/directory.ts"
import { VaultReader, vaultReaderLayer } from "../../src/channels/vault/reader.ts"
import { TodoWriter, todoWriterLayer } from "../../src/channels/todo/writer.ts"
import type { TodoWriter as TodoWriterShape } from "../../src/channels/todo/writer.ts"
import { CsrClient } from "../../src/channels/csr/client.ts"
import { generateSessionKey } from "../../src/channels/csr/key.ts"
import type { SearchConfig } from "../../src/channels/search/client.ts"
import { SearchClient } from "../../src/channels/search/client.ts"
import type { SearchQuery, SearchReply } from "../../src/channels/search/schema.ts"
import { SearchQuery as SearchQuerySchema } from "../../src/channels/search/schema.ts"
import type { ObservedPosture, SandboxManifest } from "../../src/kernel/manifest.ts"
import {
  ObservedPosture as ObservedPostureSchema,
  SandboxManifest as SandboxManifestSchema,
} from "../../src/kernel/manifest.ts"
import { verifyStartup } from "../../src/supervisor/startup.ts"
import { CONTENT_SECURITY_POLICY } from "../../src/ui/csp.ts"
import { renderView } from "../../src/ui/render.ts"
import { makeSyntheticNetwork } from "../../src/evidence/synthetic-network.ts"
import { sha256Text } from "../../src/evidence/digest.ts"
import { makeClock } from "../../src/journey/world.ts"
import { baseDocument } from "../ui/support.ts"
import { IDENTITY, ORIGIN, request, serveChain, serveOne } from "../channels/chat/support.ts"
import {
  certificateFor,
  defaultResponder,
  harness as csrHarness,
  runWith as csrRunWith,
} from "../channels/csr/support.ts"
import { harness as searchHarness, mutableClock, onClient } from "../channels/search/support.ts"
import { withTempDir } from "../support.ts"

export interface ProbeCase {
  readonly id: string
  readonly channel: string
  readonly probe: string
  readonly expected: string
}

export interface ProbeCatalogue {
  readonly schemaVersion: number
  readonly dataset: string
  readonly version: string
  readonly suite: string
  readonly cspPolicyDigest: string
  readonly cases: ReadonlyArray<ProbeCase>
}

export interface ProbeResult extends ProbeCase {
  readonly outcome: string
  readonly receipt: boolean
  readonly detail: string
}

const result = (
  probeCase: ProbeCase,
  outcome: string,
  receipt: boolean,
  detail: string,
): ProbeResult => ({ ...probeCase, outcome, receipt, detail })

export const loadCatalogue = async (root: string): Promise<ProbeCatalogue> =>
  Schema.decodeUnknownSync(
    Schema.Struct({
      schemaVersion: Schema.Number,
      dataset: Schema.String,
      version: Schema.String,
      suite: Schema.String,
      cspPolicyDigest: Schema.String,
      cases: Schema.Array(
        Schema.Struct({
          id: Schema.String,
          channel: Schema.String,
          probe: Schema.String,
          expected: Schema.String,
        }),
      ),
    }),
  )(JSON.parse(await Deno.readTextFile(`${root}/fixtures/security-v1/channel-probes.json`)))

const byId = (catalogue: ProbeCatalogue, id: string): ProbeCase => {
  const found = catalogue.cases.find((entry) => entry.id === id)
  if (found === undefined) throw new Error(`unknown redteam case ${id}`)
  return found
}

const INBOX_LIMITS: InboxConfig["limits"] = {
  maxBatchBytes: 200_000,
  maxBatchesPerPoll: 20,
  maxEntriesPerBatch: 8,
  maxEntryBytes: 100_000,
  maxTotalBlobBytes: 500_000,
}

export const inboxProbes = async (
  root: string,
  catalogue: ProbeCatalogue,
): Promise<ReadonlyArray<ProbeResult>> => {
  const results: Array<ProbeResult> = []
  const directory = await Deno.makeTempDir({ prefix: "kanbei-redteam-inbox-" })
  try {
    const source = await Deno.readFile(`${root}/fixtures/ingest-v1/batches/tuta-mail-1.age`)
    await Deno.writeFile(`${directory}/tuta-mail-1.age`, source)
    await Deno.writeFile(`${directory}/tuta-mail-1-replay.age`, source)
    const tampered = new Uint8Array(source)
    tampered[tampered.length - 1] = (tampered[tampered.length - 1] ?? 0) ^ 0xff
    await Deno.writeFile(`${directory}/tampered.age`, tampered)
    await Deno.writeFile(`${directory}/oversized.age`, new Uint8Array(400_000))

    const traversal = await Effect.runPromise(
      Effect.gen(function* () {
        const inbox = yield* InboxDirectory
        return yield* Effect.flip(inbox.read("../escape.age"))
      }).pipe(Effect.provide(directoryLayer(directory))),
    )
    results.push(
      result(
        byId(catalogue, "INBOX-PATH-TRAVERSAL"),
        traversal.reason === "name is not a base name"
          ? "denied:inbox_name_invalid"
          : `accepted:${traversal.reason}`,
        // Refused by the directory adapter before any receipt can exist.
        false,
        traversal.reason,
      ),
    )

    const identityPath = `${root}/fixtures/ingest-v1/keys/identity.txt`
    const config: InboxConfig = {
      identityPath,
      allowedOrigins: ["tuta-mail"],
      limits: INBOX_LIMITS,
      freshnessWindowMs: 3_600_000,
    }
    const blobs = fileBlobSinkLayer(`${directory}/cas`)
    const cas = contentStoreLayer.pipe(Layer.provide(blobs))
    const ledger = ledgerLayer("redteam").pipe(
      Layer.provide(fileLedgerStoreLayer(`${directory}/ledger.jsonl`)),
    )
    const checkpoints = fileInboxCheckpointLayer(`${directory}/checkpoint.json`)
    const quarantine = fileInboxQuarantineLayer(`${directory}/quarantine.json`)
    const world = await Effect.runPromise(
      Effect.gen(function* () {
        const reader = yield* InboxReader
        const poll = yield* reader.poll
        return { poll, state: yield* reader.state }
      }).pipe(
        Effect.provide(
          Layer.mergeAll(
            inboxReaderLayer(config).pipe(
              Layer.provide(directoryLayer(directory)),
              Layer.provide(ageDecryptorLayer({ identityPath })),
              Layer.provide(checkpoints),
              Layer.provide(quarantine),
              Layer.provide(cas),
              Layer.provide(ledger),
            ),
            cas,
            ledger,
            checkpoints,
            quarantine,
          ),
        ),
        Effect.provideService(
          Clock.Clock,
          makeClock({ now: Date.parse("2026-11-11T00:00:00.000Z") }),
        ),
      ),
    )
    const reasons = world.state.quarantined.map((entry) => `${entry.name}:${entry.reason}`)
    const decrypted = world.state.quarantined.find((entry) => entry.name === "tampered.age")
    results.push(
      result(
        byId(catalogue, "INBOX-CIPHERTEXT-SUBSTITUTION"),
        decrypted?.reason === "decrypt_failed"
          ? "quarantined:decrypt_failed"
          : `accepted:${decrypted?.reason ?? "none"}`,
        decrypted !== undefined,
        reasons.join(","),
      ),
    )
    const oversized = world.state.quarantined.find((entry) => entry.name === "oversized.age")
    results.push(
      result(
        byId(catalogue, "INBOX-OVERSIZED"),
        oversized?.reason === "batch_too_large"
          ? "quarantined:batch_too_large"
          : `accepted:${oversized?.reason ?? "none"}`,
        oversized !== undefined,
        reasons.join(","),
      ),
    )
    results.push(
      result(
        byId(catalogue, "INBOX-REPLAY"),
        world.poll.duplicates === 1 && world.poll.accepted === 1
          ? "duplicate"
          : `accepted:${world.poll.accepted};duplicates:${world.poll.duplicates}`,
        // A duplicate is a poll outcome, not a receipt row.
        false,
        JSON.stringify(world.poll),
      ),
    )
    return results
  } finally {
    await Deno.remove(directory, { recursive: true })
  }
}

export const vaultProbes = async (
  _root: string,
  catalogue: ProbeCatalogue,
): Promise<ReadonlyArray<ProbeResult>> => {
  const results: Array<ProbeResult> = []
  await withTempDir("redteam-vault", async (directory) => {
    // The probe mounts a realistic subtree, not the whole temp dir: a root of
    // "" would make every path inside the mount trivially allowed.
    const vault = `${directory}/vault`
    await Deno.mkdir(vault)
    await Deno.writeTextFile(`${directory}/outside.md`, "secret")
    await Deno.writeTextFile(`${vault}/outside.md`, "secret")
    await Deno.symlink(`${directory}/outside.md`, `${vault}/link.md`)
    await Deno.writeTextFile(`${vault}/original.md`, "linked")
    await Deno.link(`${vault}/original.md`, `${vault}/hard.md`)
    const probes = [
      ["VAULT-SYMLINK", "vault/link.md"],
      ["VAULT-HARDLINK", "vault/hard.md"],
      ["VAULT-TRAVERSAL", "vault/../outside.md"],
    ] as const
    const outcomes = await Promise.all(
      probes.map(async ([id, path]) => {
        const error = await Effect.runPromise(
          Effect.gen(function* () {
            const reader = yield* VaultReader
            return yield* Effect.flip(reader.read(path))
          }).pipe(
            Effect.provide(
              vaultReaderLayer({ roots: ["vault"], maxBytes: 4_096 }).pipe(
                Layer.provide(vaultFilesLayer(directory)),
              ),
            ),
          ),
        )
        return result(byId(catalogue, id), `denied:${error.code}`, false, `${path}: ${error.code}`)
      }),
    )
    results.push(...outcomes)
  })
  return results
}

export const todoProbes = async (
  _root: string,
  catalogue: ProbeCatalogue,
): Promise<ReadonlyArray<ProbeResult>> => {
  const results: Array<ProbeResult> = []
  const limits = { maxBytes: 4_096, maxWriteBytes: 256, maxLines: 40, maxLineChars: 120 }
  const runTodo = <A, E>(
    directory: string,
    effect: Effect.Effect<A, E, TodoWriterShape>,
  ): Promise<A> =>
    Effect.runPromise(
      effect.pipe(
        Effect.provide(todoWriterLayer(limits).pipe(Layer.provide(todoFilesLayer(directory)))),
        Effect.provideService(Clock.Clock, makeClock({ now: 1_000_000 })),
      ),
    )

  await withTempDir("redteam-todo-quota", async (directory) => {
    const error = await runTodo(
      directory,
      Effect.gen(function* () {
        const writer = yield* TodoWriter
        return yield* Effect.flip(
          writer.write({ text: Array.from({ length: 3 }, () => "x".repeat(100)).join("\n") }),
        )
      }),
    )
    results.push(
      result(byId(catalogue, "TODO-QUOTA"), `denied:${error.code}`, false, error.code),
    )
  })
  await withTempDir("redteam-todo-symlink", async (directory) => {
    await Deno.writeTextFile(`${directory}/outside.txt`, "outside\n")
    await Deno.symlink(`${directory}/outside.txt`, `${directory}/todo.txt`)
    const error = await runTodo(
      directory,
      Effect.gen(function* () {
        const writer = yield* TodoWriter
        return yield* Effect.flip(writer.write({ text: "new\n" }))
      }),
    )
    const untouched = await Deno.readTextFile(`${directory}/outside.txt`)
    results.push(
      result(byId(catalogue, "TODO-SYMLINK"), `denied:${error.code}`, false, untouched.trim()),
    )
    assertEquals(untouched, "outside\n")
  })
  await withTempDir("redteam-todo-conflict", async (directory) => {
    const outcome = await runTodo(
      directory,
      Effect.gen(function* () {
        const writer = yield* TodoWriter
        yield* writer.write({
          text: "planned\n",
          expectedDigest: yield* sha256Text("").pipe(Effect.orDie),
        })
        const original = yield* writer.read
        yield* Effect.promise(() => Deno.writeTextFile(`${directory}/todo.txt`, "owner edit\n"))
        const error = yield* Effect.flip(
          writer.write({ text: "agent edit\n", expectedDigest: original.digest }),
        )
        return {
          error,
          after: yield* Effect.promise(() => Deno.readTextFile(`${directory}/todo.txt`)),
        }
      }),
    )
    results.push(
      result(
        byId(catalogue, "TODO-CONFLICT"),
        `denied:${outcome.error.code}`,
        false,
        outcome.after.trim(),
      ),
    )
    assertEquals(outcome.after, "owner edit\n")
  })
  return results
}

const errorCode = (
  outbound: ReadonlyArray<{ kind: string; error?: { code: string } | undefined }>,
): string => outbound.at(-1)?.error?.code ?? "none"

const receipted = (receipts: ReadonlyArray<{ outcome: "ok" | "denied" }>): boolean =>
  receipts.some((receipt) => receipt.outcome === "denied")

export const chatProbes = async (
  _root: string,
  catalogue: ProbeCatalogue,
): Promise<ReadonlyArray<ProbeResult>> => {
  const results: Array<ProbeResult> = []

  const missing = await serveOne({ route: "status", accessIdentity: IDENTITY, origin: ORIGIN })
  results.push(
    result(
      byId(catalogue, "CHAT-NO-TOKEN"),
      `denied:${errorCode(missing.outbound)}`,
      receipted(missing.receipts),
      errorCode(missing.outbound),
    ),
  )
  const badPasskey = await serveOne(request({ passkey: "wrong passkey" }))
  results.push(
    result(
      byId(catalogue, "CHAT-BAD-PASSKEY"),
      `denied:${errorCode(badPasskey.outbound)}`,
      receipted(badPasskey.receipts),
      errorCode(badPasskey.outbound),
    ),
  )
  const foreign = await serveOne(request({ origin: "https://evil.invalid" }))
  results.push(
    result(
      byId(catalogue, "CHAT-FOREIGN-ORIGIN"),
      `denied:${errorCode(foreign.outbound)}`,
      receipted(foreign.receipts),
      errorCode(foreign.outbound),
    ),
  )
  const oversized = await serveOne(request({
    route: "chat.turn",
    payload: { turnId: "turn-1", text: "x".repeat(9_000) },
  }))
  results.push(
    result(
      byId(catalogue, "CHAT-OVERSIZED"),
      `denied:${errorCode(oversized.outbound)}`,
      receipted(oversized.receipts),
      errorCode(oversized.outbound),
    ),
  )
  const session = await serveChain([
    () => request(),
    (sessionId) => request({ route: "decision.list", sessionId, payload: {} }),
  ])
  assert(session.receipts.every((receipt) => receipt.outcome === "ok"))
  return results
}

export const csrProbes = async (
  _root: string,
  catalogue: ProbeCatalogue,
): Promise<ReadonlyArray<ProbeResult>> => {
  const results: Array<ProbeResult> = []
  const now = Date.parse("2026-11-11T00:00:00.000Z")
  const reap = async (
    id: string,
    h: Awaited<ReturnType<typeof csrHarness>> extends Effect.Effect<infer A> ? A : never,
  ) => {
    const outcome = await csrRunWith(
      h,
      now,
      Effect.gen(function* () {
        const client = yield* CsrClient
        const error = yield* Effect.flip(client.renew)
        return { error, receipts: yield* client.receipts }
      }),
    )
    const calls = await Effect.runPromise(Ref.get(h.transport.calls))
    const staged = await Effect.runPromise(Ref.get(h.store.staged))
    assertEquals(Option.isNone(staged), true)
    results.push(
      result(
        byId(catalogue, id),
        `denied:${outcome.error.code}`,
        outcome.receipts.some((receipt) => receipt.outcome === "refused"),
        `${outcome.error.code}; CA calls ${calls.length}`,
      ),
    )
    return calls.length
  }

  const wrongCn = await Effect.runPromise(
    csrHarness(
      () => Effect.die("the CA must not be called for a wrong CN"),
      Option.none(),
      { commonName: "evil.example" },
    ),
  )
  const cnCalls = await reap("CSR-WRONG-CN", wrongCn)
  assertEquals(cnCalls, 0)

  const wrongSan = await Effect.runPromise(
    csrHarness((csr) =>
      Effect.succeed(defaultResponder(csr, now, 90 * 24 * 60 * 60 * 1000, "evil.example"))
    ),
  )
  await reap("CSR-WRONG-SAN", wrongSan)

  const foreign = await Effect.runPromise(generateSessionKey())
  const wrongKey = await Effect.runPromise(
    csrHarness(() =>
      Effect.succeed({
        chain: certificateFor(
          "kanbei.sammohr.dev",
          foreign.publicKeySpkiDer,
          now - 1000,
          now + 90 * 24 * 60 * 60 * 1000,
        ),
        notAfter: new Date(now + 90 * 24 * 60 * 60 * 1000).toISOString().replace(
          /\.\d{3}Z$/u,
          "Z",
        ),
      })
    ),
  )
  await reap("CSR-WRONG-KEY", wrongKey)

  const missing = await Effect.runPromise(
    csrHarness(() => Effect.succeed({ chain: "", notAfter: "" })),
  )
  await reap("CSR-WRONG-SPKI", missing)
  return results
}

const outcomeCode = (outcome: { verdict: "hits" | "blocked"; reason?: string }): string =>
  outcome.verdict === "hits" ? "hits" : (outcome.reason ?? "blocked")

const blockedReply = (reason: "dlp" | "judge") => (): SearchReply => ({
  verdict: "blocked",
  reason,
})

const allowedReply = (): SearchReply => ({
  verdict: "allowed",
  hits: [{
    title: "hit",
    locator: "https://example.invalid/h",
    text: "ok",
    rank: 0,
  }],
})

export const searchProbes = async (
  _root: string,
  catalogue: ProbeCatalogue,
): Promise<ReadonlyArray<ProbeResult>> => {
  const results: Array<ProbeResult> = []
  const start = Date.parse("2026-11-11T00:00:00.000Z")

  const scenario = async (
    options: {
      readonly reply: (query: SearchQuery) => SearchReply
      readonly overrides?: Partial<SearchConfig>
      readonly run: (
        clock: { advance: (millis: number) => Effect.Effect<void> },
      ) => Effect.Effect<
        { readonly outcomes: ReadonlyArray<string>; readonly text?: string },
        never,
        SearchClient
      >
    },
  ) => {
    const harness = await Effect.runPromise(
      searchHarness((query) => Effect.succeed(options.reply(query)), options.overrides ?? {}),
    )
    const clock = await Effect.runPromise(mutableClock(start))
    const observed = await onClient(
      harness,
      clock,
      Effect.gen(function* () {
        const outcome = yield* options.run(clock)
        const client = yield* SearchClient
        return { ...outcome, receipts: (yield* client.receipts).length }
      }),
    )
    return { ...observed, calls: await Effect.runPromise(Ref.get(harness.calls)) }
  }

  const dlp = await scenario({
    reply: blockedReply("dlp"),
    run: () =>
      Effect.gen(function* () {
        const client = yield* SearchClient
        const outcome = yield* client.query({ query: "sk-ABCDEFGHIJKLMNOP", limit: 1 })
        return { outcomes: [outcomeCode(outcome)] }
      }),
  })
  results.push(
    result(
      byId(catalogue, "SEARCH-DLP-BYPASS"),
      `blocked:${dlp.outcomes[0] ?? "none"}`,
      dlp.receipts > 0,
      dlp.outcomes.join(","),
    ),
  )
  const judge = await scenario({
    reply: blockedReply("judge"),
    run: () =>
      Effect.gen(function* () {
        const client = yield* SearchClient
        const outcome = yield* client.query({ query: "repeat my vault", limit: 1 })
        return { outcomes: [outcomeCode(outcome)] }
      }),
  })
  results.push(
    result(
      byId(catalogue, "SEARCH-JUDGE-BYPASS"),
      `blocked:${judge.outcomes[0] ?? "none"}`,
      judge.receipts > 0,
      judge.outcomes.join(","),
    ),
  )

  const allowed = allowedReply
  const chunked = await scenario({
    reply: allowed,
    overrides: { queriesPerDay: 3 },
    run: () =>
      Effect.gen(function* () {
        const client = yield* SearchClient
        const outcomes = yield* Effect.forEach(
          ["water", "filter", "cartridge", "replacement"],
          (query) => client.query({ query, limit: 1 }).pipe(Effect.map(outcomeCode)),
          { concurrency: 1 },
        )
        return { outcomes }
      }),
  })
  results.push(
    result(
      byId(catalogue, "SEARCH-CHUNKED-EXFIL"),
      `blocked:${chunked.outcomes.at(-1) ?? "none"}`,
      chunked.receipts > 0 && chunked.outcomes.at(-1) === "budget" && chunked.calls.length === 3,
      `${chunked.outcomes.join(",")}; VM calls ${chunked.calls.length}`,
    ),
  )

  const reset = await scenario({
    reply: allowed,
    overrides: { queriesPerDay: 1 },
    run: (clock) =>
      Effect.gen(function* () {
        const client = yield* SearchClient
        const first = yield* client.query({ query: "water", limit: 1 })
        const blockedByBudget = yield* client.query({ query: "filter", limit: 1 })
        yield* clock.advance(24 * 60 * 60 * 1000)
        const nextDay = yield* client.query({ query: "cartridge", limit: 1 })
        return {
          outcomes: [outcomeCode(first), outcomeCode(blockedByBudget), outcomeCode(nextDay)],
        }
      }),
  })
  results.push(
    result(
      byId(catalogue, "SEARCH-BUDGET-RESET"),
      reset.outcomes.at(-1) ?? "none",
      reset.receipts > 0 && reset.outcomes[1] === "budget" && reset.outcomes[2] === "hits",
      reset.outcomes.join(","),
    ),
  )

  const markup = await scenario({
    reply: () => ({
      verdict: "allowed",
      hits: [{
        title: "<b>Reading</b>",
        locator: "https://example.invalid/a",
        text: "<script>alert(1)</script> https://evil.invalid/ exfil",
        rank: 0,
      }],
    }),
    run: () =>
      Effect.gen(function* () {
        const client = yield* SearchClient
        const outcome = yield* client.query({ query: "reading", limit: 1 })
        const text = outcome.hits.map((hit) => `${hit.title} ${hit.text}`).join(" ")
        return { outcomes: [outcomeCode(outcome)], text }
      }),
  })
  const cleanText = markup.text ?? ""
  results.push(
    result(
      byId(catalogue, "SEARCH-MARKUP"),
      markup.outcomes[0] === "hits" && !cleanText.includes("<") && !cleanText.includes("http")
        ? "stripped"
        : `unstripped:${cleanText}`,
      markup.receipts > 0,
      cleanText,
    ),
  )

  const forged = await scenario({
    reply: allowed,
    run: () =>
      Effect.sync(() => {
        const attempts = ["provider", "url", "authorization", "budget"]
        const accepted = attempts.filter((field) => {
          const attempt: Record<string, unknown> = { query: "water", limit: 1 }
          attempt[field] = "forged"
          return Result.isSuccess(
            Result.try(() =>
              Schema.decodeUnknownSync(SearchQuerySchema, { onExcessProperty: "error" })(attempt)
            ),
          )
        })
        return { outcomes: accepted.length === 0 ? ["schema"] : accepted }
      }),
  })
  results.push(
    result(
      byId(catalogue, "SEARCH-FORGED-FIELD"),
      forged.outcomes[0] === "schema" ? "denied:schema" : "accepted:forged",
      // Nothing was queried, so no receipt exists; the call count is the evidence.
      false,
      `${forged.outcomes.join(",")}; VM calls ${forged.calls.length}`,
    ),
  )
  return results
}

export const cspProbes = async (
  _root: string,
  catalogue: ProbeCatalogue,
): Promise<ReadonlyArray<ProbeResult>> => {
  const network = await Effect.runPromise(makeSyntheticNetwork())
  await Effect.runPromise(
    network.serve("https://example.invalid/x.png", { status: 200, headers: {}, body: "image" }),
  )
  const document = baseDocument()
  const row = document.sections[0]?.rows[0]
  if (row === undefined) throw new Error("baseline document has no row")
  const planted = baseDocument({
    sections: [{
      id: "queue",
      title: "Queue",
      rows: [{
        ...row,
        cells: [{
          label: "subject",
          text:
            '![x](https://example.invalid/x.png) <a href="https://example.invalid/l">l</a> <iframe src="https://example.invalid/f"></iframe> <style>body{background:url(https://example.invalid/c)}</style> <https://example.invalid/a>',
        }],
      }],
    }],
  })
  const rendered = renderView(planted)
  const calls = await Effect.runPromise(network.calls)
  const policyDigest = await Effect.runPromise(sha256Text(CONTENT_SECURITY_POLICY))
  const ok = Result.isSuccess(rendered) && calls.length === 0 &&
    policyDigest === catalogue.cspPolicyDigest
  return [
    result(
      byId(catalogue, "CSP-PLANTED-RESOURCES"),
      ok ? "zero-requests" : "request-or-policy-drift",
      // The evidence is the zero-request synthetic network, not a receipt.
      false,
      `requests ${calls.length}; policy ${policyDigest}`,
    ),
  ]
}

const boundaryManifest = (): SandboxManifest =>
  Schema.decodeUnknownSync(SandboxManifestSchema)({
    sandboxId: "redteam",
    agentId: "agent-1",
    generation: "gen-1",
    cgroupPath: "/kanbei/redteam",
    enforcementRequired: "process_cgroup",
    mounts: [
      { path: "/run/kanbei/inbox", access: "ro", class: "inbox" },
      { path: "/run/kanbei/vault", access: "ro", class: "vault" },
      { path: "/run/kanbei/todo", access: "rw", class: "todo" },
    ],
    writableRoots: ["/run/kanbei/todo"],
    channels: ["inbox", "vault", "todo", "chat", "csr", "search"],
    limits: {
      wallMs: 1000,
      memoryBytes: 1024,
      pidsMax: 4,
      cpuQuota: 1,
      openFiles: 16,
      todoBytes: 512,
    },
  })

const boundaryPosture = (overrides: Record<string, unknown> = {}): ObservedPosture =>
  Schema.decodeUnknownSync(ObservedPostureSchema)({
    enforcement: "process_cgroup",
    generation: "gen-1",
    addresses: ["lo"],
    channelSockets: ["inbox", "vault", "todo", "chat", "csr", "search"],
    mounts: boundaryManifest().mounts,
    writableRoots: boundaryManifest().writableRoots,
    ...overrides,
  })

export const boundaryProbes = (
  _root: string,
  catalogue: ProbeCatalogue,
): Promise<ReadonlyArray<ProbeResult>> => {
  const manifest = boundaryManifest()
  const extraMount: ObservedPosture = boundaryPosture({
    mounts: [...manifest.mounts, { path: "/host", access: "ro", class: "image" }],
  })
  const probes = [
    ["BOUNDARY-EXTRA-MOUNT", manifest, extraMount],
    [
      "BOUNDARY-EXTRA-SOCKET",
      Schema.decodeUnknownSync(SandboxManifestSchema)({
        ...manifest,
        channels: ["inbox", "vault", "todo"],
      }),
      boundaryPosture(),
    ],
    ["BOUNDARY-NIC", manifest, boundaryPosture({ addresses: ["lo", "eth0"] })],
    ["BOUNDARY-WEAK-POSTURE", manifest, boundaryPosture({ enforcement: "monty_wasm" })],
  ] as const
  return Promise.resolve(probes.map(([id, declared, posture]) => {
    const verdict = verifyStartup(declared, posture)
    const code = Result.isFailure(verdict) ? verdict.failure.code : "started"
    return result(byId(catalogue, id), `refused:${code}`, false, code)
  }))
}

export const channelProbes = async (
  root: string,
  catalogue: ProbeCatalogue,
): Promise<ReadonlyArray<ProbeResult>> => [
  ...await inboxProbes(root, catalogue),
  ...await vaultProbes(root, catalogue),
  ...await todoProbes(root, catalogue),
  ...await chatProbes(root, catalogue),
  ...await csrProbes(root, catalogue),
  ...await searchProbes(root, catalogue),
  ...await cspProbes(root, catalogue),
  ...await boundaryProbes(root, catalogue),
]
