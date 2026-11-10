## Plan

### 0. Plan conventions

Requirements are authoritative. This plan orders implementation work without assuming any existing target code, repository layout, or runtime selection. Runtime, execution language, deployment plane, isolation mechanism, and numeric thresholds remain decision-gated.

**Task completion contract.** Every task, including spikes, closes only when all of the following are attached:

1. **Validation:** the task-specific checks run through the conformance harness and emit an `EvidenceEnvelope` as defined by `07 §7.0`, bound to canonical head, fixture version, environment fingerprint, seeds, and post-redaction model recordings. Zero-tolerance checks never retry.
2. **Review/fix:** an independent reviewer checks scope against `00-requirements.md` and the normative validation section `07 §7`, confirms evidence sufficiency, and records findings. Defects are fixed and the affected checks are rerun before the task closes.
3. **No silent pass:** `inconclusive` or `blocked` does not become a default choice and does not unblock dependent tasks.

**Validation identifiers.** `07 §7` is the sole normative source for every validation identifier, fixture, protocol, pass criterion, and for `EvidenceEnvelope`; this plan defines none. `spec/06-validation-plan.md` is a superseded early draft and is a source for nothing in this plan.

| Identifier | Normative definition |
|---|---|
| `EvidenceEnvelope`, `Outcome`, test classes, spike pre-registration | `07 §7.0` |
| `ZT-01`–`ZT-14` zero-tolerance gates | `07 §7.1` |
| `I-01`–`I-19` canonical invariants | `07 §7.2` |
| `V1`–`V22` mechanical checks | `07 §7.2` |
| `AS-01`–`AS-12` acceptance scenarios | `07 §7.3` |
| `C-01`–`C-14` concurrency, approval, and override tests | `07 §7.3` |
| `R-01`–`R-07` replay, undo, and crash tests | `07 §7.3` |
| `P-01`–`P-10` poisoning, retraction, and quarantine tests | `07 §7.3` |
| `B-01`–`B-12` CAS, backup, restore, and erasure tests | `07 §7.3` |
| `U-01`–`U-10` UI structural tests | `07 §7.3` |
| `journey-v1`, `ui-v1` sealed fixtures | `07 §7.3` |
| `S1`–`S6` spike protocol aliases (mapped below) | `07 §7.4` |
| `LANG-1`, `NODE-DENO-1`, `DEP-1`, `ISO-1`, `LIM-1`, `UI-1`, `UI-2`, `ING-1` spike protocols | `07 §7.4` |

Spike aliases used by this plan follow `07 §7.4` exactly: `S1` = `LANG-1` + `NODE-DENO-1` (`A1`, `A4`), `S2` = `DEP-1` (`A2`), `S3` = `ISO-1` (`A3`), `S4` = `LIM-1` (`A6`, `H4`), `S5` = reconciliation and poisoning measurement (`C9`, `H2`), `S6` = interface comprehension (`H6`). `B5`'s synchronous-versus-asynchronous index-membership measurement has no `07 §7.4` protocol and is a plan-local check: it cites `07 §7.2` `I-16` and `07 §7.3` `C-05`, `R-01`, `B-08`, `ZT-12`, is decided by independent review, and closes `DG-5` without a spike alias. Its pre-registered latency budget is a harness configuration, not a validation identifier.

Further sealed dataset labels used by this plan name fixture versions sealed under `07 §7.0` rule 3. They add no pass criterion of their own; every criterion is the cited `07 §7` table.

| Dataset | Normative definition |
|---|---|
| `journey-v1` | `07 §7.3` sealed acceptance fixture for `AS-01`–`AS-12` |
| `ui-v1` | `07 §7.3` `U-01`–`U-10` comprehension baseline |
| `memory-v1`, `memory-holdout-v1` | `07 §7.4` reconciliation-quality protocol; holdout sealed and never tuned after labels freeze (`07 §7.0` rule 3) |
| `external-v1` | `07 §7.3` `AS-05` and `C-12` external-divergence scenarios |
| `security-v1` | `07 §7.1` `ZT-01`–`ZT-14` scanned-surface list |
| `cost-v1` | `07 §7.4` `LIM-1` workload reported against `07 §7.3` `AS-12` |

`DG-1`–`DG-8` are decision gates defined in this plan; they are not validation identifiers and close only on the `07 §7` evidence named in their producing task. A check for which `07 §7` defines no identifier (for example tool-descriptor ordering in `F4`) is a plan-local check: the owning task states it, independent review decides it, and it is never cited as a gate.

### Decision gates

| Gate | Decision | Owner | Producing task | Blocks |
|---|---|---|---|---|
| `DG-1` | Agent-facing execution language | Human | `A4` | `F4` |
| `DG-2` | Node versus Deno for the TypeScript control plane | Human | `A1` | `B1` onward |
| `DG-3` | Deployment plane and broker identity | Human | `A2` | `E1`, `E2`, `H7` |
| `DG-4` | Lightweight inter-agent isolation mechanism | Human | `A3` | `E1` |
| `DG-5` | Synchronous versus asynchronous index membership | Human | `B5` | `C1` |
| `DG-6` | Resource ceilings and quality thresholds | Human | `H4` | `H4A`, `H6`, `I1` |
| `DG-7` | Reconciliation quality thresholds | Human | `H2` | `I1` |
| `DG-8` | Final release sign-off | Human | `I5` | release |

Dependency ordering note: non-effectful agent execution foundations precede the security substrate, but effectful execution begins only after capabilities, sandboxing, and broker mediation exist. The plan-local index-membership measurement `B5` follows the ledger/CAS skeleton because it measures those contracts. `S4` and `DG-6` necessarily follow measurable components, and `S6` follows the frozen consent surface and the web shell. The origin-isolated frozen consent surface (`D3A`) precedes mutable approval UI integration (`F1`, `G1`). Effect enforcement is proved twice: `E3` exercises admission plumbing against explicitly synthetic test-scoped limits, and `H4A` re-verifies the same enforcement points against the human-approved values from `DG-6`. `H4` proposes values and the human decides at `DG-6`; no earlier task sets an operator ceiling. Activation is likewise split: `E1A` is a frozen supervisor stub that authorizes `F1`'s declarative activation receipts, and `F3` replaces it with the full candidate gate.

### Traceability matrix

Requirement anchors cite `00-requirements.md` line numbers (`req. N` = line N). Validation identifiers, fixtures, protocols, and pass criteria are defined only in `07 §7`. A row is satisfied only when every listed task is closed with that evidence and independent review. Every identifier below is a `07 §7` identifier; no row cites an identifier this plan defines. `07 §7.6` defines no `T` identifiers and owns no implementation-task traceability; this plan owns its `A0`–`I5` tasks, and the gate and scenario identifiers in that table are normative and the task labels are not; `A0`–`I5` is the implementation taxonomy.

