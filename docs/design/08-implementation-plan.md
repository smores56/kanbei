## Plan

### 0. Plan conventions

Requirements are authoritative. This plan orders implementation work and is rebased onto the deployed split design in SDLC #509: a NIC-less Kanbei VM, offline `llama.cpp` inference, six closed channels, a static CSP shell, human-approved executable candidates, and Monty for agent-authored code. Runtime, deployment plane, isolation mechanism, and agent-facing language are decided and recorded in `07 §4.9.5` and `04 §8.2`; the remaining numeric thresholds stay decision-gated.

**Task completion contract.** Every task, including spikes, closes only when all of the following are attached:

1. **Validation:** the task-specific checks run through the conformance harness and emit an `EvidenceEnvelope` as defined by `07 §7.0`, bound to canonical head, fixture version, environment fingerprint, seeds, and post-redaction model recordings. Zero-tolerance checks never retry.
2. **Review/fix:** an independent reviewer checks scope against `00-requirements.md` and the normative validation section `07 §7`, confirms evidence sufficiency, and records findings. Defects are fixed and the affected checks are rerun before the task closes.
3. **No silent pass:** `inconclusive` or `blocked` does not become a default choice and does not unblock dependent tasks.

**Validation identifiers.** `07 §7` is the sole normative source for every validation identifier, fixture, protocol, pass criterion, and for `EvidenceEnvelope`; this plan defines none. `spec/06-validation-plan.md` is deleted and is a source for nothing.

| Identifier | Normative definition |
|---|---|
| `EvidenceEnvelope`, `Outcome`, test classes, spike pre-registration | `07 §7.0` |
| `ZT-01`–`ZT-18` zero-tolerance gates (incl. channel leak, CSP no-request, Todo cap, stale heartbeat) | `07 §7.1` |
| `I-01`–`I-20` canonical invariants | `07 §7.2` |
| `V1`–`V23` mechanical checks | `07 §7.2` |
| `AS-01`–`AS-12` acceptance scenarios | `07 §7.3` |
| `C-01`–`C-14` concurrency, approval, and override tests | `07 §7.3` |
| `R-01`–`R-07` replay, undo, and crash tests | `07 §7.3` |
| `P-01`–`P-10` poisoning, retraction, and quarantine tests | `07 §7.3` |
| `B-01`–`B-12` CAS, backup, restore, and erasure tests | `07 §7.3` |
| `U-01`–`U-10` UI structural tests | `07 §7.3` |
| `journey-v2`, `ingest-v1`, `ui-v1` sealed fixtures | `07 §7.3` |
| `S3`–`S8` spike protocols | `07 §7.4` |
| Protocols `LIM-1`, `UI-1`, `UI-2`, `ING-1`, `NET-1`, `LUKS-1`, `FRESH-1`, `JUDGE-1`, `IMG-1`, `D2`–`D8` | `07 §7.4`, `07 §8.1` |

`S1` (`LANG-1` + `NODE-DENO-1`) and `S2` (`DEP-1`) are retired with their decisions recorded; `S3` records the Monty containment and resource evidence. `S4` measures lanes, channels, and ceilings; `S5` measures reconciliation; `S6` measures comprehension and the CSP shell; `S7` measures ingest batches and freshness; `S8` measures the search filter.

Sealed datasets used by this plan:

| Dataset | Normative definition |
|---|---|
| `journey-v2` | `07 §7.3` sealed life-admin acceptance fixture for `AS-01`–`AS-12` |
| `ingest-v1` | `07 §7.4` `S7` batch and freshness workload |
| `ui-v1` | `07 §7.3` `U-01`–`U-10` comprehension baseline |
| `memory-v1`, `memory-holdout-v1` | `07 §7.4` reconciliation-quality protocol; holdout sealed and never tuned after labels freeze |
| `external-v1` | `07 §7.3` `AS-05` and `C-12` external-divergence scenarios |
| `security-v1` | `07 §7.1` `ZT-01`–`ZT-18` scanned-surface list, channel-leak probes, and search-filter suite |
| `cost-v1` | `07 §7.4` `LIM-1` workload reported against `07 §7.3` `AS-12` |
| `append-v1` | `07 §7.2` canonicalization and lifecycle corpus |

`DG-1`–`DG-8` are decision gates defined in this plan; they are not validation identifiers and close only on the `07 §7` evidence named in their producing task. A check for which `07 §7` defines no identifier is a plan-local check: the owning task states it, independent review decides it, and it is never cited as a gate.

### Decision gates

| Gate | Decision | Owner | Producing task | Blocks | Status |
|---|---|---|---|---|---|
| `DG-1` | Agent-facing execution language | Human | `A4` (retired) | `F4` | **Closed**: Monty. The model-dependent bake-off is retired because the deployment has one fixed local model; Monty's WASM backend, host-call mediation, ambient-capability denial, and time/memory bounds are recorded in `07 §4.9.5`. |
| `DG-2` | Node versus Deno for the TypeScript control plane | Human | `A1` | `B1` onward | **Closed**: `deno@2.9.2`; composite within the pre-registered margin, with Node winning append throughput. Domain code stays behind the runtime seam. |
| `DG-3` | Deployment plane and broker identity | Human | `A2` (retired) | — | **Retired**: the plane is one NIC-less qemu microVM with a root-only host; there is no broker and no host-side agent process (`04 §8.2`, `04 §8.7`). |
| `DG-4` | Inter-agent isolation mechanism | Human | `A3` | `E1` | **Closed**: Monty plus a cgroup-contained VM process tree; containment only, no OS-per-agent security claim (`04 §3.5`). |
| `DG-5` | Synchronous versus asynchronous index membership | Human | `B5` | `C1` | Open |
| `DG-6` | Resource ceilings and quality thresholds | Human | `H4` | `H4A`, `H6`, `I1` | Open |
| `DG-7` | Reconciliation quality thresholds | Human | `H2` | `I1` | Open |
| `DG-8` | Final release sign-off | Human | `I5` | release | Open |

