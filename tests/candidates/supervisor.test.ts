import { assert, assertEquals } from "@std/assert"
import { Effect, Layer, Option } from "effect"
import { CandidateError, CandidateSupervisor } from "../../src/candidates/supervisor.ts"
import { rulingCommitmentOf } from "../../src/candidates/consent.ts"
import { PointerError, PointerStore, ScopePointer } from "../../src/candidates/pointer.ts"
import { pointerFileLayer } from "../../src/adapters/pointer-file.ts"
import { Ledger } from "../../src/kernel/ledger.ts"
import {
  approveFor,
  artifact,
  ConsentLog,
  inputs,
  movableClock,
  OWNER_ORIGIN,
  run,
  staged,
  workspace,
} from "./support.ts"
import { passingInputs } from "../../src/candidates/validation.ts"
import { withTempDir } from "../support.ts"

const failure = <A, E extends CandidateError>(
  directory: string,
  effect: Effect.Effect<A, E, CandidateSupervisor | Ledger | ConsentLog>,
  options = {},
): Promise<CandidateError> => run(directory, effect.pipe(Effect.flip), options)

const gestureFor = (
  request: {
    readonly display: {
      readonly consentDigest: string
      readonly manifest: { readonly candidateDigest: string }
      readonly tokenHash: string
    }
  },
  token: string,
  overrides: Record<string, unknown> = {},
) =>
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
      ...overrides,
    }
  })

export const wrapPointer = (
  root: string,
  wrap: (inner: PointerStore) => PointerStore,
): Layer.Layer<PointerStore> =>
  Layer.effect(
    PointerStore,
    Effect.gen(function* () {
      const inner = yield* PointerStore
      return wrap(inner)
    }),
  ).pipe(Layer.provide(pointerFileLayer(root)))

Deno.test("an executable candidate cannot activate without an approval receipt", async () => {
  await withTempDir("candidates", async (directory) => {
    const error = await failure(
      directory,
      Effect.gen(function* () {
        yield* staged()
        const supervisor = yield* CandidateSupervisor
        return yield* supervisor.activate("cand-1", Effect.succeed("pass" as const))
      }),
    )
    assertEquals(error.code, "approval_required")
  })
})

Deno.test("requestApproval returns the frozen render and never the plaintext token", async () => {
  await withTempDir("candidates", async (directory) => {
    const result = await run(
      directory,
      Effect.gen(function* () {
        yield* staged()
        const supervisor = yield* CandidateSupervisor
        const log = yield* ConsentLog
        const request = yield* supervisor.requestApproval("cand-1", {
          sessionId: "human-1",
          origin: OWNER_ORIGIN,
          verifiedAtUs: 0,
        }, 60_000_000)
        return { request, delivered: yield* log.tokens }
      }),
    )
    assert(!("token" in result.request), "the plaintext token must not be on the approval result")
    assertEquals(result.delivered.length, 1)
    assertEquals(/^kc_[0-9a-f]{64}$/u.test(result.delivered[0]?.token ?? ""), true)
    assertEquals(result.request.render.text.includes(result.delivered[0]?.token ?? "x"), false)
  })
})

Deno.test("two minted tokens differ: the token comes from the CSPRNG", async () => {
  await withTempDir("candidates", async (directory) => {
    const tokens = await run(
      directory,
      Effect.gen(function* () {
        yield* staged()
        const supervisor = yield* CandidateSupervisor
        const log = yield* ConsentLog
        yield* supervisor.requestApproval("cand-1", {
          sessionId: "human-1",
          origin: OWNER_ORIGIN,
          verifiedAtUs: 0,
        }, 60_000_000)
        yield* supervisor.requestApproval("cand-1", {
          sessionId: "human-1",
          origin: OWNER_ORIGIN,
          verifiedAtUs: 0,
        }, 60_000_000)
        return yield* log.tokens
      }),
    )
    assertEquals(tokens.length, 2)
    assertEquals(tokens[0]?.token === tokens[1]?.token, false)
    assertEquals(/^kc_[0-9a-f]{64}$/u.test(tokens[0]?.token ?? ""), true)
  })
})

