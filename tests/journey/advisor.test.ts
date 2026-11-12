import { assertEquals } from "@std/assert"
import { agreement, deriveAdvice } from "../../src/journey/advisor.ts"
import { appendMemoryEvents, readMemory } from "../../src/journey/memory.ts"
import { Effect, Layer } from "effect"
import { fileLedgerStoreLayer } from "../../src/adapters/ledger-file.ts"
import { Ledger, ledgerLayer } from "../../src/kernel/ledger.ts"
import { withTempDir } from "../support.ts"

const startIso = "2026-11-11T00:00:00.000Z"

Deno.test("advice derives deadlines and renewals from mail, calendar, finance, and phone only", () => {
  const advice = deriveAdvice([
    {
      origin: "tuta-mail",
      resourceKey: "message/1",
      text: '{"subject":"Lease renewal","renewalDate":"2026-12-01"}',
      state: "current",
    },
    {
      origin: "tuta-calendar",
      resourceKey: "event/1",
      text: '{"summary":"Dentist","start":"2026-11-12T09:00:00Z"}',
      state: "current",
    },
    {
      origin: "phone",
      resourceKey: "sms/1",
      text: '{"text":"filter change by 2026-11-25"}',
      state: "current",
    },
    {
      origin: "immich",
      resourceKey: "asset/1",
      text: '{"assetId":"a","takenAt":"2026-11-09T12:00:00Z"}',
      state: "current",
    },
  ], { startIso, horizonDays: 60 })
  assertEquals(advice.map((entry) => entry.key), [
    "tuta-calendar|event/1|deadline",
    "phone|sms/1|deadline",
    "tuta-mail|message/1|renewal",
  ])
  assertEquals(advice.find((entry) => entry.origin === "tuta-mail")?.dueAt, "2026-12-01")
})

Deno.test("advice outside the horizon and non-JSON bodies produce nothing", () => {
  const advice = deriveAdvice([
    {
      origin: "tuta-mail",
      resourceKey: "message/1",
      text: '{"subject":"Lease renewal","renewalDate":"2028-01-01"}',
      state: "current",
    },
    { origin: "tuta-mail", resourceKey: "message/2", text: "not json", state: "current" },
  ], { startIso, horizonDays: 60 })
  assertEquals(advice, [])
})

Deno.test("a stale source keeps its advice but never as current", () => {
  const advice = deriveAdvice([
    {
      origin: "finance",
      resourceKey: "transaction/1",
      text: '{"merchant":"Grocer","dueDate":"2026-11-20"}',
      state: "stale",
    },
  ], { startIso, horizonDays: 60 })
  assertEquals(advice.length, 1)
  assertEquals(advice[0]?.state, "stale")
  assertEquals(advice.filter((entry) => entry.state === "current").length, 0)
})

Deno.test("agreement counts exact matches and reports precision and recall", () => {
  const result = agreement(new Set(["a", "b"]), new Set(["b", "c"]))
  assertEquals(result.matched, 1)
  assertEquals(result.precision, 0.5)
  assertEquals(result.recall, 0.5)
})

Deno.test("memory reconciliation marks contradictions contested and retractions suspect", async () => {
  await withTempDir("journey-memory", async (directory) => {
    const records = await Effect.runPromise(
      Effect.gen(function* () {
        yield* appendMemoryEvents(
          [
            {
              claimId: "one",
              subject: "cadence",
              value: "annual",
              confidence: 0.5,
              source: "tuta-mail|message/1",
            },
            {
              claimId: "two",
              subject: "cadence",
              value: "monthly",
              confidence: 0.5,
              source: "phone|sms/1",
            },
            {
              claimId: "three",
              subject: "provider",
              value: "acme",
              confidence: 0.5,
              source: "tuta-mail|message/2",
            },
            {
              claimId: "four",
              subject: "landlord",
              value: "old",
              confidence: 0.5,
              source: "tuta-mail|message/1",
            },
          ],
          [{ targetClaimId: "four", reason: "moved" }],
        )
        const ledger = yield* Ledger
        return yield* ledger.records
      }).pipe(
        Effect.provide(
          ledgerLayer("test").pipe(
            Layer.provide(fileLedgerStoreLayer(`${directory}/ledger.jsonl`)),
          ),
        ),
      ),
    )
    const memory = readMemory(records)
    assertEquals(memory.map((claim) => `${claim.claimId}=${claim.state}`), [
      "four=suspect",
      "one=contested",
      "three=active",
      "two=contested",
    ])
  })
})
