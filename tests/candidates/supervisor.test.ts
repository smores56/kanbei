import { assertEquals } from "@std/assert"
import { Effect, Option } from "effect"
import { CandidateError, CandidateSupervisor } from "../../src/candidates/supervisor.ts"
import { Ledger } from "../../src/kernel/ledger.ts"
import { artifact, inputs, run, staged, workspace } from "./support.ts"
import { passingInputs } from "../../src/candidates/validation.ts"
import { withTempDir } from "../support.ts"

const failure = <A, E extends CandidateError>(
  directory: string,
  effect: Effect.Effect<A, E, CandidateSupervisor>,
): Promise<CandidateError> => run(directory, effect.pipe(Effect.flip))

const approveFor = (sessionId: string, candidateId = "cand-1") =>
  Effect.gen(function* () {
    const supervisor = yield* CandidateSupervisor
    const request = yield* supervisor.requestApproval(candidateId, sessionId, 60_000_000)
    const receipt = yield* supervisor.approve(candidateId, {
      token: request.token,
      consentDigest: request.display.consentDigest,
      candidateDigest: request.display.manifest.candidateDigest,
      sessionId,
    })
    return { request, receipt }
  })

Deno.test("an executable candidate cannot activate without an approval receipt", async () => {
  await withTempDir("candidates", async (directory) => {
    const error = await failure(
      directory,
      Effect.gen(function* () {
        yield* staged()
        const supervisor = yield* CandidateSupervisor
        return yield* supervisor.activate("cand-1", "pass")
      }),
    )
    assertEquals(error.code, "approval_required")
  })
})

Deno.test("an approval is single-use and bound to the rendered consent digest", async () => {
  await withTempDir("candidates", async (directory) => {
    const result = await run(
      directory,
      Effect.gen(function* () {
        yield* staged()
        const supervisor = yield* CandidateSupervisor
        const request = yield* supervisor.requestApproval("cand-1", "human-1", 60_000_000)
        const gesture = {
          token: request.token,
          consentDigest: request.display.consentDigest,
          candidateDigest: request.display.manifest.candidateDigest,
          sessionId: "human-1",
        }
        const receipt = yield* supervisor.approve("cand-1", gesture)
        const replay = yield* supervisor.approve("cand-1", gesture).pipe(Effect.flip)
        return { request, receipt, replay }
      }),
    )
    assertEquals(
      result.request.render.text.includes(result.request.display.consentDigest),
      true,
    )
    assertEquals(
      result.request.render.lines.some((line) => line.startsWith("expires ")),
      true,
    )
    assertEquals(result.receipt.ruling, "approve")
    assertEquals(result.receipt.candidateDigest, result.request.display.manifest.candidateDigest)
    assertEquals(result.replay.code, "approval_already_consumed")
  })
})

Deno.test("a consent digest or session mismatch mints no receipt", async () => {
  await withTempDir("candidates", async (directory) => {
    const result = await run(
      directory,
      Effect.gen(function* () {
        yield* staged()
        const supervisor = yield* CandidateSupervisor
        const request = yield* supervisor.requestApproval("cand-1", "human-1", 60_000_000)
        const wrongDigest = yield* supervisor.approve("cand-1", {
          token: request.token,
          consentDigest: `sha256:${"0".repeat(64)}`,
          candidateDigest: request.display.manifest.candidateDigest,
          sessionId: "human-1",
        }).pipe(Effect.flip)
        const wrongSession = yield* supervisor.approve("cand-1", {
          token: request.token,
          consentDigest: request.display.consentDigest,
          candidateDigest: request.display.manifest.candidateDigest,
          sessionId: "human-2",
        }).pipe(Effect.flip)
        return { wrongDigest, wrongSession }
      }),
    )
    assertEquals(result.wrongDigest.code, "approval_digest_mismatch")
    assertEquals(result.wrongSession.code, "approval_session_mismatch")
  })
})