| Requirement | Validation | Owning tasks |
|---|---|---|
| req. 7, 9 two projects plus Kanbei, world-state/memory/self-modification at comparable depth | `AS-01`–`AS-12`, `journey-v1` | `C2`, `I1`, `I4` |
| req. 17 authority records, effect receipts, replay never repeats effects | `I-05`, `I-09`, `I-10`, `ZT-07`, `ZT-11`, `V16` | `D1`, `D3`, `D7`, `H8` |
| req. 32–40 open components, one sequencer, append-only correction, hash chain | `I-01`–`I-03`, `ZT-02`, `ZT-09`, `V13`, `V17` | `B1`, `B2`, `B4`, `B6` |
| req. 44–52 semantic projection, retraction, quarantine, disclosure | `I-06`, `I-07`, `AS-06`, `AS-07`, `P-01`–`P-10`, `ZT-05`, `ZT-06`, `V15` | `C4`–`C7`, `C9`, `H2` |
| req. 56–58 intent anchor, interpretations, chat authority | `I-05`, `I-11`, `AS-02` | `D1` |
| req. 60–62 autonomy tiers, global queue, approval binding | `I-05`, `I-11`, `AS-03`, `C-06`–`C-09`, `ZT-08`, `ZT-13`, `U-05`, `V5`, `V8` | `D2`, `D3`, `D3A` |
| req. 64 override receipts versus hard boundaries | `I-12`, `C-10`, `ZT-04`, `V6` | `D2`, `D3`, `E3`, `H4A` |
| req. 66 nested work and evidence-bound completion | `AS-04`, `I-02`, `I-11`, `V1`, `V2` | `D4` |
| req. 68 external change recording without clobber | `AS-05`, `C-12`, `ZT-03` | `C3`, `D4` |
| req. 70–76 durable loops, inbox/outbox, timers, checkpoints | `C-03`, `R-05`, `R-06`, `V11`, `V12` | `D5`, `D8` |
| req. 72, 78 hard ceilings, at-least-once effects, `outcome_unknown` | `I-09`, `I-12`, `C-14`, `AS-12`, `V3`, `V4` | `D7`, `E3`, `H4`, `H4A` |
| req. 80, 128 exact post-redaction model and provider I/O | `R-05`, `AS-10` (recorded-I/O basis), `V10` | `D6`, `D8`, `G4` |
| req. 84–88 replay, undo, branch sets | `I-10`, `I-17`, `R-01`–`R-07`, `ZT-07`, `AS-10`, `V7`, `V9` | `D8`, `H8` |
| req. 92–104 self-modification, frozen trust root, scoped candidate activation, migrations | `I-13`, `I-14`, `AS-08`, `R-02`, `ZT-10` | `E1A`, `F1`–`F5`, `I1` |
| req. 108–118 threat model, access boundary, broker, credential authority | `ZT-01`, `ZT-02`, `ZT-04`, `AS-09`, `security-v1` | `E1`, `E2`, `E4`, `E5`, `F5`, `H5` |
| req. 122–126 tool surface, execution language, control-plane runtime | `S1`, `AS-08` (tool wiring) | `A1`, `A4`, `F4` |
| req. 132–138 web shell, protocols, curation, multimodal | `U-01`–`U-10`, `ui-v1`, `AS-06`, `AS-10` | `C7`, `G1`–`G3`, `H6` |
| req. 142–146 state path, SQLite ledger, CAS, ingest | `AS-01`, `B-01`–`B-04`, `I-07`, `I-08` | `B3`, `B4`, `C2` |
| req. 148–152 operational projections, retention, erasure, backup | `I-08`, `I-15`, `I-16`, `B-01`–`B-12`, `AS-11`, `ZT-12` | `B3`, `B5`, `C1`, `C8`, `H1`, `H3` |
| req. 154 loopback and tunnel exposure | `security-v1` (host exposure probes) | `H7` |
| req. 156–158 zero-tolerance gates, evidence-producing validation | `ZT-01`–`ZT-14`, `AS-01`–`AS-12`, `EvidenceEnvelope` | `A0`, `B6`, `C10`, `D3A`, `D9`, `E5`, `G5`, `H5`, `H9`, `I1`–`I5` |
| req. 160–168 deferred decisions and spike protocols | `S1`–`S6` | `A1`–`A6`, `C9`, `H2`, `H4`, `H6` |
| req. 72, 158 measured ceilings and thresholds after baseline | `S4`, `AS-12`, `I-12` | `A6`, `H4`, `H4A`, `H6`, `I3` |

---

## Phase A — Evidence substrate and decision-gated spikes

### A0 — Evidence harness, sealed fixtures, and replayable test substrate

- **Purpose:** Make every later result evidence-producing rather than assertion-producing.
- **Depends on:** none.
- **Scope:** Conformance runner; `EvidenceEnvelope`; test classes `unit`, `property`, `conformance`, `chaos`, `redteam`, `spike`, `bench`; virtual clock and synthetic network; post-redaction recorded model/search I/O; sealed fixture versions and hash manifests; deterministic rerun; environment fingerprint; artifact capture; failure classification.
- **Acceptance:** Validation: `EvidenceEnvelope` dry-run journey; `07 §7.0` schema validation; deterministic re-run; fixture hash manifest; failure-class recording. Review/fix: independent review of harness determinism, fixture sealing, and evidence completeness; defects fixed and checks rerun.
- **Gate:** none.

### A1 — Spike S1: Node versus Deno control-plane bake-off

- **Purpose:** Select the TypeScript control-plane runtime without weakening the OS sandbox.
- **Depends on:** `A0`.
- **Scope:** Node and Deno arms using the same control-plane code where feasible, same OS sandbox, same SQLite ledger, broker, and supervisor. Measure cold/warm start, RSS, event-loop p50/p95 under append/read load, SQLite driver correctness and single-writer behavior, worker/process isolation, signal handling, systemd integration, OpenTelemetry/tracing, packaging size, and permission behavior. Run the security probes against both.
- **Acceptance:** Validation: `S1` (`07 §7.4` `LANG-1` + `NODE-DENO-1`) pre-registration for the `NODE-DENO-1` arm, paired comparison, traces, decision matrix, hard-fail list, rejected alternative, and residual risks; the same `S1` protocol is shared with `A4`. Review/fix: independent review of protocol fidelity and resource measurements; decision record corrected before publication.
- **Gate:** closes `DG-2`; human-signed.

### A2 — Spike S2: deployment plane and broker identity

- **Purpose:** Determine whether pure user-space deployment can protect broker secrets and enforce fail-closed boundaries.
- **Depends on:** `A0`.
- **Scope:** Host checks for systemd, user namespaces, delegated cgroup controllers, `ProtectSystem`, `ProtectHome`, and writable state. Compare pure user-space against a dedicated-UID broker/sandbox helper. Test same-UID process visibility, ptrace/Yama, `/proc`, secret readability, Landlock, `PrivateTmp`, `PrivateNetwork`, `RestrictAddressFamilies`, `MemoryMax`, `MemoryHigh`, `CPUQuota`, `TasksMax`, broker identity, fail-open behavior, crash recovery, linger/restart, and loopback/tunnel/Access posture.
- **Acceptance:** Validation: `S2` (`07 §7.4` `DEP-1`) host check matrix, same-UID findings, fail-closed evidence, deployment-plane and broker-identity recommendation, explicit residuals. Review/fix: independent security review; any missing probe added and rerun.
- **Gate:** closes `DG-3`; human-signed.

### A3 — Spike S3: lightweight inter-agent isolation

- **Purpose:** Select a crash/resource containment mechanism, not a security boundary.
- **Depends on:** `A0`.
- **Scope:** Compare process with cgroups/rlimits, WASM instance, and microVM. Measure cold start, RSS, CPU, density at increasing agent counts, crash isolation, OOM containment, fork-bomb containment, cleanup after kill, IPC behavior, and operability. Container-per-agent is excluded.
- **Acceptance:** Validation: `S3` (`07 §7.4` `ISO-1`) containment matrix at target concurrency; explicit statement that no security property is claimed. Review/fix: independent review of containment claims and operability costs; defects fixed and rerun.
- **Gate:** closes `DG-4`; human-signed.

### A4 — Spike S1: agent-facing execution-language bake-off

- **Purpose:** Select the programmable execution language and runtime pair for the agent-facing surface.
- **Depends on:** `A0`.
- **Scope:** Arms: Monty, TypeScript, Lua/Luau, Bash, full CPython. Use the ten-task workload suite, host-call latency at 10 ms, 100 ms, and 1 s, at least two models per provider including a smaller model, and 30 trials per cell at temperature 0. Measure success and evidence coverage, tokens, turns, tool calls, retries, p50/p95 latency, startup and peak memory, snapshot/restore, progressive disclosure at increasing tool counts, security probes, and integration surface.
- **Acceptance:** Validation: `S1` (`07 §7.4` `LANG-1` + `NODE-DENO-1`) pre-registered non-inferiority and cost/latency/operability rules for the `LANG-1` arm, traces, failure taxonomy, selected language or explicit “no selection”; the same `S1` protocol is shared with `A1`. Review/fix: independent review of pre-registration and analysis; violations corrected before the gate.
- **Gate:** closes `DG-1`; human-signed.

