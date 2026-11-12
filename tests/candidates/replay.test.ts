import { assert, assertEquals } from "@std/assert"
import { Effect, Result } from "effect"
import type { CandidateApprovalReceipt, ConsentGesture } from "../../src/candidates/consent.ts"
import { CandidateError, CandidateSupervisor } from "../../src/candidates/supervisor.ts"
import { rulingCommitmentOf } from "../../src/candidates/consent.ts"
import { Ledger } from "../../src/kernel/ledger.ts"
import { ConsentLog, OWNER_ORIGIN, run, staged } from "./support.ts"
import { withTempDir } from "../support.ts"

const APPROVAL_COMPONENT = "kb.work/candidate_approval@1.0.0"

const gestureFor = (
  request: {
    readonly display: {
      readonly consentDigest: string
      readonly manifest: { readonly candidateDigest: string }
      readonly tokenHash: string
    }
  },
  token: string,
): Effect.Effect<ConsentGesture> =>
  Effect.gen(function* () {
    return {
      token,
      consentDigest: request.display.consentDigest,
      candidateDigest: request.display.manifest.candidateDigest,
      sessionId: "human-1",
      origin: OWNER_ORIGIN,
      ruling: "approve" as const,
      rulingCommitment: yield* rulingCommitmentOf(
        request.display.consentDigest,
        "approve",
        request.display.tokenHash,
      ),
    }
  })

Deno.test("concurrent replays of one approval token mint exactly one receipt", async () => {
  await withTempDir("candidates-replay", async (directory) => {
    const outcome = await run(
      directory,
      Effect.gen(function* () {
        yield* staged()
        const supervisor = yield* CandidateSupervisor
        const ledger = yield* Ledger
        const log = yield* ConsentLog
        const request = yield* supervisor.requestApproval("cand-1", {
          sessionId: "human-1",
          origin: OWNER_ORIGIN,
          verifiedAtUs: 0,
        }, 60_000_000)
        const delivered = yield* log.latest("human-1")
        const gesture = yield* gestureFor(request, delivered.token)
        const attempts = yield* Effect.all(
          [
            Effect.result(supervisor.approve("cand-1", gesture)),
            Effect.result(supervisor.approve("cand-1", gesture)),
          ],
          { concurrency: 2 },
        )
        const records = yield* ledger.records
        return { attempts, records }
      }),
    )
    const approvals = outcome.records.filter((record) =>
      record.components.some((component) => component.type === APPROVAL_COMPONENT)
    )
    assertEquals(approvals.length, 1)
    const succeeded = outcome.attempts.filter(Result.isSuccess)
    assertEquals(succeeded.length, 1)
    const failed = outcome.attempts.flatMap((attempt) =>
      Result.isFailure(attempt) ? [attempt.failure] : []
    )
    assertEquals(failed.length, 1)
    assert(failed[0] instanceof CandidateError)
    assertEquals(failed[0].code, "approval_already_consumed")
    const receipt: CandidateApprovalReceipt | undefined = succeeded[0]?.success
    assert(receipt !== undefined)
    assertEquals(receipt.tokenHash.length > 0, true)
  })
})