Deno.test("an approval is single-use and bound to token, digest, session, and origin", async () => {
  await withTempDir("candidates", async (directory) => {
    const result = await run(
      directory,
      Effect.gen(function* () {
        yield* staged()
        const supervisor = yield* CandidateSupervisor
        const log = yield* ConsentLog
        const request = yield* supervisor.requestApproval("cand-1", {
          sessionId: "human-1",
          origin: OWNER_ORIGIN,
          verifiedAtUs: 0,
        }, 60_000_000)
        const delivered = yield* log.latest("human-1")
        const gesture = yield* gestureFor(request, delivered.token)
        const receipt = yield* supervisor.approve("cand-1", gesture)
        const replay = yield* supervisor.approve("cand-1", gesture).pipe(Effect.flip)
        const forgedToken = yield* supervisor.approve("cand-1", {
          ...gesture,
          token: `kc_${"0".repeat(64)}`,
        }).pipe(Effect.flip)
        const foreignOrigin = yield* supervisor.approve("cand-1", {
          ...gesture,
          origin: "https://evil.invalid",
        }).pipe(Effect.flip)
        return { request, receipt, replay, forgedToken, foreignOrigin }
      }),
    )
    assertEquals(
      result.request.render.text.includes(result.request.display.consentDigest),
      true,
    )
    assertEquals(result.receipt.ruling, "approve")
    assertEquals(result.receipt.tokenHash, result.request.display.tokenHash)
    assertEquals(result.receipt.origin, OWNER_ORIGIN)
    assertEquals(result.replay.code, "approval_already_consumed")
    assertEquals(result.forgedToken.code, "approval_token_invalid")
    assertEquals(result.foreignOrigin.code, "approval_session_unverified")
  })
})

Deno.test("a gesture with a broken ruling commitment mints no receipt", async () => {
  await withTempDir("candidates", async (directory) => {
    const error = await failure(
      directory,
      Effect.gen(function* () {
        yield* staged()
        const supervisor = yield* CandidateSupervisor
        const log = yield* ConsentLog
        const request = yield* supervisor.requestApproval("cand-1", {
          sessionId: "human-1",
          origin: OWNER_ORIGIN,
          verifiedAtUs: 0,
        }, 60_000_000)
        const delivered = yield* log.latest("human-1")
        const gesture = yield* gestureFor(request, delivered.token, {
          rulingCommitment: `sha256:${"1".repeat(64)}`,
        })
        return yield* supervisor.approve("cand-1", gesture)
      }),
    )
    assertEquals(error.code, "approval_digest_mismatch")
  })
})