Deno.test("an expired consent display mints no receipt", async () => {
  await withTempDir("candidates", async (directory) => {
    const error = await failure(
      directory,
      Effect.gen(function* () {
        yield* staged()
        const supervisor = yield* CandidateSupervisor
        const request = yield* supervisor.requestApproval("cand-1", "human-1", 0)
        return yield* supervisor.approve("cand-1", {
          token: request.token,
          consentDigest: request.display.consentDigest,
          candidateDigest: request.display.manifest.candidateDigest,
          sessionId: "human-1",
        })
      }),
    )
    assertEquals(error.code, "approval_expired")
  })
})

Deno.test("approval activates by flipping exactly one scope pointer", async () => {
  await withTempDir("candidates", async (directory) => {
    const result = await run(
      directory,
      Effect.gen(function* () {
        const supervisor = yield* CandidateSupervisor
        yield* staged()
        yield* approveFor("human-1")
        const first = yield* supervisor.activate("cand-1", "pass")
        yield* staged(
          workspace({ candidateId: "cand-2", generation: 2, base: "cand-1" }),
          artifact("second bundle"),
        )
        yield* approveFor("human-1", "cand-2")
        const second = yield* supervisor.activate("cand-2", "pass")
        const pointer = yield* supervisor.pointer("project/kanbei")
        return { first, second, pointer }
      }),
    )
    assertEquals(Option.isSome(result.first.pointer), true)
    assertEquals(Option.getOrThrow(result.second.pointer).candidateId, "cand-2")
    assertEquals(Option.getOrThrow(result.second.pointer).generation, 2)
    assertEquals(Option.getOrThrow(result.pointer).prev?.candidateId, "cand-1")
    const files = [...Deno.readDirSync(`${directory}/pointers`)].filter((entry) =>
      entry.name.endsWith(".json")
    )
    assertEquals(files.length, 1)
  })
})

Deno.test("a failed health check restores the previous candidate and keeps external effects", async () => {
  await withTempDir("candidates", async (directory) => {
    const result = await run(
      directory,
      Effect.gen(function* () {
        const supervisor = yield* CandidateSupervisor
        const ledger = yield* Ledger
        yield* staged()
        yield* approveFor("human-1")
        yield* supervisor.activate("cand-1", "pass")
        yield* staged(
          workspace({ candidateId: "cand-2", generation: 2, base: "cand-1" }),
          artifact("second bundle"),
        )
        yield* approveFor("human-1", "cand-2")
        yield* ledger.append({
          components: [{
            type: "kb.effect/receipt@1.0.0",
            payload: { candidateId: "cand-2", effect: "write-todo" },
          }],
          appender: "frozen",
          principal: "effect-journal",
          authorityClass: "effect_receipt",
        })
        const failed = yield* supervisor.activate("cand-2", "fail")
        const pointer = yield* supervisor.pointer("project/kanbei")
        const candidate = yield* supervisor.candidate("cand-2")
        const records = yield* ledger.records
        return { failed, pointer, candidate, records }
      }),
    )
    assertEquals(Option.getOrThrow(result.pointer).candidateId, "cand-1")
    assertEquals(result.failed.state, "failed_health")
    assertEquals(Option.getOrThrow(result.candidate).state, "failed_health")
    const effects = result.records.filter((record) =>
      record.components.some((component) => component.type === "kb.effect/receipt@1.0.0")
    )
    assertEquals(effects.length, 1)
    const failures = result.records.filter((record) =>
      record.components.some((component) =>
        component.type === "kb.activation/candidate_failure@1.0.0"
      )
    )
    assertEquals(failures.length, 1)
  })
})

Deno.test("a declarative change activates live without a consent receipt", async () => {
  await withTempDir("candidates", async (directory) => {
    const result = await run(
      directory,
      Effect.gen(function* () {
        const supervisor = yield* CandidateSupervisor
        const ledger = yield* Ledger
        yield* staged(workspace({ kind: "declarative" }))
        const activated = yield* supervisor.activate("cand-1", "pass")
        const records = yield* ledger.records
        return { activated, records }
      }),
    )
    assertEquals(result.activated.state, "active")
    const consents = result.records.filter((record) =>
      record.components.some((component) => component.type === "kb.work/consent_display@1.0.0")
    )
    assertEquals(consents.length, 0)
  })
})