Dependency ordering note: non-effectful agent-execution foundations precede the security substrate, but effectful execution begins only after the channel hosts, Monty host, and ceiling governor exist. The plan-local index-membership measurement `B5` follows the ledger/CAS skeleton because it measures those contracts. `S4` and `DG-6` necessarily follow measurable components, and `S6` follows the consent surface and the static shell. The origin-isolated frozen consent surface (`D3A`) precedes any human-facing approval flow, including candidate approval (`F3`).

### Traceability matrix

Requirement anchors cite `00-requirements.md` sections, not line numbers. Validation identifiers, fixtures, protocols, and pass criteria are defined only in `07 §7`. A row is satisfied only when every listed task is closed with that evidence and independent review. `07 §7.6` owns the requirement-to-gate mapping; this plan owns its `A0`–`A7`, `B`–`H`, and `I` tasks.

| Requirement (00 section) | Validation | Owning tasks |
|---|---|---|
| Product target; Security and external effects | `ZT-15`, `AS-01`, `AS-11`, `V20`, `V23` | `A7`, `E1`, `E2`, `H7`, `I1` |
| Architecture and authority | `ZT-02`, `ZT-11`, `I-01`, `I-05`, `V17`, `V22` | `B1`, `B4`, `D1`, `D3`, `E1` |
| Record and state semantics | `ZT-02`, `ZT-09`, `I-01`–`I-03`, `V13`, `C-01`, `C-02` | `B1`, `B2`, `B4`, `B6` |
| Semantic memory and reconciliation | `ZT-05`, `ZT-06`, `AS-06`, `AS-07`, `P-01`–`P-10`, `V15` | `C4`–`C7`, `C9`, `H2` |
| Intent, approval, and work | `ZT-04`, `ZT-08`, `ZT-13`, `AS-02`–`AS-05`, `C-06`–`C-11`, `V5`, `V8` | `D1`–`D4`, `D9` |
| Agent loops and durability | `ZT-04`, `ZT-12`, `C-03`, `R-05`, `R-06`, `V2`–`V4` | `D5`, `D7`, `D8`, `E3`, `H4` |
| Replay and undo | `ZT-07`, `AS-10`, `R-01`–`R-07`, `V9` | `D8`, `H8` |
| Self-modification | `ZT-02`, `ZT-10`, `ZT-14`, `AS-08`, `I-13`, `I-14`, `V7`, `V22` | `E1A`, `F1`–`F5` |
| Security and external effects | `ZT-01`, `ZT-04`, `ZT-15`, `ZT-16`, `AS-09`, `AS-11`, `V16`, `V20` | `E1`–`E5`, `F5`, `H5` |
| Tools and providers | `ZT-01`, `ZT-17`, `AS-09`, `AS-12`, `S4`, `S8`, `V10` | `D6`, `F4`, `G4`, `H4` |
| Interface and multimodal state | `ZT-06`, `ZT-12`, `ZT-13`, `ZT-16`, `U-01`–`U-10`, `S6` | `C7`, `G1`–`G3`, `H6` |
| Persistence, freshness, and backup | `ZT-09`, `ZT-12`, `ZT-18`, `AS-11`, `AS-12`, `B-01`–`B-12`, `S7` | `B3`–`B5`, `C8`, `H1`, `H3`, `H7` |
| Validation and deferred decisions | all `ZT-*`, all `AS-*`, `EvidenceEnvelope` | `A0`, `A5`–`A7`, `B6`, `C10`, `D9`, `E5`, `G5`, `H5`, `H9`, `I1`–`I5` |

## Phase A — Evidence substrate and decision-gated spikes

### A0 — Evidence harness, sealed fixtures, and replayable test substrate

- **Purpose:** Make every later result evidence-producing rather than assertion-producing.
- **Depends on:** none.
- **Scope:** Conformance runner; `EvidenceEnvelope`; test classes `unit`, `property`, `conformance`, `chaos`, `redteam`, `spike`, `bench`; virtual clock and synthetic network; post-redaction recorded model/search I/O; sealed fixture versions and hash manifests; deterministic rerun; environment fingerprint; artifact capture; failure classification.
- **Acceptance:** Validation: `EvidenceEnvelope` dry-run journey; `07 §7.0` schema validation; deterministic re-run; fixture hash manifest; failure-class recording. Review/fix: independent review of harness determinism, fixture sealing, and evidence completeness; defects fixed and checks rerun.
- **Gate:** none.

### A1 — Spike S1: Node versus Deno control-plane bake-off (closed; retained as evidence)

- **Purpose:** Select the TypeScript control-plane runtime. **Status: closed** selecting `deno@2.9.2`; this task is retained for its evidence and is not reopened.
- **Depends on:** `A0`.
- **Scope:** Node and Deno arms using the same control-plane code where feasible, the same OS sandbox, the same SQLite ledger, and the same supervisor seam. Measure cold/warm start, RSS, event-loop p50/p95 under append/read load, SQLite driver correctness and single-writer behavior, worker/process isolation, signal handling, systemd integration, OpenTelemetry/tracing, packaging size, and permission behavior. Run the security probes against both.
- **Acceptance:** Validation: `S1` (`07 §7.4` `LANG-1` + `NODE-DENO-1`) pre-registration for the `NODE-DENO-1` arm, paired comparison, traces, decision matrix, hard-fail list, rejected alternative, and residual risks; the same `S1` protocol is shared with `A4`. Review/fix: independent review of protocol fidelity and resource measurements; decision record corrected before publication.
- **Gate:** closed `DG-2`; human-signed. Re-run the comparison only after the ledger sequencer and worker model exist, as the gate record requires.

