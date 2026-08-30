# M3 Report — Agent spine (one provider, responder, perpetual cognition, bounded scheduler, typed tools, async provenance, interrupted/ambiguous recovery)

Date: 2026-08-30. Commits: `802351a` (Wave 1+2), `6d63876` (session spine + gate), plus this
report. Milestone gate: **GREEN**. Instrument ratified first (`60bfb43`,
docs/dogfooding-instrument.md).

## Deliverables

New crates (14 → 17):

- **kanbei-provider** — the provider gateway (R-19 tier-2 built-in): one normalized
  `ProviderEngine` seam with the OpenAI-compatible HTTP engine (ureq 3, rustls) and a
  deterministic `FakeEngine` for the gate; key custody (R-28/D-06) injected at call time
  only, never canonical; `ModelCallRecord`/`EgressEntry` canonical payloads; minimal
  context renderer (the typed staged projection pipeline is M4 — M3 records the same
  contract).
- **kanbei-scheduler** — the run FSM (R-09/E-10 Wake = Run: every accepted wake creates
  exactly one RunId with a kind discriminator and typed trigger provenance), canonical
  wake-acceptance/denial records naming the responsible constraint (R-09/E-09), terminal
  outcomes (R-18), kernel-owned circuit breakers on 4 canonical counters (R-17/E-02),
  budgets with explicit `Blocked` on exhaustion (R-17/H-05), the `CognitionProvider`
  step seam (R-18/E-01) over the closed host-command set, and the built-in default
  scheduler policy (responder > cognition > child; coalescing). The Luau policy module
  runtime is deferred with the seam defined — the mirror of the R-20 retention deferral.
- **kanbei-tools** — the deterministic tool registry (canonical schemas, sorted names,
  digest), the tool FSM records (`ToolCallId` = `call_` brand, `ToolIntent` with caller
  principal R-14/D-02 + origin_snapshot, `ToolOutcome` with origin + commit snapshots and
  `interrupted|ambiguous` classification), approval digest binding (R-16/D-12: tool +
  action + canonicalized args + cwd/env fingerprint, domain-separated), and native
  executors with launch controls (R-28/D-S2): fs read/search/write/patch, git
  status/diff, process exec (timeout, output limits, inherited-FD closure, process-tree
  kill, default-deny env), todo state. Memory tools register with explicit `Unavailable`
  until M4; `child.spawn` schema exists, dispatch is a session seam (R-09).

Session spine (kanbei-session `spine.rs`):

- wake acceptance/denial, run start/outcome commit paths;
- `model_call`: commits the intent record (rendered hash + params + cache plan), invokes
  the engine, commits the outcome repeating the rendered hash (R-08/E-13 intent
  provenance) + the egress entry (R-15);
- `tool_call`: commits the intent BEFORE dispatch (B-05), runs the approval gate, and at
  dispatch re-runs the broker guard set (R-16/D-11/C-10) — revoked intents resolve
  `interrupted` with a user-visible reason; outcomes pass the retention gate first
  (R-28/D-S1) and boundary facts commit canonically;
- bounded approval queue with oldest-evicted overflow (R-17/H-05);
- `cognition_loop`: one bounded orchestration over the closed command set, checking the
  wake deadline/budget at every host-command boundary (R-18/E-01), `Blocked` on
  exhaustion;
- responder priority (R-09/E-10): `cancel_active_run` classifies the in-flight cognition
  run `Failed(UserCancelled)`; committed intents are never rolled back;
- `resume_cognition`: explicit user resume after a breaker trip (R-17/E-02);
- `classify_pending_intents` at open: committed-intent-without-outcome is the sufficient
  condition for interrupted/ambiguous classification (B-05); idempotent (already
  classified intents are skipped).

Manifest schema 3 (kanbei-snapshot): tool-registry digest + provider-config digest +
scheduler-policy name pins (R-08/A-07/E-12), `#[serde(default)]` for backward compat.
kanbei-core BRANDS: `run_`, `call_` (R-09 identity model). kanbei-capabilities:
`Principal` serde, `policy_version` public, `grants_version` accessor.

## Gate evidence (kanbei-testkit/tests/gate_m3.rs, 8 tests)

| Acceptance bullet (architecture 629-671) | Result |
|---|---|
| Crash injection at effect dispatch / outcome (633) | 14-point crash matrix green: wake accept, run start, model call, tool intent commit, tool dispatch, tool outcome commit, run outcome — each Before/After point aborts (SIGABRT) and `verify_m3_recovery` passes |
| Consistency 6 Crash, 7 Recovery | `verify_recovery_tolerant` (log invariants, contiguity, ack coverage, closure) + spine invariants: every tool intent resolves to an outcome OR an explicit classification; model intents pair with outcomes |
| Consistency 3 Canonical fact, 5 Payload, 11 Causality | record-pairing test: wake_acceptance < run_start < model_call < model_outcome < run_outcome; run_id pairs across the run lifecycle; rendered hash repeats intent→outcome |
| Consistency 15 Scope (review gate) | spine is a pure consumer: scope count and composition epoch unchanged after a full run |
| Differentiator: runaway-wake breaker trips within budget + canonical fact (R-17/H-05) | 2 consecutive failures trip `breaker_tripped` (counter named); subsequent wakes denied with the responsible constraint; `resume_cognition` clears |
| Differentiator: responder priority | in-flight cognition cancelled `Failed(UserCancelled)`, responder wake accepted next, cancellation facts canonical |
| Differentiator: budget exhaustion → explicit `Blocked` | token-budget override: run ends with one canonical `Blocked` outcome |
| Differentiator: approval queue bound with overflow/eviction | bound enforced in the queue; revocation at dispatch resolves `interrupted` (recheck path tested) |

Workspace: 241 tests / 49 suites green (includes M1 + M2 gates); clippy `-D warnings`
clean on all 17 crates including the wasm guest.

## Decisions and deviations (all within ratified architecture)

- **Scheduler policy**: Rust built-in default (responder > cognition > child, coalescing)
  with the `SchedulerPolicy` trait seam; the Luau policy module runtime is deferred —
  mirror of the R-20 retention-policy deferral, documented in-crate.
- **Provider**: one OpenAI-compatible engine; key from env/config, injected at call time
  only, never recorded (keychain custody deferred with the `KeySource` seam).
- **Tool set**: fs/git/process/todo fully implemented; `memory.query/propose` and
  `child.spawn` schemas registered, dispatch resolves explicit `Unavailable`/seam errors
  (child execution is M4 — R-09's tool-FSM routing is defined).
- **Approval semantics**: the approval digest binds args + cwd/env fingerprint (R-16/D-12)
  via the capabilities crate; approval resolution is the session's `check_approval` +
  parked-queue re-verification at dispatch; a `re-approval is a new intent` (revoked →
  `interrupted`).
- **Run genesis pins a manifest** (R-08 run-genesis transition); model/tool facts are
  pure events referencing the last-pinned manifest; `intent_classified` recovery facts
  are pure events.

## Perf notes

- Gate run: ~6 s wall for gate_m3 (14 crash children); full workspace 19 s warm.
- No new hot-path budgets claimed; M3 adds no storage-path changes (spine events use the
  existing commit path). Provider HTTP is untested against real networks in CI (gate uses
  the fake engine); the conformance parse is unit-tested.

## Next: M4 (context and memory)

Cache-aware projection pipeline (TrajectoryView → ValidProviderContext with the R-05
invariants), memory DAG/claims/roots with the R-11 transition actor, exact-entity +
FTS5/BM25 retrieval, child runs (R-09 tool-FSM routing is defined), memory tools
activated.