### A5 — Gate consolidation and baseline freeze

- **Purpose:** Convert spike outcomes into fixed downstream contracts before implementation.
- **Depends on:** `A1`, `A2`, `A3`, `A4`.
- **Scope:** Record `DG-1` through `DG-4`; selected runtime, execution language, deployment plane, broker identity, and isolation mechanism; rejected alternatives; accepted residuals; provisional operator-envelope schema; harness configuration. Prohibit silent defaults.
- **Acceptance:** Validation: signed gate records and configuration manifests; cross-check that no dependent task can start on an unrecorded choice. Review/fix: independent review of gate records and dependency implications; inconsistencies fixed and records reissued.
- **Gate:** consumes `DG-1`–`DG-4`.

### A6 — Spike S4 and S6 pre-registration: resource, cost, freshness, and comprehension protocols

- **Purpose:** Pre-register threshold measurement before values are needed, without inventing numbers.
- **Depends on:** `A0`, `A5`.
- **Scope:** Define `cost-v1` workload, repetitions, comparison rules, bootstrap confidence intervals, cold-versus-warm separation, enforced-ceiling checks, quality metrics, and the `ui-v1` UI comprehension protocol. Do not set numeric thresholds.
- **Acceptance:** Validation: `S4` (`07 §7.4` `LIM-1`, `cost-v1`) and `S6` (`07 §7.4` `UI-1`, `UI-2`, `ING-1`, `ui-v1`) pre-registration records validate against the harness under `07 §7.0` rule 4; dependent benchmark tasks can reference them. Review/fix: independent review of metric definitions and decision rules; ambiguities fixed before benchmark implementation.
- **Gate:** prepares `DG-6`; closes no decision yet.

---

## Phase B — Walking skeleton

### B1 — Record envelope, canonicalization, component registry, and frozen append API

- **Purpose:** Establish the only canonical write path and preserve unknown components.
- **Depends on:** `A0`, `DG-2`.
- **Scope:** Narrow record envelope; namespaced independently versioned components; JCS canonical bytes; hash chain over canonical bytes and predecessor; frozen append API; authority-channel enforcement; unknown-component round-trip; schema/version registry; conditional policy checks; idempotency keys.
- **Acceptance:** Validation: `ZT-02`, `ZT-09`, `ZT-11`, `V13`, `V14`, `V17`; property tests; component-corpus round-trip; direct-SQLite denial. Review/fix: independent review of canonicalization, authority enforcement, and append boundary; defects fixed and zero-tolerance checks rerun.
- **Gate:** consumes `DG-2`; closes none.

### B2 — SQLite ledger, single sequencer, operational-projection commit, and recovery

- **Purpose:** Provide durable canonical ordering and synchronous read-your-writes.
- **Depends on:** `B1`.
- **Scope:** SQLite ledger with one writer, WAL, full synchronous durability, and foreign keys; single sequencer; transactional operational-projection commit; read-your-writes; startup integrity verification; crash/restart recovery; sequence gap/duplicate reporting.
- **Acceptance:** Validation: `C-01`, `C-02`; base `R-01`; crash before and after commit; restart evidence; gap/duplicate report. Review/fix: independent review of sequencer exclusivity, transaction boundaries, and recovery; defects fixed and rerun.
- **Gate:** consumes `DG-2`; closes none.

### B3 — Whole-object CAS, manifest publication, admission, and orphan reclamation

- **Purpose:** Make blob visibility ordered and independently replaceable.
- **Depends on:** `B1`.
- **Scope:** Local OCI-layout whole-object CAS behind a replaceable blob-store contract; publish bytes before manifest and manifest before ledger visibility; per-object and total admission limits; backpressure; GC; safe orphan reclamation; no chunk-level dedup.
- **Acceptance:** Validation: `B-01`–`B-04`; oversize, quota, and huge-object tests; crash after bytes before manifest. Review/fix: independent review of publication order, quota enforcement, and reclamation safety; defects fixed and rerun.
- **Gate:** consumes `DG-2`; closes none.

### B4 — Control-plane topology and walking-skeleton vertical slice

- **Purpose:** Prove one observation can traverse blob publication, canonical append, synchronous read, and a minimal user-visible surface across restart.
- **Depends on:** `A5`, `B2`, `B3`.
- **Scope:** Process topology selected by `DG-2`/`DG-3`; trusted core/sequencer, sandbox supervisor, broker, UI client, watchdog, and backup boundaries; loopback-only API; startup integrity checks; health/readiness. Vertical slice: fixture observation → CAS bytes → manifest → append → operational read → minimal web or CLI view → restart and reread.
- **Acceptance:** Validation: end-to-end evidence envelope; `B-01`/`B-02` subset; read-your-writes; restart; loopback binding; integrity-failure refusal. Review/fix: independent review of process boundaries and skeleton evidence; defects fixed and rerun.
- **Gate:** consumes `DG-2`, `DG-3`; closes none.

### B5 — Index-membership measurement (plan-local; closes `DG-5`)

- **Purpose:** Decide which cheap indexes may join the atomic operational-projection class.
- **Depends on:** `B2`, `B3`, `B4`.
- **Scope:** Prototype metadata tables, FTS, graph adjacency, embedding index, and proposed accelerators. Measure commit and read p50/p95, write amplification, database size, contention with the single writer, crash consistency, rebuild time, and digest equality. Test read immediately after commit, after restart, and during rebuild.
- **Acceptance:** Validation: plan-local pre-registered latency budget (harness configuration; no `07 §7.4` spike alias) with `07 §7.2` `I-16` and `07 §7.3` `C-05`, `R-01`, `B-08`, `ZT-12`; each index classified synchronous or asynchronous with a coverage watermark. Review/fix: independent review of measurement validity and classification; misclassifications corrected and rerun.
- **Gate:** closes `DG-5`; human-signed.

### B6 — Walking-skeleton review and contract freeze

- **Purpose:** Freeze append, ledger, CAS, and read contracts before state/memory expansion.
- **Depends on:** `B4`, `B5`.
- **Scope:** Cross-task review of canonical write path, ledger ordering, blob ordering, read API, coverage semantics, and evidence completeness. Freeze downstream contracts.
- **Acceptance:** Validation: conformance rerun of the skeleton; no open `ZT-02`, `ZT-09`, or `ZT-11` findings. Review/fix: independent review record; defects fixed; contract changes re-reviewed before freeze.
- **Gate:** consumes `DG-5`; closes none.

---

## Phase C — Canonical state, memory, and persistence

### C1 — Operational projection suite

- **Purpose:** Provide strong read-your-writes for operational state.
- **Depends on:** `B2`, `B4`, `B6`, `DG-5`.
- **Scope:** Work DAG, approvals, raw observation metadata, active configuration, and ledger status. Commit atomically with the ledger; expose coverage and pending work; rebuild deterministically; classify any index per `DG-5`.
- **Acceptance:** Validation: base `R-01`; `C-05` partial; coverage honesty; restart and rebuild digest equality. Review/fix: independent review of atomic visibility and watermark semantics; defects fixed and rerun.
- **Gate:** consumes `DG-5`; closes none.

### C2 — Source-adapter contract and HTTP/filesystem/search adapters