### A2 — Spike S2: deployment plane and broker identity (retired)

- **Purpose:** ~~Determine whether pure user-space deployment can protect broker secrets~~ **Retired.** The split removes the broker and the host-side agent process; the plane is fixed (`04 §8.2`). The task closes with a retirement record that maps every probe to its replacement control and states which host-security facts remain unmeasured.
- **Depends on:** `A0`.
- **Scope:** Retirement record. For each pre-split probe (user namespaces, ptrace/Yama and `/proc` visibility, Landlock ABI, `ProtectHome`/`ProtectSystem`, cgroup delegation, broker identity, secret reachability, loopback exposure) state the fact, the replacement control in the deployed VM (`04 §8.1`–`§8.7`), and what remains unmeasured. Record the rootless-namespace and shared-store evidence as historical, not as a claim about this deployment.
- **Acceptance:** Validation: retirement record complete, every probe mapped, and the `DG-3` retirement signed; residuals (host root reads the volume, vTPM seal may be nominal) restated. Review/fix: independent security review of the mapping; defects fixed and record reissued.
- **Gate:** closed `DG-3` as retired; human-signed retirement record.

### A3 — Spike S3: agent-program containment under Monty

- **Purpose:** Record the containment and resource behavior of agent-authored programs under Monty, and state explicitly that no security property is claimed for the interpreter.
- **Depends on:** `A0`.
- **Scope:** Monty WASM backend start and execution; async host-call mediation; ambient filesystem, network, and subprocess denial with zero network requests; `while True` and unbounded-allocation bounds with fresh-session recovery; child-timeout reporting; per-sandbox cgroup memory, pids, CPU, wall-clock, and scratch isolation; kill and orphan reaping; process-wide OOM behavior. Container-per-agent stays excluded; a microVM-per-agent is out of scope because the VM is the boundary.
- **Acceptance:** Validation: `S3` (`07 §7.4`) containment matrix at target concurrency on `security-v1`; explicit statement that no security property is claimed and that engine compromise and process-wide OOM are residual. Review/fix: independent review of containment claims and operability costs; defects fixed and rerun.
- **Gate:** closes `DG-4`; human-signed.

### A4 — Spike S1: agent-facing execution-language bake-off (retired)

- **Purpose:** ~~Select the programmable execution language~~ **Retired.** Monty is fixed by the split design (`07 §4.9.5`). The task closes with the recorded decision, the retained local measurements, and an explicit statement that no live model comparison exists.
- **Depends on:** `A0`.
- **Scope:** Arms: Monty, TypeScript, Lua/Luau, Bash, full CPython. Use the ten-task workload suite, host-call latency at 10 ms, 100 ms, and 1 s, at least two models per provider including a smaller model, and 30 trials per cell at temperature 0. Measure success and evidence coverage, tokens, turns, tool calls, retries, p50/p95 latency, startup and peak memory, snapshot/restore, progressive disclosure at increasing tool counts, security probes, and integration surface.
- **Acceptance:** Validation: `S1` (`07 §7.4` `LANG-1` + `NODE-DENO-1`) pre-registered non-inferiority and cost/latency/operability rules for the `LANG-1` arm, traces, failure taxonomy, selected language or explicit “no selection”; the same `S1` protocol is shared with `A1`. Review/fix: independent review of pre-registration and analysis; violations corrected before the gate.
- **Gate:** closed `DG-1` with the design decision; human-signed record.

### A5 — Gate consolidation and baseline freeze

- **Purpose:** Convert spike outcomes into fixed downstream contracts before implementation.
- **Depends on:** `A1`, `A2`, `A3`, `A4`.
- **Scope:** Record `DG-1` through `DG-4` with their status (closed, closed, retired, closed); selected runtime, execution language, deployment plane, and containment; rejected alternatives; accepted residuals (engine compromise, process-wide OOM, search leakage); provisional operator-envelope schema for lanes, ingest, CAS, Todo, chat, and search; harness configuration. Prohibit silent defaults.
- **Acceptance:** Validation: signed gate records and configuration manifests; cross-check that no dependent task can start on an unrecorded choice. Review/fix: independent review of gate records and dependency implications; inconsistencies fixed and records reissued.
- **Gate:** consumes `DG-1`–`DG-4`.

### A6 — Spike S4 and S6 pre-registration: resource, cost, freshness, and comprehension protocols

- **Purpose:** Pre-register threshold measurement before values are needed, without inventing numbers.
- **Depends on:** `A0`, `A5`.
- **Scope:** Define `cost-v1` workload, repetitions, comparison rules, bootstrap confidence intervals, cold-versus-warm separation, enforced-ceiling checks, quality metrics, and the `ui-v1` UI comprehension protocol. Do not set numeric thresholds.
- **Acceptance:** Validation: `S4` (`07 §7.4` `LIM-1`, `cost-v1`) and `S6` (`07 §7.4` `UI-1`, `UI-2`, `ING-1`, `ui-v1`) pre-registration records validate against the harness under `07 §7.0` rule 4; dependent benchmark tasks can reference them. Review/fix: independent review of metric definitions and decision rules; ambiguities fixed before benchmark implementation.
- **Gate:** prepares `DG-6`; closes no decision yet.

---

### A7 — Seal `journey-v2` and `ingest-v1` fixtures

