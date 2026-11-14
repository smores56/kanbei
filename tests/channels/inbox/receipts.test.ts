import { assertEquals } from "@std/assert"
import { Effect, Layer } from "effect"
import { fileLedgerStoreLayer } from "../../../src/adapters/ledger-file.ts"
import { Ledger, ledgerLayer } from "../../../src/kernel/ledger.ts"
import { appendBatchReceipt } from "../../../src/channels/inbox/receipts.ts"
import { withTempDir } from "../../support.ts"

const receipt = (receivedAt: string) => ({
  receipt: {
    origin: "finance" as const,
    batchDigest: "sha256:" + "0".repeat(64),
    sequence: 1,
    receivedAt,
    entries: 1,
    publishedBytes: 10,
    coverage: { complete: true, gaps: 0, suppressed: 0 },
  },
  heartbeat: {
    origin: "finance" as const,
    kind: "batch" as const,
    sequence: 1,
    producedAt: "2026-11-11T00:00:00Z",
    receivedAt,
    coverage: { complete: true, gaps: 0, suppressed: 0 },
  },
  gaps: [],
})

const run = <A, E>(
  directory: string,
  effect: Effect.Effect<A, E, Ledger>,
): Promise<A> =>
  Effect.runPromise(
    effect.pipe(
      Effect.provide(
        ledgerLayer("receipts-test").pipe(
          Layer.provide(fileLedgerStoreLayer(`${directory}/ledger.jsonl`)),
        ),
      ),
    ),
  )

Deno.test("a receipt stored under an older receivedAt is a duplicate, not a crash", async () => {
  await withTempDir("receipt-legacy", async (directory) => {
    const first = await run(directory, appendBatchReceipt(receipt("2026-11-11T00:00:01Z")))
    const replay = await run(directory, appendBatchReceipt(receipt("2026-11-11T00:00:02Z")))
    assertEquals(first, false)
    assertEquals(replay, true)
  })
})