- **Purpose:** Capture source content as observations with provenance, not authority.
- **Depends on:** `B3`, `B4`, `B6`.
- **Scope:** Closed adapter contract; scan request/batch, observation drafts, capture evidence, coverage receipt, checkpoints, idempotency keys, event/full/targeted scans, missing grace, failure mapping. HTTP, filesystem, and search adapters use recorded transport until live providers are integrated. Adapters are producers, never canonical writers.
- **Acceptance:** Validation: `AS-01`; provenance, coverage, idempotent re-ingest, crash after each publication step, `304` handling with recorded transport. Review/fix: independent review of evidence ordering and absence behavior; defects fixed and rerun.
- **Gate:** closes none.

### C3 — Git adapter and external divergence

- **Purpose:** Record external commits immediately without clobbering speculative history.
- **Depends on:** `C2`.
- **Scope:** Git ref, commit, and rewrite observations; raw commit/blob capture; changed paths; force-push and abandonment; optional worktree observations; remote fetch as a capability-mediated effect; explicit conflict assessment; no-clobber basis checks; external commits are observations, not approvals.
- **Acceptance:** Validation: `AS-05`; `C-12`; `ZT-03`; no-clobber evidence; conflict surfacing. Review/fix: independent review of divergence classification and write refusal; defects fixed and zero-tolerance checks rerun.
- **Gate:** closes none.

### C4 — Semantic claim projection and provenance

- **Purpose:** Materialize rebuildable semantic claims without granting them authority.
- **Depends on:** `C1`, `C2`.
- **Scope:** Candidate claims derived from observations; source span or selector; derivation; scope; coverage watermark; confidence as uncalibrated metadata; no authority records from content; declared projection versions.
- **Acceptance:** Validation: `AS-06` partial; `I-07`; rebuild determinism; missing-provenance rejection. Review/fix: independent review of provenance and authority separation; defects fixed and rerun.
- **Gate:** closes none.

### C5 — Reconciliation proposals: merge, split, alias, contradiction, and coverage

- **Purpose:** Apply non-destructive reconciliation while preserving source identities.
- **Depends on:** `C4`.
- **Scope:** Agent-proposed merge, split, alias, contradiction, rebutting, and undercutting operations; projection-version application; stable source identities; reversibility; working-claim preference with contested alternatives visible; incremental subgraph handling.
- **Acceptance:** Validation: `C-04`, `C-05`; merge reversal exactness; split purity; contested-state visibility; `memory-v1` baseline. Review/fix: independent review of reversibility and non-destructiveness; defects fixed and rerun.
- **Gate:** closes none.

### C6 — Retraction, read-time exclusion, quarantine, and poisoning response

- **Purpose:** Ensure retracted or poisoned claims are never presented as current.
- **Depends on:** `C5`.
- **Scope:** Retraction records; synchronous suspect marks; read-time exclusion before rebuild; scope enforcement at read time; quarantine of affected subgraphs; asynchronous incremental rebuild; prior claims and derivation history remain inspectable; dependent approvals become stale.
- **Acceptance:** Validation: `AS-07`; `P-01`–`P-10`; `ZT-05`, `ZT-06`, `V15`; zero stale-current reads; coverage-honesty checks. Review/fix: independent review of synchronous exclusion and quarantine blast radius; defects fixed and zero-tolerance checks rerun.
- **Gate:** closes none.

### C7 — Disclosure projection DAG and human curation gestures

- **Purpose:** Provide versioned disclosure without fixed world/feature/task levels.
- **Depends on:** `C4`.
- **Scope:** Arbitrary versioned disclosure projection DAGs with coverage, provenance, and staleness; human curation gestures append merge, split, link, scope, correction, retraction, and promotion records; agents and renderers receive the same structured view specifications.
- **Acceptance:** Validation: `AS-06`; provenance drill-down; disclosure expansion equals the evidence graph; curation records are authority records only when human-authored. Review/fix: independent review of DAG coverage and gesture authority; defects fixed and rerun.
- **Gate:** closes none.

### C8 — Backup, restore, erasure epochs, and derived rebuild

- **Purpose:** Make persistence recoverable and physical erasure exceptional but explicit.
- **Depends on:** `B2`, `B3`, `C4`.
- **Scope:** Online SQLite point-in-time snapshot immediately before filesystem backup; CAS copy with GC paused; checkpoint digests; restore reference verification; derived index rebuild; erasure epoch covering online and controlled backup artifacts; retention separation; residual declaration for offline copies.
- **Acceptance:** Validation: `AS-11`; `B-05`–`B-12`; tampered snapshot/blob restore; erasure residual report; migration compatibility. Review/fix: independent review of snapshot consistency and erasure boundaries; defects fixed and rerun.
- **Gate:** closes none.

### C9 — Memory baseline, holdout protocol, and resolver freeze

- **Purpose:** Provide measured reconciliation quality without inventing thresholds.
- **Depends on:** `C4`, `C5`, `C6`.
- **Scope:** Sealed `memory-v1` and `memory-holdout-v1`; two independent label passes; transaction-time and valid-time labels; frozen resolver prompts/rules before holdout; metric reporting for pairwise, B-cubed, split, contradiction, retraction propagation, poison residual, false quarantine, and blast radius.
- **Acceptance:** Validation: `S5` (`07 §7.4`; `memory-v1`, `memory-holdout-v1`) dataset hashes; label protocol; baseline report with confidence intervals; pre-registration for `DG-7`. Review/fix: independent review of label leakage and metric implementation; defects fixed and baseline regenerated.
- **Gate:** feeds `DG-7`; closes no decision yet.

### C10 — Canonical state and memory phase review

- **Purpose:** Verify provenance, coverage, retraction, and recovery contracts before agent execution.
- **Depends on:** `C1`–`C9`.
- **Scope:** Cross-task review of observation authority, semantic derivation, reconciliation reversibility, read-time exclusion, disclosure, backup/restore, and memory baseline.
- **Acceptance:** Validation: conformance rerun of `AS-01`, `AS-05`, `AS-06`, `AS-07`; no open `ZT-05` or `ZT-06` findings. Review/fix: independent review record; defects fixed; affected evidence rerun before phase close.
- **Gate:** closes none.

---

## Phase D — Agent execution foundations

### D1 — Intent anchors, chat authority inference, and interpretations

- **Purpose:** Preserve exact human intent while allowing versioned agent interpretations.
- **Depends on:** `C1`.
- **Scope:** Immutable intent anchor; amendments and vetoes append linked records; source spans; interpretations with assumptions, scope, plan reference, and acceptance criteria; interpretation hash; chat authority inference as derived metadata; forbidden authority creation from content.
- **Acceptance:** Validation: `AS-02`; `I-05`, `I-11`; authority-injection rejection; hash binding. Review/fix: independent review of anchor immutability and derived-authority separation; defects fixed and rerun.
- **Gate:** closes none.

### D2 — Autonomy policy and tier evaluation

- **Purpose:** Make autonomy tiers human-owned and agent-proposable only.
- **Depends on:** `D1`.
- **Scope:** Declarative policy with `automatic`, `review_after`, `approval_required`, and `forbidden`; matching on operation, effect class, data class, reversibility, blast radius, and substrate; most-restrictive rule; unknown or unmatched scope defaults to approval-required; tier proposals cannot self-grant; `forbidden` is an admission denial.
- **Acceptance:** Validation: `C-11`, `V6`; tier matrix; no-match default; forbidden admission. Review/fix: independent review of policy precedence and self-grant denial; defects fixed and rerun.
- **Gate:** closes none.

### D3 — Global decision queue, approval binding, batching, and expiry

- **Purpose:** Bind approvals to exact intent and plan versions without bypass.
- **Depends on:** `D1`, `D2`, `C1`.
- **Scope:** One global queue grouped by intent and deadline; shared-evidence deduplication; individual and batch rulings; approval binding to interpretation hash, plan hash, subject, authorized scope, constraints, and expiry; stale ruling on evidence/plan/scope change; duplicate idempotency.
- **Acceptance:** Validation: `C-06`–`C-09`; `V8`; queue grouping by intent and deadline; evidence-set dedupe; exact approval-binding and staleness fields; batch and duplicate rulings. Review/fix: independent review of binding and staleness transitions; defects fixed and rerun. Consent-surface evidence for approval binding (`AS-03`, `ZT-08`, `V5`) cannot pass before the frozen surface exists and closes with `D3A`.
- **Gate:** closes none.