- **Purpose:** Give the rebased acceptance journey and the ingest batch contract sealed fixtures before dependent tasks reference them.
- **Depends on:** `A0`, `A5`.
- **Scope:** Seal the life-admin `journey-v2` fixture (one batch per origin, a stale source, an external commit batch, a declarative activation, an executable candidate awaiting approval, a Todo cap case, a blocked search query, and a CSP no-request case) and the `ingest-v1` workload (batch sizes and rates, gaps, suppressions, outage backlog). Publish hashes in the fixture registry and record the fixture versions in the harness.
- **Acceptance:** Validation: fixture manifests hash-verified; `07 §7.0` rule 3 sealing evidence; negative case per fixture (tampered entry, reordered sequence, missing heartbeat) rejects. Review/fix: independent review that no task cites an unsealed fixture; defects fixed and fixtures resealed.
- **Gate:** prepares `DG-6`, `DG-7`; closes none.

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
- **Scope:** Process topology under `DG-2` and the fixed VM plane; trusted core/sequencer, channel hosts, Monty host and sandbox supervisor, lane governor, static UI client, consent surface, watchdog; loopback-only API; startup integrity checks including the channel manifest; health/readiness. Vertical slice: fixture observation → CAS bytes → manifest → append → operational read → minimal web or CLI view → restart and reread.
- **Acceptance:** Validation: end-to-end evidence envelope; `B-01`/`B-02` subset; read-your-writes; restart; loopback binding; integrity-failure refusal. Review/fix: independent review of process boundaries and skeleton evidence; defects fixed and rerun.
- **Gate:** consumes `DG-2`; closes none.

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

### C2 — Ingest batch contract and ingest-side adapters

- **Purpose:** Consume one-way ingest batches as observations with provenance, not authority, and keep the ingest-side adapter contract closed.
- **Depends on:** `B3`, `B4`, `B6`.
- **Scope:** `IngestBatchManifest` and `IngestEntry` validation; age decryption with the in-VM identity; ordered `(origin, sequence)` acceptance with gap detection; CAS publication before observation visibility; one coverage receipt per batch; idempotency by `batchDigest`; suppression accounting; missing grace; failure mapping; per-source freshness heartbeat. The ingest-side `git`/`http`/`search`/`fs` adapters are exercised through recorded batches. Adapters and batch producers are never canonical writers.
- **Acceptance:** Validation: `AS-01`, `ZT-18`; provenance, coverage, idempotent re-ingest, gap and suppression recording, crash after each publication step, stale and unknown heartbeat behavior. Review/fix: independent review of evidence ordering and absence behavior; defects fixed and rerun.
- **Gate:** closes none.

### C3 — Git material and external divergence

- **Purpose:** Record external commits immediately without clobbering speculative history.
- **Depends on:** `C2`.
- **Scope:** Git ref, commit, and rewrite entries inside batches; commit/blob capture; changed paths; force-push and abandonment; explicit conflict assessment; basis checks against recorded refs; external commits as observations, never approvals. There is no working tree in the VM and no write path to a repository, so no-clobber is structural for repositories and remains a gate for Todo and ledger basis.
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
- **Scope:** Online SQLite point-in-time snapshot immediately before the host image snapshot; CAS consistency; checkpoint digests; restore reference verification and derived-index rebuild; erasure epoch covering online and controlled backup artifacts; retention separation; residual declaration for offline copies. Host snapshot mechanics are `IMG-1`; the in-VM ordering is not deferred.
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

### D6 — Local lane policy and exact model I/O capture

- **Purpose:** Route task classes to local lanes and capture exact available model evidence without promising regeneration.
- **Depends on:** `B3`, `D5`.
- **Scope:** Human-owned task policies mapping task classes to the chat lane or the background pool; lane request/response contract against `llama.cpp`; lane-seconds reservation and accounting; chat starvation protection; exact post-redaction semantic and wire capture of request, response, params, output events, and exposed reasoning artifacts; reasoning absence explicit; retries as new model calls; recorded-I/O replay path. There is no remote provider, no fallback chain, and no credential.
- **Acceptance:** Validation: `V10`; capture digests; zero credential-bearing bytes in any artifact, CAS, or canonical surface (`ZT-01`); timeout/malformed-output/lane-unavailable tests with a fake lane server; reasoning absence; replay performs no model call; chat lane never preempted by background work. Review/fix: independent review of I/O completeness, redaction commitments, and replay prohibition; defects fixed and rerun.
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

## Phase E — Security, channels, and effectful execution

### E1 — Capability contracts, channel hosts, Monty host, and fail-closed verification

- **Purpose:** Enforce the closed channel boundary and contain agent-authored code.
- **Depends on:** `A5` (`DG-4`), `B2`, `B4`.
- **Scope:** Channel capability contracts (`channel.inbox`, `vault`, `todo`, `chat`, `csr`, `search`, `lane.invoke`); the six channel hosts and their schemas; mount manifest and cap enforcement; `SandboxSpec.channels`; enforcement class and post-start verification including the interface, mount, and socket set; per-sandbox cgroups; start-token and peer attribution; Monty host-call stubs generated from the manifest; no canonical database, trust-root file, or secret mounted into a sandbox.
- **Acceptance:** Validation: `ZT-02`, `ZT-04`, `ZT-10`, `ZT-15`, `V20`, `V23`; fail-open test; `ip link` shows `lo` only; mount and socket set equal the manifest; resource enforcement; undeclared-stub denial. Review/fix: independent security review; defects fixed and zero-tolerance checks rerun.
- **Gate:** consumes `DG-4`; closes none.

### E1A — Frozen activation-supervisor stub