Deno.test("an unverified session cannot request or rule on a consent", async () => {
  await withTempDir("candidates", async (directory) => {
    const result = await run(
      directory,
      Effect.gen(function* () {
        yield* staged()
        const supervisor = yield* CandidateSupervisor
        const request = yield* supervisor.requestApproval("cand-1", {
          sessionId: "agent-1",
          origin: OWNER_ORIGIN,
          verifiedAtUs: 0,
        }, 60_000_000).pipe(Effect.flip)
        const log = yield* ConsentLog
        const expired = yield* supervisor.requestApproval("cand-1", {
          sessionId: "human-1",
          origin: OWNER_ORIGIN,
          verifiedAtUs: 0,
        }, 0)
        const delivered = yield* log.latest("human-1")
        const gesture = yield* gestureFor(expired, delivered.token)
        const late = yield* supervisor.approve("cand-1", gesture).pipe(Effect.flip)
        return { request, late }
      }),
    )
    assertEquals(result.request.code, "approval_session_unverified")
    assertEquals(result.late.code, "approval_expired")
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
        const first = yield* supervisor.activate("cand-1", Effect.succeed("pass" as const))
        yield* staged(
          workspace({ candidateId: "cand-2", generation: 2, base: "cand-1" }),
          artifact("second bundle"),
        )
        yield* approveFor("human-1", "cand-2")
        const second = yield* supervisor.activate("cand-2", Effect.succeed("pass" as const))
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

Deno.test("the activation receipt is appended after the pointer flip", async () => {
  await withTempDir("candidates", async (directory) => {
    const result = await run(
      directory,
      Effect.gen(function* () {
        const supervisor = yield* CandidateSupervisor
        const ledger = yield* Ledger
        yield* staged()
        yield* approveFor("human-1")
        const activated = yield* supervisor.activate("cand-1", Effect.succeed("pass" as const))
        const pointer = Option.getOrThrow(activated.pointer)
        const records = yield* ledger.records
        return { activated, pointer, records }
      }),
    )
    const activation = result.records.find((record) =>
      record.components.some((component) => component.type === "kb.activation/candidate@1.0.0")
    )
    assert(activation !== undefined, "the activation receipt must exist")
    assertEquals(activation.idempotency_key, result.pointer.activationReceipt)
    assert(
      activation.recorded_at_us >= result.pointer.activatedAtUs,
      "the receipt must not exist before the flip",
    )
  })
})

Deno.test("a failed health probe restores the previous pointer and never activates", async () => {
  await withTempDir("candidates", async (directory) => {
    const result = await run(
      directory,
      Effect.gen(function* () {
        const supervisor = yield* CandidateSupervisor
        const ledger = yield* Ledger
        yield* staged()
        yield* approveFor("human-1")
        yield* supervisor.activate("cand-1", Effect.succeed("pass" as const))
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
        const failed = yield* supervisor.activate("cand-2", Effect.succeed("fail" as const))
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
    const activations = result.records.filter((record) =>
      record.components.some((component) =>
        component.type === "kb.activation/candidate@1.0.0" &&
        (component.payload as Record<string, unknown>)["candidateId"] === "cand-2"
      )
    )
    assertEquals(activations.length, 0)
    const failures = result.records.filter((record) =>
      record.components.some((component) =>
        component.type === "kb.activation/candidate_failure@1.0.0"
      )
    )
    assertEquals(failures.length, 1)
  })
})

Deno.test("a failed pointer flip leaves the old candidate live and records nothing", async () => {
  await withTempDir("candidates", async (directory) => {
    let publishes = 0
    const failing = (root: string) =>
      wrapPointer(root, (inner) => ({
        read: inner.read,
        clear: inner.clear,
        publish: (pointer: ScopePointer) => {
          publishes += 1
          return publishes === 1 ? inner.publish(pointer) : Effect.fail(
            new PointerError({
              operation: "publish",
              scope: pointer.scope,
              reason: "simulated flip failure",
            }),
          )
        },
      }))
    const result = await run(
      directory,
      Effect.gen(function* () {
        const supervisor = yield* CandidateSupervisor
        const ledger = yield* Ledger
        yield* staged()
        yield* approveFor("human-1")
        yield* supervisor.activate("cand-1", Effect.succeed("pass" as const))
        yield* staged(
          workspace({ candidateId: "cand-2", generation: 2, base: "cand-1" }),
          artifact("second bundle"),
        )
        yield* approveFor("human-1", "cand-2")
        const refused = yield* supervisor
          .activate("cand-2", Effect.succeed("pass" as const))
          .pipe(Effect.flip)
        const pointer = yield* supervisor.pointer("project/kanbei")
        const records = yield* ledger.records
        return { refused, pointer, records }
      }),
      { pointer: failing },
    )
    assertEquals(result.refused.code, "store_failed")
    assertEquals(Option.getOrThrow(result.pointer).candidateId, "cand-1")
    const activations = result.records.filter((record) =>
      record.components.some((component) =>
        component.type === "kb.activation/candidate@1.0.0" &&
        (component.payload as Record<string, unknown>)["candidateId"] === "cand-2"
      )
    )
    assertEquals(activations.length, 0)
  })
})

Deno.test("a declarative change requires validation and activates live without consent", async () => {
  await withTempDir("candidates", async (directory) => {
    const result = await run(
      directory,
      Effect.gen(function* () {
        const supervisor = yield* CandidateSupervisor
        const ledger = yield* Ledger
        yield* supervisor.stage(workspace({ kind: "declarative" }), artifact())
        const unvalidated = yield* supervisor
          .activate("cand-1", Effect.succeed("pass" as const))
          .pipe(Effect.flip)
        yield* supervisor.validate("cand-1", inputs)
        const activated = yield* supervisor.activate("cand-1", Effect.succeed("pass" as const))
        const records = yield* ledger.records
        return { unvalidated, activated, records }
      }),
    )
    assertEquals(result.unvalidated.code, "validation_incomplete")
    assertEquals(result.activated.state, "active")
    const consents = result.records.filter((record) =>
      record.components.some((component) => component.type === "kb.work/consent_display@1.0.0")
    )
    assertEquals(consents.length, 0)
  })
})

Deno.test("a declarative view document is validated before it goes live", async () => {
  await withTempDir("candidates", async (directory) => {
    const view = {
      schemaVersion: 1,
      viewId: "test-view",
      revision: "rev-1",
      route: "graph",
      title: "Test",
      sections: [{
        id: "queue",
        title: "Queue",
        rows: [{
          id: "row-1",
          anchor: "anchor:row-1",
          cells: [{ label: "subject", text: "hello" }],
          badges: [{ kind: "current", label: "current" }],
          actions: [
            { id: "action:rule", kind: "rule", label: "Rule", target: "route:graph" },
            {
              id: "action:consent",
              kind: "link-consent",
              label: "Consent",
              target: "route:decisions",
            },
          ],
          provenance: {
            observationId: "obs:1",
            span: "bytes 0-5",
            derivation: "capture",
            scope: "personal",
          },
        }],
      }],
      anchors: ["anchor:row-1"],
      consentLink: "consent:decision-1",
      decisionAffordance: "action:rule",
      disclosure: [
        { field: "provenance", mode: "include" },
        { field: "badges", mode: "include" },
        { field: "actions", mode: "include" },
        { field: "freshness", mode: "include" },
      ],
      freshness: {
        state: "current",
        observedAt: "2026-11-11T00:00:00.000Z",
        windowMs: 3_600_000,
        pending: 0,
        watermarkSeq: 1,
      },
    }
    const result = await run(
      directory,
      Effect.gen(function* () {
        const supervisor = yield* CandidateSupervisor
        const viewInputs = passingInputs({
          resolvedArtifacts: ["dep.a@1.0.0"],
          authorizedSources: ["obs:"],
        })
        yield* supervisor.stage(
          workspace({ kind: "declarative" }),
          new TextEncoder().encode(JSON.stringify(view)),
        )
        yield* supervisor.validate("cand-1", viewInputs)
        const live = yield* supervisor.activate("cand-1", Effect.succeed("pass" as const))
        yield* supervisor.stage(
          workspace({ candidateId: "cand-2", generation: 2, kind: "declarative" }),
          new TextEncoder().encode(
            JSON.stringify({ ...view, consentLink: "https://evil.invalid" }),
          ),
        )
        yield* supervisor.validate("cand-2", viewInputs)
        const refused = yield* supervisor
          .activate("cand-2", Effect.succeed("pass" as const))
          .pipe(Effect.flip)
        return { live, refused }
      }),
    )
    assertEquals(result.live.state, "active")
    assertEquals(result.refused.code, "view_invalid")
  })
})

Deno.test("a quarantined candidate cannot be laundered back into validated", async () => {
  await withTempDir("candidates", async (directory) => {
    const result = await run(
      directory,
      Effect.gen(function* () {
        const supervisor = yield* CandidateSupervisor
        yield* supervisor.stage(
          workspace({
            candidateId: "cand-root",
            capabilities: [{ capability: "trust_root.read", scope: "narrow" }],
          }),
          artifact(),
        )
        yield* supervisor.validate("cand-root", inputs)
        const quarantined = yield* supervisor.candidate("cand-root")
        const again = yield* supervisor.validate("cand-root", inputs).pipe(Effect.flip)
        const activate = yield* supervisor
          .activate("cand-root", Effect.succeed("pass" as const))
          .pipe(Effect.flip)
        return { quarantined, again, activate }
      }),
    )
    assertEquals(Option.getOrThrow(result.quarantined).state, "quarantined")
    assertEquals(result.again.code, "candidate_quarantined")
    assertEquals(result.activate.code, "candidate_quarantined")
  })
})

Deno.test("every validation step commits to the digest of the evidence it ruled on", async () => {
  await withTempDir("candidates", async (directory) => {
    const result = await run(
      directory,
      Effect.gen(function* () {
        const clean = yield* staged(
          workspace(),
          artifact(),
          passingInputs({ resolvedArtifacts: ["dep.a@1.0.0"] }),
        )
        yield* staged(
          workspace({ candidateId: "cand-2", generation: 2 }),
          artifact("second"),
          passingInputs({ resolvedArtifacts: ["dep.a@1.0.0"], securityProbes: true }),
        )
        return { clean }
      }),
    )
    const report = Option.getOrThrow(result.clean.report)
    assertEquals(report.steps.length, 8)
    for (const step of report.steps) {
      assertEquals(/^sha256:[0-9a-f]{64}$/u.test(step.evidenceDigest), true, step.step)
    }
    assertEquals(report.inputsDigest.startsWith("sha256:"), true)
    const digests = new Set(report.steps.map((step) => step.evidenceDigest))
    assertEquals(digests.size, report.steps.length)
  })
})

Deno.test("the frozen consent text names the added, removed, and broadened capabilities", async () => {
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
        const request = yield* supervisor.requestApproval("cand-next", {
          sessionId: "human-1",
          origin: OWNER_ORIGIN,
          verifiedAtUs: 0,
        }, 60_000_000)
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
    const text = result.request.render.text
    assertEquals(text.includes("channel.todo.write(narrow)"), true)
    assertEquals(text.includes("channel.inbox.read(broad)"), true)
    assertEquals(text.includes("dep.a@1.1.0"), true)
  })
})

Deno.test("rollback restores the predecessor, verifies the chain, and refuses a repeat", async () => {
  await withTempDir("candidates", async (directory) => {
    const result = await run(
      directory,
      Effect.gen(function* () {
        const supervisor = yield* CandidateSupervisor
        const ledger = yield* Ledger
        yield* staged()
        yield* approveFor("human-1")
        yield* supervisor.activate("cand-1", Effect.succeed("pass" as const))
        yield* staged(
          workspace({ candidateId: "cand-2", generation: 2, base: "cand-1" }),
          artifact("second bundle"),
        )
        yield* approveFor("human-1", "cand-2")
        yield* supervisor.activate("cand-2", Effect.succeed("pass" as const))
        const rolled = yield* supervisor.rollback("project/kanbei", "regression")
        const pointer = yield* supervisor.pointer("project/kanbei")
        const again = yield* supervisor.rollback("project/kanbei", "regression").pipe(
          Effect.flip,
        )
        const records = yield* ledger.records
        return { rolled, pointer, again, records }
      }),
    )
    assertEquals(Option.getOrThrow(result.pointer).candidateId, "cand-1")
    assertEquals(result.again.code, "no_rollback_target")
    const rollbacks = result.records.filter((record) =>
      record.components.some((component) =>
        component.type === "kb.activation/candidate_rollback@1.0.0"
      )
    )
    assertEquals(rollbacks.length, 1)
    assertEquals(result.rolled.receipt, rollbacks[0]?.record_id)
  })
})

Deno.test("rollback re-checks the approval of an executable target", async () => {
  await withTempDir("candidates", async (directory) => {
    const clock = movableClock(Date.parse("2026-11-11T00:00:00.000Z"))
    const result = await run(
      directory,
      Effect.gen(function* () {
        const supervisor = yield* CandidateSupervisor
        yield* staged()
        const log = yield* ConsentLog
        const request = yield* supervisor.requestApproval("cand-1", {
          sessionId: "human-1",
          origin: OWNER_ORIGIN,
          verifiedAtUs: 0,
        }, 1_000_000)
        const delivered = yield* log.latest("human-1")
        const gesture = yield* gestureFor(request, delivered.token)
        yield* supervisor.approve("cand-1", gesture)
        yield* supervisor.activate("cand-1", Effect.succeed("pass" as const))
        yield* staged(
          workspace({ candidateId: "cand-2", generation: 2, base: "cand-1" }),
          artifact("second bundle"),
        )
        yield* approveFor("human-1", "cand-2")
        yield* supervisor.activate("cand-2", Effect.succeed("pass" as const))
        clock.advance(5_000)
        return yield* supervisor.rollback("project/kanbei", "late").pipe(Effect.flip)
      }),
      { clock: clock.clock },
    )
    assertEquals(result.code, "approval_expired")
  })
})

const tampered = (root: string) =>
  wrapPointer(root, (inner) => ({
    publish: inner.publish,
    clear: inner.clear,
    read: (scope) =>
      inner.read(scope).pipe(
        Effect.map((pointer) =>
          Option.map(pointer, (value) => ({
            ...value,
            ...(value.prev === undefined ? {} : {
              prev: { ...value.prev, candidateId: "cand-forged" },
            }),
          }))
        ),
      ),
  }))

Deno.test("a forged pointer predecessor disagrees with the activation chain", async () => {
  await withTempDir("candidates", async (directory) => {
    const error = await failure(
      directory,
      Effect.gen(function* () {
        const supervisor = yield* CandidateSupervisor
        yield* staged()
        yield* approveFor("human-1")
        yield* supervisor.activate("cand-1", Effect.succeed("pass" as const))
        yield* staged(
          workspace({ candidateId: "cand-2", generation: 2, base: "cand-1" }),
          artifact("second bundle"),
        )
        yield* approveFor("human-1", "cand-2")
        yield* supervisor.activate("cand-2", Effect.succeed("pass" as const))
        return yield* supervisor.rollback("project/kanbei", "forged")
      }),
      { pointer: tampered },
    )
    assertEquals(error.code, "pointer_corrupt")
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