### D3A — Frozen consent renderer, single-use token service, and gesture endpoint

- **Purpose:** Provide the only surface that can turn a human decision into a ruling, before any mutable approval UI exists.
- **Depends on:** `D1`, `D3`, `B4`.
- **Scope:** Origin-isolated minimal `kanbei-consent` renderer served from its own origin with no mutable candidate or agent-reachable code; render directly from canonical projections; a single-use, session-bound consent token minted per render and consumed atomically by the gesture endpoint; same-origin and CSRF binding; frame-ancestors/clickjack refusal; gestures that carry the ruling (`approve`, `reject`, `defer`, `amend`) and the rendered `consentDigest`; manifest and ruling binding to interpretation hash, resolved plan hash, work-item ids, completion subject, evidence-set hash, authorized scope, constraints, expiry, `consentDigest`, `subject_hash`, `granted_tier`, `max_effect_class`, and reversibility; non-human principals and mutable-UI-supplied digests rejected.
- **Acceptance:** Validation: `AS-03`, `ZT-08`, `ZT-11`, `ZT-13`, `U-05`, `I-11`, `V5`; consumed-token replay, cross-site gesture (CSRF), framed clickjack attempt, and mutable-UI-supplied digest all reject; one ruling per token; complete manifest and ruling binding over anchor, interpretation hash, resolved plan hash, work-item ids, completion subject, evidence-set hash, authorized scope, constraints, expiry, `consentDigest`, `subject_hash`, `granted_tier`, `max_effect_class`, and reversibility, recomputed from canonical projections. Review/fix: independent security review of origin isolation, token lifetime, gesture authorization, and manifest completeness; defects fixed and zero-tolerance checks rerun.
- **Gate:** closes none.

### D4 — Work DAG, completion evidence, and promotion

- **Purpose:** Make nested work reviewable and completion evidence-bound.
- **Depends on:** `D3A`.
- **Scope:** Nested work items with one ownership parent and an arbitrary causal DAG; work-state machine; completion evidence against acceptance criteria and blockers; autonomy-tier promotion; `outcome_unknown` blocks completion; external-divergence hooks.
- **Acceptance:** Validation: `AS-04`; `I-02`, `I-11`, `V1`, `V2`; ownership/causal acyclicity; completion denial without evidence. Review/fix: independent review of work-state enforcement and promotion; defects fixed and rerun.
- **Gate:** closes none.

### D5 — Durable loop runtime

- **Purpose:** Provide persistent self-triggering agent loops without a second workflow authority.
- **Depends on:** `B2`, `D4`.
- **Scope:** Loop entity and state machine; idempotent inbox/outbox; subscriptions; wakeups and coalescing; timers; self-trigger progress markers and stall escalation; idle backoff; concurrency policy; checkpoints; cancellation; crash recovery.
- **Acceptance:** Validation: `C-03`, `V11`, `V12`; duplicate delivery; crash/resume; timer recovery; self-loop no-progress escalation. Review/fix: independent review of idempotency and recovery; defects fixed and rerun.
- **Gate:** closes none.

### D6 — Provider-neutral model routing and exact I/O capture

- **Purpose:** Capture exact available model evidence without promising regeneration.
- **Depends on:** `B3`, `D5`.
- **Scope:** Human-owned task-routing policies; provider-neutral request/response contract; exact post-redaction semantic and wire payload capture of request, response, params, output events, and exposed reasoning artifacts, each carrying its redaction commitment; credential-bearing bytes are neither persisted nor required as evidence; reasoning absence explicit; retries as new model calls; recorded-I/O replay path. Live provider transports come later.
- **Acceptance:** Validation: `V10`; post-redaction capture digests and redaction-commitment records; zero credential-bearing bytes in any artifact, CAS, or canonical surface (`ZT-01`); `429`/`5xx`/timeout/malformed-output tests with a fake provider; reasoning absence; replay performs no model call. Review/fix: independent review of I/O completeness, redaction commitments, and replay prohibition; defects fixed and rerun.
- **Gate:** closes none.

### D7 — Effect journal, idempotency, `outcome_unknown`, and compensation

- **Purpose:** Make external-effect semantics explicit before real capabilities exist.
- **Depends on:** `B2`, `D5`.
- **Scope:** Effect states; journal-before-dispatch; idempotency keys; fencing tokens; at-least-once attempts; `outcome_unknown`; no blind retry of unknown non-idempotent effects; compensation as a new effect; receipts; denied attempts recorded. Use a test capability only.
- **Acceptance:** Validation: `I-09`, `V3`, `V4`, `V16`; effect state-machine tests; crash after attempt; no blind retry; `C-14` partial. Review/fix: independent review of ambiguous-outcome handling and receipt evidence; defects fixed and rerun.
- **Gate:** closes none.

### D8 — Checkpoints, exact available LLM I/O, and undo forest

- **Purpose:** Support deterministic recovery, replay, and additive undo.
- **Depends on:** `D5`, `D6`, `D7`.
- **Scope:** Checkpoints with state digest, open effects, inbox cursors, timers, model-I/O head, and plugin-state references; single undo from a checkpoint; cross-agent dependency-aware branch sets; descendants preserved; explicit consistency mode; residual effects.
- **Acceptance:** Validation: `R-05`, `R-06`, `V7`; `I-17`; descendant preservation; no assumed global cut; digest mismatch handling. Review/fix: independent review of undo semantics and replay basis; defects fixed and rerun.
- **Gate:** closes none.

### D9 — Agent-execution conformance review

- **Purpose:** Verify authority, approval, work, loop, and journal contracts before effectful execution.
- **Depends on:** `D1`–`D8`, `D3A`.
- **Scope:** Run `AS-02`–`AS-05`, `C-03`, `C-06`–`C-11`, and `R-05`/`R-06` with recorded model I/O and the test capability. Review invariant coverage.
- **Acceptance:** Validation: conformance evidence bundle; no open `ZT-08` or `ZT-11` findings; recorded-I/O replay. Review/fix: independent review record; defects fixed; affected evidence rerun before phase close.
- **Gate:** closes none.

---

## Phase E — Security, broker, and effectful execution

### E1 — Capability contracts, sandbox supervisor, and fail-closed verification

- **Purpose:** Enforce mediated external authority and containment.
- **Depends on:** `A5` (`DG-3`, `DG-4`), `B2`, `B4`.
- **Scope:** Mediated filesystem, process, network, model, and secret capabilities; sandbox specification and mount classes; writable roots; enforcement class; post-start verification; fail-closed launch; resource limits; process identities; no canonical database, trust root, or secrets mounted into agent code; inter-agent containment.
- **Acceptance:** Validation: `ZT-02`, `ZT-04`, `ZT-10`; fail-open test; namespace/Landlock/ptrace probes; resource enforcement; mount inspection. Review/fix: independent security review; defects fixed and zero-tolerance checks rerun.
- **Gate:** consumes `DG-3`, `DG-4`; closes none.

### E1A — Frozen activation-supervisor stub