- **Purpose:** Provide the sole authority path for activation and activation-receipt records before full candidate validation exists, so declarative activation never appends supervisor-only records itself.
- **Depends on:** `E1`, `B6`.
- **Scope:** Frozen supervisor process identity and launch contract; append path for activation and activation-receipt components; atomic flip and revert of one pre-registered scope pointer; startup verification that agent and plugin channels cannot append those component classes; append-only activation history. Only a sealed synthetic candidate: no mutable code loading, capability diff, health check, or external-effect rollback. Explicit retirement list for `F3`.
- **Acceptance:** Validation: `ZT-02`, `ZT-10` stub-scope subset; agent/plugin append attempts denied; pointer flip and revert recorded as supervisor-authored receipts; trust-root mutation refused. Review/fix: independent review of the stub's authority boundary and retirement list; defects fixed and zero-tolerance checks rerun.
- **Gate:** consumes `DG-4`; closes none.

### E2 — Channel hosts and the closed manifest

- **Purpose:** Make the six channels the only byte paths, with caps, receipts, and fail-closed behavior.
- **Depends on:** `E1`.
- **Scope:** Inbox reader (age decryption, ordered sequences, gap and suppression records, idempotency by batch digest); Vault reader (read-only, symlink refusal); Todo writer (per-write and per-mount cap, `todo_cap_exceeded`, receipts); Chat server (JSON routes, in-VM TLS, passkey plus Access identity, byte cap on responses); CSR handler (one CN, one key algorithm, one SAN set); Search client (`{query, limit}`, no key, no URL control). Audit intent before each channel operation and a receipt after. No broker, no enrollment, no credential, no HTTP client.
- **Acceptance:** Validation: `AS-09`, `ZT-01`, `ZT-15`, `ZT-17`; channel schema violations, undecryptable batch, cap breach, judge/DLP block, and host-socket failure all deny closed with receipts; no process in the VM holds a provider key. Review/fix: independent security review of channel ordering and cap enforcement; defects fixed and zero-tolerance checks rerun.
- **Gate:** consumes `DG-4`; closes none.

### E3 — Effectful execution through channels

- **Purpose:** Connect the effect journal to real channel capabilities and hard-policy plumbing.
- **Depends on:** `D7`, `E1`, `E2`.
- **Scope:** Inbox, Vault, Todo, Chat, CSR, Search, and lane effects; hard ceiling, budget, tier, and receipt checks before dispatch; idempotency keys; `outcome_unknown`; compensation; receipts; denial records; no effect outside the sandbox, channel manifest, global ceiling, or frozen-root policy. Admission uses explicitly synthetic test-scoped limits from a sealed fixture; this task sets no operator ceiling and asserts no approved value, and effects stay confined to the fixture.
- **Acceptance:** Validation: `I-09`, `I-12`, `C-14`, `ZT-04` under synthetic limits; crash after attempt; compensation as new effect; policy denial evidence; enforcement-point inventory for `H4A`. Review/fix: independent review of admission ordering and receipt completeness; defects fixed and zero-tolerance checks rerun.
- **Gate:** closes none.

### E4 — Security red-team core

- **Purpose:** Demonstrate the hostile-agent threat model against the implemented boundary.
- **Depends on:** `E1`, `E2`, `E3`.
- **Scope:** Hostile agent/tool code, hostile dependencies, prompt injection through batches and search results, vault theft, canonical DB write, Monty escape attempts, channel leaks (NIC, undeclared share, undeclared vsock port), Todo cap abuse, CSP remote-request and autolink attempts, consent forgery, approval bypass, audit forgery, persistence, fail-open, and trust-root mutation.
- **Acceptance:** Validation: `ZT-01`, `ZT-02`, `ZT-04`, `ZT-10`, `ZT-15`, `ZT-16`, `ZT-17`; `security-v1`; accepted residuals recorded, especially search leakage, approved-code reach, and Monty engine compromise. Review/fix: independent red-team review; every finding fixed or explicitly accepted with evidence before phase close.
- **Gate:** closes none.

### E5 — Effectful agent-execution conformance review

- **Purpose:** Verify real effects do not bypass approval, channel, or receipt contracts.
- **Depends on:** `D9`, `E3`, `E4`.
- **Scope:** Run `AS-04`, `AS-05`, `AS-09`, `AS-12`, `C-12`, and `C-14` with real channel hosts. Check approval scope coverage, external-change recording, no-clobber behavior for Todo, and effect receipts.
- **Acceptance:** Validation: conformance evidence bundle; zero-tolerance checks never retried. Review/fix: independent review record; defects fixed; affected evidence rerun before phase close.
- **Gate:** closes none.

## Phase F — Self-modification

### F1 — Declarative self-modification (live)

- **Purpose:** Allow safe live mutation of context lenses, view catalogs, themes, keybinds, settings, and disclosure rules.
- **Depends on:** `D1`, `D3A`, `D4`, `C7`, `E1`, `E1A`.
- **Scope:** Versioned declarative documents; schema and budget validation; authorized-source and component-version checks; ordered and total disclosure rules; no remote reference, no code, no new origin; rejection of anchor, decision-affordance, or consent-link removal; next-turn activation with the current turn pinned; rollback to the previous candidate; activation receipt authored only by the `E1A` supervisor stub; visibility/hidden diff. Human decision views remain outside agent context scope.
- **Acceptance:** Validation: `AS-08` declarative lane; invalid-document rejection; next-turn pinning; previous-candidate retention; coverage honesty; activation and revert receipts carry only the `E1A` authority path. Review/fix: independent review of declarative validation and hidden-evidence effects; defects fixed and rerun.
- **Gate:** closes none.

### F2 — Plugin kernel, scopes, and scoped state