Deno.test("a trust-root or undeclared capability is rejected and quarantined", async () => {
  await withTempDir("candidates", async (directory) => {
    const result = await run(
      directory,
      Effect.gen(function* () {
        const supervisor = yield* CandidateSupervisor
        const trustRoot = yield* supervisor.stage(
          workspace({
            candidateId: "cand-root",
            capabilities: [{ capability: "trust_root.read", scope: "narrow" }],
          }),
          artifact(),
        )
        yield* supervisor.validate(trustRoot.manifest.candidateId, inputs)
        const undeclared = yield* supervisor.stage(
          workspace({
            candidateId: "cand-unknown",
            capabilities: [{ capability: "channel.evil", scope: "narrow" }],
          }),
          artifact(),
        )
        yield* supervisor.validate(undeclared.manifest.candidateId, inputs)
        const rootRecord = yield* supervisor.candidate("cand-root")
        const unknownRecord = yield* supervisor.candidate("cand-unknown")
        const refused = yield* supervisor.activate("cand-root", "pass").pipe(Effect.flip)
        return { rootRecord, unknownRecord, refused }
      }),
    )
    assertEquals(Option.getOrThrow(result.rootRecord).state, "quarantined")
    assertEquals(Option.getOrThrow(result.unknownRecord).state, "quarantined")
    assertEquals(result.refused.code, "candidate_quarantined")
  })
})

Deno.test("the capability diff shows added, removed, broadened, and dependency changes", async () => {
  await withTempDir("candidates", async (directory) => {
    const result = await run(
      directory,
      Effect.gen(function* () {
        const supervisor = yield* CandidateSupervisor
        yield* staged(workspace({ candidateId: "cand-base" }))
        yield* staged(
          workspace({
            candidateId: "cand-next",
            generation: 2,
            base: "cand-base",
            capabilities: [
              { capability: "channel.inbox.read", scope: "broad" },
              { capability: "channel.todo.write", scope: "narrow" },
            ],
            dependencies: [{ id: "dep.a", version: "1.1.0" }],
          }),
          artifact("next bundle"),
          passingInputs({ resolvedArtifacts: ["dep.a@1.0.0", "dep.a@1.1.0"] }),
        )
        const record = yield* supervisor.candidate("cand-next")
        const request = yield* supervisor.requestApproval("cand-next", "human-1", 60_000_000)
        return { record, request }
      }),
    )
    const record = Option.getOrThrow(result.record)
    const diff = Option.getOrThrow(record.diff)
    assertEquals(diff.added.map((entry) => entry.capability), ["channel.todo.write"])
    assertEquals(diff.removed, [])
    assertEquals(diff.changed.length, 1)
    assertEquals(diff.changed[0]?.broadening, true)
    assertEquals(diff.dependencyChanges.length, 1)
    assertEquals(diff.dependencyChanges[0]?.compatible, true)
    assertEquals(diff.trustRootAccess, false)
    assertEquals(
      result.request.render.text.includes(Option.getOrThrow(record.diffDigest)),
      true,
    )
  })
})

Deno.test("staged candidates and approvals survive a restart", async () => {
  await withTempDir("candidates", async (directory) => {
    await run(
      directory,
      Effect.gen(function* () {
        yield* staged()
        yield* approveFor("human-1")
      }),
    )
    const restarted = await run(
      directory,
      Effect.gen(function* () {
        const supervisor = yield* CandidateSupervisor
        return {
          candidate: yield* supervisor.candidate("cand-1"),
          candidates: yield* supervisor.candidates,
          pointer: yield* supervisor.pointer("project/kanbei"),
        }
      }),
    )
    const candidate = Option.getOrThrow(restarted.candidate)
    assertEquals(candidate.state, "approved")
    assertEquals(candidate.manifest.candidateId, "cand-1")
    assertEquals(restarted.candidates.length, 1)
    assertEquals(Option.isNone(restarted.pointer), true)
  })
})