- **Purpose:** Provide the sole authority path for activation and activation-receipt records before full candidate validation exists, so declarative activation never appends supervisor-only records itself.
- **Depends on:** `E1`, `B6`.
- **Scope:** Frozen supervisor process identity and launch contract; append path for activation and activation-receipt components; atomic flip and revert of one pre-registered scope pointer; startup verification that agent and plugin channels cannot append those component classes; append-only activation history. Only a sealed synthetic candidate: no mutable code loading, capability diff, health check, or external-effect rollback. Explicit retirement list for `F3`.
- **Acceptance:** Validation: `ZT-02`, `ZT-10` stub-scope subset; agent/plugin append attempts denied; pointer flip and revert recorded as supervisor-authored receipts; trust-root mutation refused. Review/fix: independent review of the stub's authority boundary and retirement list; defects fixed and zero-tolerance checks rerun.
- **Gate:** consumes `DG-3`, `DG-4`; closes none.

### E2 — Credential broker

- **Purpose:** Inject enrolled credentials without exposing them to agent code.
- **Depends on:** `E1`, `DG-3`.
- **Scope:** Human enrollment authority; logical hostname to endpoint mapping; resolve-once or pinned-address policy; TLS verification; credential-header stripping; exactly-once injection; redirects never followed; cross-host refusal; response and log redaction; audit intent before secret use; revocation/rotation; no method/path authorization.
- **Acceptance:** Validation: `AS-09`; `ZT-01`; DNS rebinding, redirect, SNI/TLS confusion, CONNECT/raw TCP, credential replay, confused deputy, redaction coverage. Review/fix: independent security review of broker ordering and secret boundaries; defects fixed and zero-tolerance checks rerun.
- **Gate:** consumes `DG-3`; closes none.

### E3 — Effectful execution integration

- **Purpose:** Connect the effect journal to real capabilities and hard-policy plumbing.
- **Depends on:** `D7`, `E1`, `E2`.
- **Scope:** Filesystem, process, broker, model, and search effects; hard policy, budget, tier, and receipt checks before dispatch; idempotency keys; `outcome_unknown`; compensation; receipts; denial records; no effect outside sandbox, broker, global resource, or frozen-root policy. Admission uses explicitly synthetic test-scoped limits from a sealed fixture; this task sets no operator ceiling and asserts no approved value, and effects stay confined to the fixture. Continuous or production dispatch remains disabled until `H4A`. It records the enforcement points that `H4A` must re-verify.
- **Acceptance:** Validation: `I-09`, `I-12`; `C-14`; `ZT-04` under synthetic limits; crash after attempt; compensation as new effect; policy denial evidence; enforcement-point inventory for `H4A`. Review/fix: independent review of admission ordering and receipt completeness; defects fixed and zero-tolerance checks rerun.
- **Gate:** consumes `DG-3`; closes none.

### E4 — Security red-team core

- **Purpose:** Demonstrate the hostile-agent threat model against the implemented boundary.
- **Depends on:** `E1`, `E2`, `E3`.
- **Scope:** Hostile agent/tool code, hostile dependencies, prompt injection, filesystem theft, canonical DB write, sandbox escape, arbitrary egress, DNS rebinding, redirect bypass, TLS/SNI confusion, opaque tunnels, credential exfiltration/replay, confused deputy, approval bypass, audit forgery, persistence, fail-open, and trust-root mutation.
- **Acceptance:** Validation: `ZT-01`, `ZT-02`, `ZT-04`, `ZT-10`; `security-v1`; accepted residuals recorded, especially project-data exfiltration and provider-wide credential authority. Review/fix: independent red-team review; every finding fixed or explicitly accepted with evidence before phase close.
- **Gate:** closes none.

### E5 — Effectful agent-execution conformance review

- **Purpose:** Verify real effects do not bypass approval, broker, sandbox, or receipt contracts.
- **Depends on:** `D9`, `E3`, `E4`.
- **Scope:** Run `AS-04`, `AS-05`, `AS-09`, `C-12`, and `C-14` with real capabilities and broker. Check approval scope coverage, external conflict recording, no-clobber behavior, and effect receipts.
- **Acceptance:** Validation: conformance evidence bundle; zero-tolerance checks never retried. Review/fix: independent review record; defects fixed; affected evidence rerun before phase close.
- **Gate:** closes none.

---

## Phase F — Self-modification

### F1 — Declarative self-modification

- **Purpose:** Allow safe live mutation of declarative context, UI, settings, keybinds, themes, and tool wiring.
- **Depends on:** `D1`, `D3A`, `D4`, `C7`, `E1`, `E1A`.
- **Scope:** Versioned declarative documents; schema and budget validation; scope restriction; ordered and total disclosure rules; explicit fallback; next-turn activation with the current turn pinned; rollback to the previous candidate; activation receipt authored only by the `E1A` supervisor stub and visibility/hidden diff. Human decision views remain outside agent context scope.
- **Acceptance:** Validation: `AS-08` declarative lane; invalid-document rejection; next-turn pinning; previous-candidate retention; coverage honesty; activation and revert receipts carry only the `E1A` authority path. Review/fix: independent review of declarative validation and hidden-evidence effects; defects fixed and rerun.
- **Gate:** closes none.

### F2 — Plugin kernel, scopes, and scoped state

- **Purpose:** Isolate arbitrary plugin code and its mutable state from the canonical ledger.
- **Depends on:** `F1`, `E1`.
- **Scope:** Scope graph; plugin manifest; `PluginContext`; required/optional dependencies; separate scoped stores; rebuildable and checkpointed durability; canonical append/query handles bound to scope; no direct SQLite handle; sandboxed plugin host; co-location only within the same scope and trust class.
- **Acceptance:** Validation: `I-13`; manifest rejection for undeclared capabilities/state; plugin-state rebuild; direct-SQLite denial. Review/fix: independent review of scope isolation and state durability; defects fixed and zero-tolerance checks rerun.
- **Gate:** closes none.

### F3 — Candidate staging, validation, capability diff, activation, and rollback

- **Purpose:** Activate mutable code through scoped candidates rather than live edits.
- **Depends on:** `F2`, `E3`, `E1A`.
- **Scope:** Candidate generations; dependency lock; manifest/schema validation; static capability scan; sandboxed dynamic conformance; migration dry-run; security probes; capability and dependency diff; shadow state; standby boot; drain; atomic scope-pointer flip; post-flip health; rollback; previous-candidate retention; frozen trust root. Retire the `E1A` synthetic-scope stub and carry out its retirement list, leaving one activation authority path.
- **Acceptance:** Validation: `AS-08` code lane; `I-14`; `ZT-10`; activation crash matrix; external effects not rolled back by pointer flip; forward-only rollback refusal; `E1A` retirement evidence with no residual synthetic-scope append path. Review/fix: independent review of activation ordering and trust-root exclusion; defects fixed and zero-tolerance checks rerun.
- **Gate:** closes none.

### F4 — Agent-visible programmable tool surface and execution runtime

- **Purpose:** Expose one discovery-and-execution surface instead of per-tool schemas.
- **Depends on:** `DG-1`, `F3`.
- **Scope:** `search`, `list`, `describe`, and `execute`; versioned tool descriptors; `call` and `program` modes; typed host-call stubs; result envelope; structured errors; streaming and cancellation; host-call budgets; runtime adapter selected by `DG-1`; capability denial.
- **Acceptance:** Validation: discovery conformance; deterministic ordering and pagination; host-call behavior; capability denial; runtime-specific conformance. Review/fix: independent review of descriptor versioning and host-call accounting; defects fixed and rerun.
- **Gate:** consumes `DG-1`; closes none.

### F5 — Self-modification crash, trust-root, and external-effect tests

- **Purpose:** Prove activation failure cannot corrupt authority or silently undo external effects.
- **Depends on:** `F3`, `F4`.
- **Scope:** Crash before and after pointer flip; health failure and rollback; rollback blocked by forward-only migration; trust-root write attempts; candidate quarantine; external-effect residual after rollback; agent editing its own context lens without affecting human decision views.
- **Acceptance:** Validation: `ZT-10`; full `AS-08`; activation crash matrix; external-effect residual report. Review/fix: independent review of rollback and residual semantics; defects fixed and zero-tolerance checks rerun.
- **Gate:** closes none.