- **Purpose:** Isolate arbitrary plugin code and its mutable state from the canonical ledger.
- **Depends on:** `F1`, `E1`.
- **Scope:** Scope graph; plugin manifest; `PluginContext`; required/optional dependencies; separate scoped stores; rebuildable and checkpointed durability; canonical append/query handles bound to scope; no direct SQLite handle; plugin host inside the Monty host process boundary; co-location only within the same scope and trust class.
- **Acceptance:** Validation: `I-13`; manifest rejection for undeclared capabilities or state; plugin-state rebuild; direct-SQLite denial; undeclared channel stub denial. Review/fix: independent review of scope isolation and state durability; defects fixed and zero-tolerance checks rerun.
- **Gate:** closes none.

### F3 — Candidate staging, validation, capability diff, human approval, and rollback

- **Purpose:** Let executable code activate only through a human approval receipt, with diffs, health checks, and rollback.
- **Depends on:** `F2`, `E3`, `E1A`, `D3A`.
- **Scope:** Candidate generations; dependency lock against image artifacts; manifest/schema validation; static capability scan; sandboxed dynamic conformance under Monty with channels denied; migration dry-run on shadow state; security probes; capability and dependency diff; candidate approval request in the decision queue rendered by the frozen consent surface; approval receipt binding candidate digest, diff, scope, and expiry; consumption on flip; shadow state; standby boot with effects gated; drain; atomic pointer flip; post-flip health; rollback; previous-candidate retention; frozen trust root untouched. Retire the `E1A` stub and carry out its retirement list.
- **Acceptance:** Validation: `AS-08` executable lane; `I-14`, `ZT-10`, `ZT-08`; activation crash matrix; a candidate cannot activate without a matching approval receipt, and a changed digest invalidates one; an agent-authored approval is rejected with `AUTHORITY_DENIED`; external effects not rolled back by pointer flip; forward-only rollback refusal; `E1A` retirement evidence. Review/fix: independent review of approval binding and activation ordering; defects fixed and zero-tolerance checks rerun.
- **Gate:** consumes `DG-5` where indexes are involved; closes none.

### F4 — Agent-visible programmable tool surface on Monty

- **Purpose:** Expose one discovery-and-execution surface instead of per-tool schemas.
- **Depends on:** `DG-1`, `F3`.
- **Scope:** `search`, `list`, `describe`, and `execute`; versioned tool descriptors; `call` mode; `program` mode under Monty with generated typed host-call stubs; result envelope; structured errors; streaming and cancellation; host-call budgets; conflict, transcript, and terminate support; capability denial. `RuntimeId` has exactly one value.
- **Acceptance:** Validation: discovery conformance; deterministic ordering and pagination; host-call behavior; ambient-capability denial inside a program; capability denial; `V10` host-call accounting. Review/fix: independent review of descriptor versioning and host-call accounting; defects fixed and rerun.
- **Gate:** consumes `DG-1`; closes none.

### F5 — Self-modification crash, trust-root, and external-effect tests

- **Purpose:** Prove activation failure cannot corrupt authority or silently undo external effects.
- **Depends on:** `F3`, `F4`.
- **Scope:** Crash before and after pointer flip; health failure and rollback; rollback blocked by forward-only migration; trust-root write attempts; approval replay and digest mismatch; candidate quarantine; external-effect residual after rollback; agent editing its own lens without affecting human decision views; declarative attempt to remove an anchor or the consent link.
- **Acceptance:** Validation: `ZT-10`, `ZT-08`, full `AS-08`; activation crash matrix; external-effect residual report. Review/fix: independent review of rollback and residual semantics; defects fixed and zero-tolerance checks rerun.
- **Gate:** closes none.

## Phase G — Static UI, local lanes, and multimodal state

### G1 — Static CSP shell and core graph, timeline, and decision views

- **Purpose:** Provide the stable human interface over renderer-neutral state without giving injected content a network path.
- **Depends on:** `D3`, `D3A`, `D4`, `C7`, `F1`.
- **Scope:** Static human-owned bundle built from the pinned commit; the CSP of `07 §4.11.7`; JSON-only API; coordinated graph, delta timeline, and global decision queue; coverage watermarks; contested, retracted, suspect, stale, and current distinctions; approval comprehension; undo visibility; accessibility tree and text fallback; model text rendered as text nodes with no HTML injection and no auto-linking.
- **Acceptance:** Validation: `U-01`–`U-10` structural assertions; `ZT-16` planted image/link/CSS/iframe/autolink cases produce zero requests and zero navigations; presentation-replay subset; renderer-neutral catalogs. Review/fix: independent UI review against comprehension invariants; defects fixed and rerun.
- **Gate:** closes none.

### G2 — AG-UI adapter, A2UI catalogs, graph components, and artifact boundary

- **Purpose:** Keep canonical semantics internal while interoperating with presentation protocols.
- **Depends on:** `G1`.
- **Scope:** AG-UI run/state/interrupt adaptation; ordinary A2UI catalog rendering; custom graph components; CSP-locked artifact region with no remote references and no credentials; no canonical UI state; stale-delta rejection; interrupt batching as a graph constraint.
- **Acceptance:** Validation: `U-09`; protocol conformance; artifact isolation; stale-delta rejection; batched interrupt resume; no catalog can introduce an origin or script. Review/fix: independent protocol review; defects fixed and rerun.
- **Gate:** closes none.

### G3 — Multimodal ingestion, previews, and derived-artifact separation

- **Purpose:** Content-address arbitrary files while keeping raw objects distinct from derivations.
- **Depends on:** `C2`, `B3`, `G1`.
- **Scope:** Text, code, images, screenshots, and PDFs arriving in batches; provenance and previews served as local bytes; OCR, text, and metadata derivations; audio/video stored without full semantic indexing; canonical raw objects distinct from transcripts, OCR, embeddings, and thumbnails; retraction-aware derived artifacts.
- **Acceptance:** Validation: provenance drill-down; derived-versus-raw distinction; preview correctness; stale derived artifacts after retraction; previews never load a remote resource. Review/fix: independent review of provenance and derivation invalidation; defects fixed and rerun.
- **Gate:** closes none.