---

## Phase G — UI, live providers, and multimodal state

### G1 — Web shell and core graph, timeline, and decision views

- **Purpose:** Provide the stable human interface over renderer-neutral state.
- **Depends on:** `D3`, `D3A`, `D4`, `C7`, `F1`.
- **Scope:** Stable web shell; coordinated graph, delta timeline, and global decision queue; coverage watermarks; contested, retracted, suspect, stale, and current distinctions; approval comprehension; undo visibility; accessibility tree and text fallback.
- **Acceptance:** Validation: `U-01`–`U-10` structural assertions; presentation-replay subset; renderer-neutral catalogs; accessibility checks. Review/fix: independent UI review against comprehension invariants; defects fixed and rerun.
- **Gate:** closes none.

### G2 — AG-UI adapter, A2UI catalogs, graph components, and MCP Apps boundary

- **Purpose:** Keep canonical semantics internal while interoperating with presentation protocols.
- **Depends on:** `G1`.
- **Scope:** AG-UI run/state/interrupt adaptation; ordinary A2UI catalog rendering; custom graph components; MCP Apps-compatible sandboxed artifact boundary; no canonical UI state; stale-delta rejection; interrupt batching as a graph constraint.
- **Acceptance:** Validation: `U-09`; protocol conformance; artifact sandbox isolation; stale-delta rejection; batched interrupt resume. Review/fix: independent protocol review; defects fixed and rerun.
- **Gate:** closes none.

### G3 — Multimodal ingestion, previews, and derived-artifact separation

- **Purpose:** Content-address arbitrary files while keeping raw objects distinct from derivations.
- **Depends on:** `C2`, `B3`, `G1`.
- **Scope:** Text, code, images, screenshots, and PDFs; provenance and previews; OCR, text, and metadata derivations; audio/video stored without full semantic indexing; canonical raw objects distinct from transcripts, OCR, embeddings, and thumbnails; retraction-aware derived artifacts.
- **Acceptance:** Validation: provenance drill-down; derived-versus-raw distinction; preview correctness; stale derived artifacts after retraction. Review/fix: independent review of provenance and derivation invalidation; defects fixed and rerun.
- **Gate:** closes none.

### G4 — Live provider integrations: model routing and TinyFish search/fetch

- **Purpose:** Connect real providers behind neutral contracts without assuming equivalent capabilities.
- **Depends on:** `D6`, `E2`, `E3`, `C2`.
- **Scope:** TinyFish search and fetch behind the provider-neutral contract; conditional fetch; documented rate limits and backpressure; `402`, `403`, `429`, `5xx`, timeout, and malformed-output mapping; exact post-redaction semantic and wire provider envelopes with recorded redaction commitments; live model providers behind routing; post-redaction I/O capture; quota accounting.
- **Acceptance:** Validation: recorded live runs; `429`/`5xx`/timeout/malformed tests; conditional `304`; post-redaction evidence preservation with redaction-commitment records; `ZT-01`; no content-bearing observation on failure. Review/fix: independent provider review; defects fixed and rerun.
- **Gate:** closes none.

### G5 — UI and provider phase review

- **Purpose:** Verify presentation neutrality, provider provenance, multimodal separation, and accessibility before hardening.
- **Depends on:** `G1`–`G4`.
- **Scope:** Cross-review of renderer neutrality, protocol adaptation, artifact sandboxing, provider evidence, derived-artifact invalidation, and comprehension invariants.
- **Acceptance:** Validation: conformance rerun of `U-*` and provider tests; no open provenance findings. Review/fix: independent review record; defects fixed; affected evidence rerun before phase close.
- **Gate:** closes none.

---

## Phase H — Hardening and operational readiness

### H1 — Chaos, crash, and fault-injection matrix

- **Purpose:** Prove recovery behavior across ledger, CAS, loops, effects, activation, and backup.
- **Depends on:** `B2`, `B3`, `C8`, `D5`, `D7`, `E3`, `F3`.
- **Scope:** `SIGKILL`, `ENOSPC`, `EIO`, fsync failure, SQLite busy/corruption detection, OOM, fork bomb, clock jump, duplicate and out-of-order inbox delivery, provider failures, broker reset, CAS GC concurrent with read, and structured-contract violations. Cover every crash-matrix row.
- **Acceptance:** Validation: chaos matrix; error classes recorded; no blind retry of unknown non-idempotent effects; recovery evidence. Review/fix: independent review of recovery claims and fault coverage; defects fixed and rerun.
- **Gate:** closes none.

### H2 — Reconciliation metrics and `DG-7`

- **Purpose:** Set reconciliation quality thresholds from measured holdout performance.
- **Depends on:** `C4`, `C5`, `C6`, `C9`.
- **Scope:** Run frozen resolver on `memory-holdout-v1`; report pairwise, B-cubed, split, contradiction, retraction propagation, poison residual, false quarantine, and blast radius with confidence intervals; confirm exact invariants: zero stale-current and exact rebuild. Set thresholds from the pre-registered baseline.
- **Acceptance:** Validation: `S5` (`07 §7.4`) holdout metrics with confidence intervals; exact invariants at zero; threshold proposal. Review/fix: independent review of holdout isolation and metric computation; defects fixed and holdout rerun only under the pre-registered protocol.
- **Gate:** closes `DG-7`; human-signed.

### H3 — Backup, restore, and erasure hardening

- **Purpose:** Prove recovery and erasure under realistic writes and tampering.
- **Depends on:** `C8`, `H1`.
- **Scope:** Writes during backup; GC pause; restore verification; missing, truncated, or tampered snapshots/blobs; erasure epoch all-or-nothing behavior; offline-copy residuals; retention separation; no destructive automatic down migration; rollback only when data remains readable.
- **Acceptance:** Validation: `AS-11`; `B-05`–`B-12`; tampered restore; erasure residual report; migration refusal evidence. Review/fix: independent review of snapshot ordering and erasure reach; defects fixed and zero-tolerance checks rerun.
- **Gate:** closes none.

### H4 — Spike S4: resource ceilings, budgets, and quality thresholds

- **Purpose:** Convert measured headroom into enforced operator configuration.
- **Depends on:** `H1`, `E3`, `C6`, `G4`, `A6`.
- **Scope:** Run `cost-v1`: full journey, idle period, fan-out burst, reconciliation backlog, external conflict, and backup/restore. Measure CPU, RSS, pids, IO, disk growth, CAS growth, tokens, spend, provider rate usage, concurrency, and latency. Set global spend/token/rate/concurrency ceilings; per-loop soft budgets and override rules; memory, CPU, task, IO, disk, and CAS admission limits; sandbox and broker concurrency; freshness objectives; quality thresholds for reconciliation and UI comprehension.
- **Acceptance:** Validation: `S4` (`07 §7.4` `LIM-1`) pre-registered workload and repetitions; bootstrap confidence intervals; enforcement evidence; hard fail if a ceiling cannot be observed or enforced. Review/fix: independent review of headroom calculation and enforcement; defects fixed and benchmark rerun.
- **Gate:** closes `DG-6`; human-signed.

### H4A — Effect enforcement under approved ceilings

- **Purpose:** Verify the `E3` admission path against the human-approved values from `DG-6` without choosing any value.
- **Depends on:** `H4`, `E3`.
- **Scope:** Load the approved operator configuration record; replay the `E3` enforcement-point inventory with the real ceilings; confirm every configured spend, token, rate, concurrency, resource, CAS, sandbox, and broker limit denies before dispatch and is observable; record the synthetic-to-approved mapping. Changing a value requires a new human decision, not this task.
- **Acceptance:** Validation: `I-12`, `ZT-04`, `C-14` under approved ceilings; `AS-12` hard-ceiling and soft-override subset; enforcement and observation evidence for each `DG-6` ceiling; any unenforceable ceiling blocks release and returns to the human. Review/fix: independent review of the approved-value mapping and denial evidence; defects fixed and checks rerun.
- **Gate:** consumes `DG-6`; closes none.

### H5 — Full zero-tolerance and red-team release suite

- **Purpose:** Exercise every release-blocking gate, not only the core six.
- **Depends on:** `D3A`, `E4`, `F5`, `H1`, `H3`, `H4A`.
- **Scope:** `ZT-01`–`ZT-14`; `security-v1`; consent replay, clickjack, CSRF, and token reuse (`ZT-13`); lifecycle-transition fabrication (`ZT-14`); persistence, audit forgery, fail-open, trust-root mutation, approval bypass, coverage dishonesty, replay tripwire, and integrity failure. Zero-tolerance failures never retry.
- **Acceptance:** Validation: complete zero-tolerance evidence bundle covering `ZT-01`–`ZT-14`; each run `pass` or `blocked`; no waiver path. Review/fix: independent release-security review; any defect blocks release candidate until fixed and rerun.
- **Gate:** closes none.

### H6 — Cost, freshness, and UI-comprehension release benchmarks

- **Purpose:** Verify the system is operable and comprehensible under configured thresholds.
- **Depends on:** `A6`, `H2`, `H4`, `G1`, `G2`.
- **Scope:** Cost metrics; coverage lag, external-ingest lag, queue age, decision-to-effect, reconciliation catch-up, index lag, and stale retracted reads; UI comprehension protocol with ten scripted questions against a raw-ledger baseline; presentation replay; accessibility.
- **Acceptance:** Validation: `S6` (`07 §7.4` `UI-1`, `UI-2`, `ING-1`) pre-registered protocol; distributions and confidence intervals; thresholds from `DG-6` and `DG-7`; zero stale retracted reads. Review/fix: independent review of benchmark protocol and comprehension analysis; defects fixed and rerun.
- **Gate:** consumes `DG-6`, `DG-7`; closes none.

### H7 — Deployment packaging and operational readiness

- **Purpose:** Package the selected deployment plane for reproducible operation, backup, and rollback.
- **Depends on:** `A5` (`DG-3`), `E1`, `E2`, `C8`.
- **Scope:** Unit or generation packaging; state path; backup registration; loopback binding; Cloudflare Tunnel and Access posture; watchdog and alerting; restart and linger behavior; rollback path; packaged host checks; no fail-open launch.
- **Acceptance:** Validation: packaged host-check rerun; restore from backup; rollback evidence; fail-open refusal. Review/fix: independent operations review; defects fixed and checks rerun.
- **Gate:** consumes `DG-3`; closes none.

### H8 — Replay, undo, and presentation-replay full suite

- **Purpose:** Prove the three replay contracts and additive undo.
- **Depends on:** `D8`, `G1`, `G2`, `C8`.
- **Scope:** Operational projection rebuild; plugin-state rebuild; presentation replay; effect tripwire; single and repeated undo; cross-agent branch sets; external compensation as a new effect; recorded LLM I/O only.
- **Acceptance:** Validation: `AS-10`; `R-01`–`R-07`, `V9`; `ZT-07`; descendant-preservation evidence; no effect dispatched during replay. Review/fix: independent review of replay boundaries and undo visibility; defects fixed and zero-tolerance checks rerun.
- **Gate:** closes none.

### H9 — Hardening review and release-candidate evidence bundle

- **Purpose:** Consolidate hardening evidence and block release on unresolved zero-tolerance findings.
- **Depends on:** `H1`–`H8`.
- **Scope:** Cross-phase review of chaos recovery, reconciliation, backup/erasure, thresholds, red-team results, benchmarks, deployment, and replay. Consolidate evidence and residual register; produce release-candidate recommendation.
- **Acceptance:** Validation: complete evidence bundle; no open zero-tolerance gate; every residual explicitly accepted or listed as unresolved. Review/fix: independent review record; defects fixed; affected evidence rerun before final acceptance.
- **Gate:** closes none.

---

## Phase I — Final end-to-end acceptance and review

### I1 — Full canonical journey acceptance

- **Purpose:** Run the approved-in-principle journey end to end.
- **Depends on:** `H9`, `DG-6`, `DG-7`.
- **Scope:** `AS-01`–`AS-12` on the sealed `journey-v1` fixture over two selected projects plus Kanbei: ingest both projects and Kanbei, explore intent, approve structural goal, execute nested deltas, reconcile external commits, reconcile memory, retract/quarantine, self-modify through scoped live candidate activation of one declarative change and one tool/plugin change with capability diff and rollback (frozen trust root untouched), make one credentialed call through the broker, replay/undo, backup/restore/erasure, and measure cost/freshness.
- **Acceptance:** Validation: every scenario executes; two-project plus Kanbei scope exercised, including cross-project scope isolation; self-modification passes the `AS-08` code lane through scoped live candidate activation with capability diff, one pointer flip, health check, and rollback, while any frozen-kernel change remains generation-gated and denied to agents (`ZT-10`); all assertions recorded; evidence envelope bound to canonical head, fixture version, and post-redaction recordings. Review/fix: independent end-to-end review; failures fixed and full scenario rerun.
- **Gate:** consumes `DG-6`, `DG-7`; closes none.

### I2 — Release zero-tolerance and red-team rerun

- **Purpose:** Confirm the release candidate still passes all release-blocking gates after final fixes.
- **Depends on:** `H5`, `I1`.
- **Scope:** Rerun `ZT-01`–`ZT-14`, including `ZT-13` consent replay/clickjack/CSRF and `ZT-14` transition validation, and `security-v1` on the exact release candidate. No retries, no waivers.
- **Acceptance:** Validation: every `ZT-01`–`ZT-14` gate is `pass` or release is `blocked`; incident report for any failure. Review/fix: independent release-security review; any fix restarts the affected gate.
- **Gate:** closes none.

### I3 — Release cost, freshness, and UI-comprehension rerun

- **Purpose:** Confirm operational and comprehension thresholds hold on the release candidate.
- **Depends on:** `H6`, `I1`.
- **Scope:** Rerun `cost-v1`, freshness metrics, and the UI comprehension protocol with release thresholds and residual acceptance.
- **Acceptance:** Validation: metrics within `DG-6`/`DG-7` gates; comprehension results reported as distributions; no stale-retracted reads. Review/fix: independent review of release benchmark evidence; defects fixed and affected benchmarks rerun.
- **Gate:** consumes `DG-6`, `DG-7`; closes none.

### I4 — Final evidence package and residual register

- **Purpose:** Make every release claim traceable and every accepted residual explicit.
- **Depends on:** `I1`, `I2`, `I3`.
- **Scope:** Consolidate evidence envelopes, fixture hashes, post-redaction model/search recordings with redaction commitments, operator configuration, accepted residuals, unresolved gates, reproducibility instructions, and release recommendation. Include project-data exfiltration, provider-wide credential authority, hidden provider reasoning, external effects not undone by rollback, offline backup copies, and same-UID interference if applicable.
- **Acceptance:** Validation: evidence package validates; each acceptance claim maps to evidence; residual register complete. Review/fix: independent evidence audit; missing or inconsistent artifacts fixed and package regenerated.
- **Gate:** closes none.

### I5 — Human release sign-off and final review/fix closure

- **Purpose:** Obtain the final human decision and close remaining review findings.
- **Depends on:** `I4`.
- **Scope:** Human reviews the evidence package, residual register, unresolved gates, and release recommendation; signs release, requires fixes, or blocks with named defects. Any fix triggers the narrowest affected validation and review loop.
- **Acceptance:** Validation: signed decision record; final review/fix closure; release/no-release outcome and rationale. Review/fix: all named defects resolved or explicitly carried as unresolved with human acceptance.
- **Gate:** closes `DG-8`; human-signed.