### G4 — Local lanes and search channel integration

- **Purpose:** Connect the real inference path and the capped search path behind the frozen contracts.
- **Depends on:** `D6`, `E2`, `E3`, `C2`.
- **Scope:** `llama.cpp` serving the owner's model with one 128k chat lane and three 32k background lanes from a shared q8_0 KV pool, MTP off; lane availability and KV health; CPU embeddings and reranker; the Search VM behind the Search channel with DLP, judge, provider limits, stripping, and root-only query logging; typed failure mapping; quota accounting; freshness of the search budget.
- **Acceptance:** Validation: recorded lane runs; `AS-09`; `ZT-01`; `ZT-17` search budget; DLP/judge block evidence; timeout and lane-unavailable tests; no content-bearing observation on failure; query log root-only. Review/fix: independent review of lane policy and search filtering; defects fixed and rerun.
- **Gate:** closes none.

### G5 — UI and provider phase review

- **Purpose:** Verify presentation neutrality, channel provenance, multimodal separation, and accessibility before hardening.
- **Depends on:** `G1`–`G4`.
- **Scope:** Cross-review of renderer neutrality, protocol adaptation, artifact isolation, channel provenance, derived-artifact invalidation, CSP assertions, and comprehension invariants.
- **Acceptance:** Validation: conformance rerun of `U-*` and channel tests; no open provenance findings; `ZT-16` rerun. Review/fix: independent review record; defects fixed; affected evidence rerun before phase close.
- **Gate:** closes none.

## Phase H — Hardening and operational readiness

### H1 — Chaos, crash, and fault-injection matrix

- **Purpose:** Prove recovery behavior across ledger, CAS, loops, effects, activation, and backup.
- **Depends on:** `B2`, `B3`, `C8`, `D5`, `D7`, `E3`, `F3`.
- **Scope:** `SIGKILL`, `ENOSPC`, `EIO`, fsync failure, SQLite busy/corruption detection, OOM, fork bomb, clock jump, duplicate and out-of-order inbox delivery, channel-host failure and restart, lane failure, search budget exhaustion, CAS GC concurrent with read, and structured-contract violations. Cover every crash-matrix row.
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
- **Scope:** Run `cost-v1`: full journey, idle period, fan-out burst, reconciliation backlog, external conflict, and backup/restore. Measure CPU, RSS, pids, IO, disk growth, CAS growth, lane seconds and lane latency, chat rate, search queries and budget, ingest bytes and batches per source, concurrency, and latency. Set GPU-lane, lane-second, ingest, CAS, state, Todo, chat, and search ceilings; per-loop soft budgets and override rules; memory, CPU, task, IO, and admission limits; sandbox and channel concurrency; freshness objectives; quality thresholds for reconciliation and UI comprehension.
- **Acceptance:** Validation: `S4` (`07 §7.4` `LIM-1`) pre-registered workload and repetitions; bootstrap confidence intervals; enforcement evidence; hard fail if a ceiling cannot be observed or enforced. Review/fix: independent review of headroom calculation and enforcement; defects fixed and benchmark rerun.
- **Gate:** closes `DG-6`; human-signed.

### H4A — Effect enforcement under approved ceilings

- **Purpose:** Verify the `E3` admission path against the human-approved values from `DG-6` without choosing any value.
- **Depends on:** `H4`, `E3`.
- **Scope:** Load the approved operator configuration record; replay the `E3` enforcement-point inventory with the real ceilings; confirm every configured lane, lane-second, ingest, CAS, state, Todo, chat, search, token, concurrency, resource, sandbox, and channel limit denies before dispatch and is observable; record the synthetic-to-approved mapping.
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
- **Scope:** Lane and channel cost metrics; batch freshness, ingest lag, gap and suppression rates, queue age, decision-to-effect, reconciliation catch-up, index lag, and stale retracted reads; UI comprehension protocol with ten scripted questions against a raw-ledger baseline; CSP no-request assertions; presentation replay; accessibility.
- **Acceptance:** Validation: `S6` (`07 §7.4` `UI-1`, `UI-2`, `ING-1`) pre-registered protocol; distributions and confidence intervals; thresholds from `DG-6` and `DG-7`; zero stale retracted reads. Review/fix: independent review of benchmark protocol and comprehension analysis; defects fixed and rerun.
- **Gate:** consumes `DG-6`, `DG-7`; closes none.

### H7 — Deployment packaging and operational readiness

- **Purpose:** Package the selected deployment plane for reproducible operation, backup, and rollback.
- **Depends on:** `A5` (`DG-4`), `E1`, `E2`, `C8`.
- **Scope:** Image packaging from the flake-pinned commit; guest module (no NIC, VFIO, swtpm, LUKS, virtiofs shares); host plumbing (relay, vsock, root-only artifacts); state path; image backup registration; loopback binding; WARP private route, in-VM TLS, and passkey posture; watchdog and freshness alerting; restart and rollback path; packaged host and guest checks; no fail-open launch; the image build refuses an unpinned or dirty pin.
- **Acceptance:** Validation: packaged host-check rerun; restore from backup; rollback evidence; fail-open refusal. Review/fix: independent operations review; defects fixed and checks rerun.
- **Gate:** consumes `DG-4`; closes none.

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

### I1 — Life-admin journey acceptance

- **Purpose:** Run the approved-in-principle journey end to end.
- **Depends on:** `H9`, `DG-6`, `DG-7`.
- **Scope:** `AS-01`–`AS-12` on the sealed `journey-v2` fixture: one batch per source plus a Git batch with coverage and heartbeats; a stale source labelled and alerted; intent exploration and one structural approval through the frozen consent surface; nested deltas with evidence-gated completion; an external commit from a later batch; memory reconciliation, retraction, and quarantine; one declarative change live and one executable candidate held for human approval then activated with rollback; Todo read/write with a cap refusal; one chat turn over the WARP route with in-VM TLS and a CSP no-request assertion; one capped search query and one blocked query; replay and undo; image restore with the recovery key.
- **Acceptance:** Validation: every scenario executes; all assertions recorded; evidence envelope bound to canonical head, fixture version, and post-redaction recordings. The executable lane passes through a human approval receipt with a capability diff, one pointer flip, a health check, and rollback, while any frozen-kernel change remains generation-gated and denied to agents (`ZT-10`). Review/fix: independent end-to-end review; defects fixed and the affected scenarios rerun.
- **Gate:** consumes `DG-6`, `DG-7`; closes none.

### I2 — Release zero-tolerance and red-team rerun

- **Purpose:** Confirm the release candidate still passes all release-blocking gates after final fixes.
- **Depends on:** `H5`, `I1`.
- **Scope:** Rerun `ZT-01`–`ZT-18` on the exact release candidate, including channel leak, CSP no-request, Todo cap, stale heartbeat, consent replay, clickjack, CSRF, and lifecycle-transition validation, with `security-v1`. No retries, no waivers.
- **Acceptance:** Validation: every `ZT-01`–`ZT-18` gate is `pass` or release is `blocked`; incident report for any failure. Review/fix: independent release-security review; any fix restarts the affected gate.
- **Gate:** closes none.

### I3 — Release ceiling, freshness, and comprehension rerun

- **Purpose:** Confirm operational and comprehension thresholds hold on the release candidate.
- **Depends on:** `H6`, `I1`.
- **Scope:** Rerun `cost-v1`, `ingest-v1` freshness metrics, and the UI comprehension protocol with release thresholds and residual acceptance.
- **Acceptance:** Validation: metrics within `DG-6`/`DG-7` gates; comprehension results reported as distributions; zero stale sources presented as current. Review/fix: independent review of release benchmark evidence; defects fixed and affected benchmarks rerun.
- **Gate:** consumes `DG-6`, `DG-7`; closes none.

### I4 — Final evidence package and residual register

- **Purpose:** Make every release claim traceable and every accepted residual explicit.
- **Depends on:** `I1`, `I2`, `I3`.
- **Scope:** Consolidate evidence envelopes, fixture hashes, post-redaction lane and search recordings, operator configuration, accepted residuals, unresolved gates, reproducibility instructions, and release recommendation. Include search leakage, approved-code reach, Monty engine compromise and OOM, Chat/Todo reach, hidden reasoning, external effects not undone by rollback, offline backup copies, and the nominal vTPM seal.
- **Acceptance:** Validation: evidence package validates; each acceptance claim maps to evidence; residual register complete. Review/fix: independent evidence audit; missing or inconsistent artifacts fixed and package regenerated.
- **Gate:** closes none.

### I5 — Human release sign-off and final review/fix closure

- **Purpose:** Obtain the final human decision and close remaining review findings.
- **Depends on:** `I4`.
- **Scope:** Human reviews the evidence package, residual register, unresolved gates, and release recommendation; signs release, requires fixes, or blocks with named defects. Any fix triggers the narrowest affected validation and review loop.
- **Acceptance:** Validation: signed decision record; final review/fix closure; release/no-release outcome and rationale. Review/fix: all named defects resolved or explicitly carried as unresolved with human acceptance.
- **Gate:** closes `DG-8`; human-signed.

---

## Phase 4 registry tasks

The rebased build is registered under SDLC #509 as T24–T39. Each registry task maps onto the plan tasks above; it does not restate them.

| Registry | Title | Plan tasks |
|---|---|---|
| #598 (T24) | Control plane, ledger, and Monty session host | `A0`, `B1`–`B4`, `E1` |
| #599 (T31) | Capability mediation: manifest, stubs, mounts, fail-closed start | `E1`, `E1A` |
| #600 (T25) | Inbox channel: age batch reader, ordering, coverage, heartbeat | `C2`, `E2` |
| #601 (T26) | Vault channel: read-only citation path | `E2`, `G3` |
| #602 (T27) | Todo channel: capped writer with receipts | `E2` |
| #603 (T28) | Chat channel: JSON API, in-VM TLS, route schema | `E2`, `H7` |
| #604 (T29) | CSR channel: schema-locked renewal and install | `E2`, `H7` |
| #605 (T30) | Search client and search-channel contract | `E2`, `G4` |
| #606 (T32) | Freshness watcher and staleness surfacing | `C2`, `H6` |
| #607 (T33) | Declarative views and the static CSP renderer | `F1`, `G1` |
| #608 (T34) | Executable-candidate approval and rollback | `F3`, `F5` |
| #609 (T35) | CAS and ledger reconciliation on the existing implementation | `B1`–`B3` |
| #610 (T36) | Ingest batch contract fixtures | `A7`, `C2` |
| #611 (T37) | Journey-v2 life-admin fixtures and benchmarks | `A7`, `H6`, `I1` |
| #612 (T38) | Packaging, image build, and flake pin | `E1`, `H7` |
| #613 (T39) | Adversarial channel-leak and CSP suite | `E4`, `H5`, `I2` |

Registry tasks marked `Part of #509` close under the same task completion contract as plan tasks; T37–T39 are tracked in the plan body rather than as native sub-issues because #509 is at GitHub's 100-sub-issue limit.
