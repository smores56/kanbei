# Kanbei Implementation Review — Findings Ledger and Decision Record

Date: 2026-08-31. Review worktree: `smores/implementation-review` (branched from `main@d84908c`).

Scope: full review of all 24 workspace crates against the ratified design corpus
(`docs/high-level-architecture.md`, `docs/architecture.md`,
`docs/design-review-handoff.md`, `docs/review-reconciliation.md`) on the axes
**fidelity → effectiveness → craft**. Seven read-only audit lanes covered the
kernel durables, wasm substrate, module lifecycle, session spine, memory/context,
UI/telemetry/GC, and the test surface; every finding below cites doc line and
file:line evidence. Milestone reports were treated as *records of what was
built*, never as design authority.

## Executive summary

**The implementation is broadly faithful to the ratified design — and its
test surface (crash matrices, recovery verifiers, invariant gates) is
unusually strong — but the review found and fixed four real blockers, all at
*enforcement seams* rather than in the machinery itself:**

1. **fs tools escaped the session root** for targets that do not exist yet
   (`root/../x` passed a component-wise prefix check on the lexically-spelled
   path) — `fs.write` wrote outside the root (empirically verified). [B-B]
2. **The approval gate self-approved**: `check_approval` parked the intent,
   returned the digest, and `tool_call` dispatched anyway — every
   approval-gated tool executed with no user in the loop, and the R-16
   re-verification machinery (`Broker::recheck`) was dead code. [D-B-C, B-F4]
3. **The retention gate failed open** (`if let Ok(admission)` swallowed
   policy errors) and discarded admitted bytes (a `Transform` redaction never
   reached storage). [B-F1, B-F9]
4. **GC swept workspace snapshots**: the collector never walked the
   workspace-manifest closure, so the first opted-in GC run quarantined
   every snapshot blob and after grace destroyed them. [F-F1]

Two more empirically reproduced correctness bugs shipped fixed: multi-scope
`reconcile` kept only the last scope's edges (the session reconciles both
scopes before *every* `memory.query`), and lineage dedup was
adjacency-based, so same-content claims with diverging scores survived. The
default gate was also not reproducible from a clean tree
(`crash_matrix_m5` fails on the wasmless stub its siblings skip).

**Biggest craft wins:** the approval flow now carries the user (park →
`resolve_approval` with digest re-derivation and version-snapshot recheck —
the first production caller of `Broker::recheck`), object install actually
fsyncs object *data* before the referencing frame became durable, commit
paths no longer swallow `PolicyError`, and the epoch-digest / store-closure
duplication collects into shared primitives.

**Biggest things left unfixed (all dispositioned, none silent):** R-24's
host-import timeout wrapper and per-generation wall-clock budget are missing
in `kanbei-vm` (the wall-clock leg is misreported by `docs/m2-report.md`);
the module-lifecycle majors (activation-time state-schema validation,
rollback leakage of registry publications, non-transactional service apply,
post-swap `RestartFailed`, disposal facts discarded) are recorded with
candidate fixes but not landed — they need their own milestone-sized waves;
the epoch counter embedded in the composition digest defeats content-address
dedup (R-01); `Session/open `/recovery` decodes bypass the versioned-record
registry (17 sites); the M-14 claim-authority invariant is unenforced — and
built-in `mem.*` fragments render memory claims in the *system* prefix,
which needs a design-ledger clarification before code follows (see §Design
conflicts). The session remains a 12.8k-LOC monolith with a concrete
splitting verdict (not executed here — see C16).

Every finding's disposition, the as-is vs. candidate tradeoffs for each
implemented change, and the amendment proposals follow.

## Commits in this review (all pushed)

| Commit | Findings |
|---|---|
| `ec65917` fs tools lexical containment + tests | B-B |
| `8c27120` retention gate fail-closed, stores admitted bytes | B-F1, B-F9, D-B-A |
| `b2f97d4` reconcile scope rows + key-based dedup + tests | E-F1, E-F2 |
| `c7c2162` GC walks workspace manifest closure + test | F-F1 |
| `b0ed06d` crash_matrix_m5 guard; vm build.rs freeze documented | G-F1 |
| `399ae4a` approval parks for user resolution; run-FSM Blocked; flush-before-effect; Balanced default | D-B-C, B-F4, B-F7, G-F2, D-B-D, D-B-E |
| `441d485` object data fsync, quota, manifest fail-closed decode, shared store_closure | A-F1, A-F3, A-F5, A-F11 |
| `d2cd7b9` quota test fix | — |
| `c16ad63` child-lifetime gate, chronology compaction ranges, GC mtime clock, gauge single pass, registry durability | E-F3, E-F9, F-F4, F-F5, E-F10 |

Gates on the final tree: `cargo nextest run --workspace` **544 passed / 2
skipped** (baseline 535 — the delta is this review's regression tests),
`--all-features` and `--run-ignored all` green, `cargo clean -p` every
touched crate + `cargo clippy --workspace -- -D warnings` **clean**.

---

## Findings ledger

Severity: blocker / major / minor / nit. Axis: fidelity / effectiveness /
craft. Dispositions per the review handoff §4.2: **Fixed** (commit),
**Design conflict** (code vs. constitution — amendment proposed, not silently
reconciled), **Deferred** (real finding, recorded with reason and candidate),
**Wontfix** (documented, deliberate). Verified-clean material is omitted;
each lane's clean list is in the lane records.

### A. Durable kernel (kanbei-core, -log, -objects, -snapshot, -projection)

| ID | Sev | Axis | Finding | Disposition |
|---|---|---|---|---|
| A-F1 | major | fid+eff | Object install synced the directory entry only — no object *data* fsync; power loss could commit an event referencing a renamed-but-unwritten object (`objects/src/lib.rs:54-71` vs arch:373). | **Fixed** (`441d485`): fsync the still-open inode (queued before dirsync on the same FIFO). |
| A-F2 | major | eff | `recover()` truncation has no floor: mid-file header corruption is indistinguishable from a torn tail and destroys every following frame, reporting success (`log/src/lib.rs:270-313,434-438` vs arch:368 "mid-file corruption is an explicit error"). | **Deferred**: the true fix needs a trusted anchor (the head files' last-pinned seq design input) threaded through `recover` — a deliberate protocol change, not a review-sized patch. Recorded as follow-up: distinguish "magic resync finds a later frame" from "EOF tear" as the intermediate step. |
| A-F3 | major | eff | `ExecutionManifest` decoded with no schema check: future-schema manifests deserialize cleanly dropping new digest fields; derived closures claim completeness. | **Fixed** (`441d485`): `from_bytes` fails on `schema > MANIFEST_SCHEMA`; gc decodes through it (unclassifiable ⇒ stays referenced). |
| A-F4 | major | fid | Identity pins write-only: `kernel_schema`/`envelope_schema`/`module_abi`/engine digest never validated on reconstruction; no `unverifiable_pins` reporting (arch:295 "reconstruction validates against the pinned versions"). | **Deferred**: needs a `Report` extension + pin table; the honest partial fix (A-F3) landed first because a silent-lossy decode is worse than an unread pin. |
| A-F5 | major | fid | R-22 hard per-object quota unimplemented. | **Fixed** (`441d485`): `MAX_OBJECT_BYTES` (64 MiB) rejects at install before any write; on-disk overshoot classifies as quota violation. Closes part of ledger:572. |
| A-F6 | minor | craft | `AppendLog::open` discards the `truncated` flag; appending after a torn tail loses the appended frames at the next recovery (API hole, not a live bug — in-repo callers recover first). | **Deferred** (small): open should refuse when truncated. |
| A-F7 | minor | fid/craft | Branding gaps: only `BranchId` is a real wrapper; expected-brand parse is `#[cfg(test)]`; `Envelope.evt` is unbranded (`session/…:1158` fills bare base58); `BRANDS` says `ev_` not `evt_`. | **Deferred**: changing the on-log `evt` format breaks reading existing sessions; needs a bump-and-upcast plan. |
| A-F8 | minor | craft | `check_event_seqs` raw-JSON-peeks `seq` instead of using `Envelope::from_line` (already a dependency); recovery never runs envelope validation. | **Deferred** (small). |
| A-F9 | minor | eff | `recover()` is O(history) hashing; budget says O(tail). | **Deferred**: tied to A-F2's anchor design. |
| A-F10 | minor | eff | Failed rebuild leaves a partially-populated disposable DB (batch commits without watermark). Self-heals on next rebuild. | **Wontfix** with reason: rebuild is destructive-idempotent (drops + recreates); a `.tmp`+rename build is the candidate if observed in practice. |
| A-F11 | minor | craft | Engine/toolchain closure exclusion duplicated at 4 sites. | **Fixed** (`441d485`): `kanbei_snapshot::store_closure`. |
| A-N1..N10 | nit | craft | `now_us` clock panic; `Profile::from` silent default; `hex()/from_hex` asymmetry; raw-JSON peek duplication (N4→A-F8); double-hashing verify; `KindStat.schema` last-write; `"recover:"` copy-paste; rebuild ignores torn flag; zstdcat emits frame metadata lines; `Meta.prev/digest` bare-hex strings. | **Deferred** as a craft sweep; N9 is **Wontfix** (frozen M1 format: `export()` is the JSONL contract). |

### B. Wasm substrate / capabilities / policy (kanbei-vm, -guest, -capabilities, -policy)

| ID | Sev | Axis | Finding | Disposition |
|---|---|---|---|---|
| B-F1 | blocker | eff | Retention admission errors swallowed at the only wired sink (session spine) — gate fails open. | **Fixed** in `8c27120`. |
| B-F2 | major | eff | R-24 leg 2 missing: no host-side timeout wrapper around host imports; a hanging `Host::call` wedges the call past the 5 s post-return-only bound. | **Deferred**: the thread+`recv_timeout` candidate is specified and host-internal; deliberately not rushed in (leaked-thread semantics on genuine hangs deserve their own CR). Highest-priority follow-up. |
| B-F3 | major | eff | R-24 leg 3 missing: no per-generation wall-clock budget; `docs/m2-report.md:49` overstates it. | **Deferred** with candidate (`Instance::accrue(nanos)`); docs/report drift recorded here as the correction. |
| B-F4 | major | eff | `Broker::recheck` dead code; approval version snapshot never captured. | **Fixed** in `399ae4a` (park captures `policy_version`/`grants_version`; resolve calls `recheck`). |
| B-F5 | major | eff | `check()` applies the UNION of all trust-class templates; design prescribes class-keyed intersection with default-deny for agent/workspace origins (arch:191-196,208,209). Latent (one template per broker in tests). | **Deferred**: needs `TrustClass` plumbed onto the invocation input; the union is self-documented in the crate. Design-correct, code-shape change. |
| B-F6 | major | eff | WASI p1 ctx exposes host clocks + seeded RNG to guest Luau (`os.time`, `math.random` seeded `time^clock`) — contradicts the guest-determinism claim and policy purity. Rust guest shim is clean; fs/net/process genuinely unreachable. | **Design conflict (minor)**: WASI clock/random override (constant clocks + fixed RNG) + nil-ing the leaky Lua globals is *internal* guest behavior, but the handoff requires ABI-adjacent changes to be signed off. Proposed: do both in one change with determinism tests pinned on observable values. Not landed here. |
| B-F7 | major | eff | Approval intent expiry never enforced; standing-scope constructs bypassable via pub fields. | **Fixed** in `399ae4a` (recheck enforces intent expiry; fork-floor grants remain session-scoped with purpose by construction). Constructor hardening of pub-field bypasses **deferred** (craft). |
| B-F8 | major | fid | Grant/approval digests do not bind package digest, ProjectId, trust class, (intent) ModuleId — "digest changes re-prompt" (arch:208) unimplementable. | **Deferred**: additive canonical-JSON fields; changes every recorded grant digest (fork facts, tests) — a migration-shaped change needing its own wave. Proposed amendment text drafted in §Amendments. |
| B-F9 | major | eff | `Admission::Stored{bytes}` discarded — redaction never reached storage. | **Fixed** in `8c27120`. |
| B-F10 | — | fid | `op_check` response hides `requires_approval`/budget — the module cannot drive the R-16 loop programmatically; any fix changes the guest-visible op-4 response shape. | **Design conflict**: proposal to add the fields (breaking, version-bumped `module_abi`) rather than a new op; do not change without ABI ratification. |
| B-F11 | mod | craft | `Host` stringly; `STALE_GENERATION` magic string duplicated across vm/modules. | **Deferred** (craft, small). |
| B-F12 | mod | craft | ABI consts scattered; `SCRATCH_SIZE` unshared and the vm-initiated scratch write unchecked (oversized args scribble guest statics instead of a typed error). | **Deferred**: shared `kanbei-abi` consts crate sketched; bounds-check on the vm write path is the safety-relevant slice. |
| B-F13 | mod | craft | POLICY_VM_CONFIG epoch comments factually wrong (absolute-vs-relative; relies on the invisible `MAX/2` clamp). | **Deferred** (small): `VmConfig::without_epoch_deadline()` + comment fixes. |
| B-F14 | mod | eff | Custom `Limiter` drops the per-table element bound. | **Deferred** (3 lines): `max_table_elements`. |
| B-F15 | minor | fid | Caller/tool-provider legs collapsed; `principal.run` always None (documented M2 scope). | **Wontfix** (documented deferral), recorded for the run-lane. |
| B-F16 | mod | craft | `admit` bypasses the kernel replay-default resolution (candidate bit `Some` always); `with_replay_default(false)` can open the default wide. | **Deferred** (small): `admit(declared: Option<bool>)`, privatize the default setter. |
| B-F17 | minor | craft | Memory fault points never exercised by an *aborting* crash child. | **Wontfix/moot**: lane B's premise is stale — `crash_child.rs` m4 mode drives `MemoryAbortInjector` under gate_m4 (verified by lane G's reachability table). |
| B-F18 | minor | craft | `json::serialize`'s `lua` param + `#[allow(only_used_in_recursion)]` genuinely dead. | **Deferred** (guest-internal change forces wasm rebuild at wave end). |

### C. Module lifecycle (kanbei-modules, -scopes, -services)

| ID | Sev | Axis | Finding | Disposition |
|---|---|---|---|---|
| C-F1 | major | fid | No activation-time state-schema validation (R-07/C-07); `PackageManifest.state_schema` is a dead field; `module reset-state` absent entirely (arch:236). | **Deferred**: the per-write CAS check (typed `SchemaMismatch`, old head untouched) approximates it; wiring activation-time checks requires module↔key binding the manifest model doesn't carry. `reset-state` + canonical reinit fact are the concrete missing surface. |
| C-F2 | major | eff | Failed-activation rollback leaks host-op-6 registry publications and per-generation contributions (only `drop_generation` cleans them; a rejected transaction leaves composition state). | **Deferred**: candidate is in the finding (error branch → `drop_generation_contributions` + reverse shared-registry keys); needs its own gate_m2 coverage extension. |
| C-F3 | major | eff | (a) `ContributionRegistry::apply` shares the live `ServiceRegistry` Arc — services escape the transaction; (b) `deactivate` counts intra-module dependents → a module with internal deps can never deactivate, and the session's `let _ = deactivate` rollback silently no-ops. | **Deferred**: the scopes crate already models the fix (cross-scope dependent filter). Two small, test-shaped fixes; not landed for lack of wave budget. |
| C-F4 | major | fid | Generation replacement not transactional: `RestartFailed` surfaces after the swap (R-25 arch:234). | **Deferred**: pre-flight `plan_replacement` (pure) against the projected entries. |
| C-F5 | major | fid | R-01: the epoch counter is mixed into the composition digest — identical compositions digest differently; no-op publishes change EpochId; manifest dedup defeated; the crate's own doc claim is false. | **Deferred**: the two-line fix (drop `"epoch"` from canonical bytes) changes every composition digest manifest → pins; needs a gate pass across m2–m6 to land. Recorded as the pick. |
| C-F6 | major | fid | Disposal has no deadline, `forced` is hardcoded false, and every session call site discards `DisposalRecord` — rejected stale effects are a counter, not canonical facts (R-24/C-04, arch:233,290). | **Deferred**: wire the record through the session commit (one event kind + the 10 `let _` sites); the drain-stub itself is a documented M2 acceptance. |
| C-F7 | minor | fid | Service replace-intent not validated against capability/precedence; no precedence model exists. | **Deferred** (needs the F8 metadata first). |
| C-F8 | minor | fid | Lifecycle metadata 5 of 8 (missing precedence, persistence, disable-allowed). | **Design conflict**: three manifest fields + schema bump, or an explicit deferral note in the ledger (proposed below). |
| C-F9 | minor | eff | Owner leases recorded but unenforced; `/ghost` scopes publishable. | **Wontfix** (session never wires child scopes today — mechanism is dormant by ratified design); note for the dynamic-scope milestone. |
| C-F10 | minor | craft | Disposal logic triplicated; public `Generation::dispose` is dead + weaker (leaks contributions). | **Deferred** (craft refactor). |
| C-F11 | minor | craft | Atomic head/replace mechanics implemented 3×. | **Deferred**: hoist `atomic_replace` into `kanbei-core::queue`; keep the two head *contracts* distinct. |

### D. Session spine (kanbei-session, -scheduler, -provider, -tools)

| ID | Sev | Axis | Finding | Disposition |
|---|---|---|---|---|
| D-B-B | blocker | fid | fs escape on nonexistent targets (see above). | **Fixed** (`ec65917`). |
| D-B-C | blocker | fid | Approval self-approval; no user resolution path; `approval_for` (the correct D-12 binder) dead code. | **Fixed** (`399ae4a`) — park, `resolve_approval`, digest re-derivation, version recheck, driver-seam resolver for unattended batteries; queue-eviction tests added (G-F2). |
| D-B-G | — | fid | fork/adopt/import/workspace/OTel/auto-GC shipped while `architecture.md:701-704` lists them as MVP non-goals. | **Design conflict** — see §Amendments. Code stays (M9's gates are its acceptance evidence). |
| D-B-F | major | fid | B-05 classification covers `tool_intent` only; `model_call`/`memory_proposal` intents without outcomes never classify. | **Deferred**: extend `scan_classified_intents` + quiesce listing; shape is present, kinds filter drops them. |
| D-F-H | major | fid | `effect_dispatch` and `restore_workspace` bypass intent-before-dispatch (restore writes the tree then commits the event). | **Deferred**: commit `workspace_restore_intent` before executing; `effect_dispatch` needs an event vocabulary decision (hosted effects are the R-19 tier-2 seam). |
| D-F-I | major | fid | R-08/E-13 rendered-hash equality "enforced by construction" — no typed check, no outcome→intent ref, `commit_tool_outcome` accepts any outcome. | **Deferred**: re-read the committed intent at outcome commit; validate intent-pairing for committed outcomes (the pending-intent scan already provides the data at open). |
| D-F-J | major | fid | No responder preemption: any wake is denied `ConcurrencyLimit` while a run is active (doc comment claims auto-cancel — false); single-threaded driver cannot process input during a run. | **Deferred**: responder-accept → cancel active cognition run; needs a concurrency-model note (the single-actor driver is by ratified design). |
| D-F-K | major | fid | Breaker floors unclamped (doc claims clamping); paused state not restored at open (re-open un-pauses cognition without user resume — arch:120). | **Deferred**: clamp at `Scheduler::new`; restore `paused` from the last unresumed `breaker_tripped`. Both small, gate-shaped. |
| D-F-L | major | fid | No-progress counter never resets on causal events ("ever-progressed" semantics vs arch:120). | **Deferred** (small, gate-shaped). |
| D-F-M | minor | fid | Egress sensitivity classes hardcoded (`vec!["call"]`). | **Deferred** (fold real fragment classes). |
| D-F-N | minor | fid | Compaction selection has enforcement, no kernel API; "causal-closed" is fragment-id containment, not a causal-parent check. | **Wontfix** (deferred milestone-shaped; enforcement half exists and tests drive the kind manually). |
| D-F-O | major | fid | SessionId never persisted; default lifetime memory is per-session (`<dir>/memory` vs the XDG shared `memory/lifetime`); no `sessions/<SessionId>/` layout; import reverse-engineers the id from markers. | **Deferred (layout)** / **partial**: identity persistence (session.json at open, import reads it) is the bounded slice that kills the T3 fragility — recorded as the pick; full XDG layout conformance is a storage-model wave. |
| D-F-P | minor | fid | `after_secs` discarded; `pending` unbounded without a timer wheel. | **Deferred** (small). |
| D-F-Q | major | craft | The versioned-record registry/upcasters bypassed on all reconstruction paths (17 raw decodes; `payload_schema: 1` hardcoded; projection *counts* upcasts but does not consume them; `follow` silently falls back to `FollowHead` on schema drift). | **Deferred**: route the load-bearing group (follow/quiesce/config_choice) through `Registry::upcast`; fail loud on drift. Milestone-shaped but high-value. |
| D-F-R | major | craft | Silent `Envelope::from_line` skips (21+ sites); `resolved_payload` returns the raw `$object` marker on store miss (a swept GC object drops a promoted intent from classification); GC skips unreadable manifests. | **Deferred**: `Result<Value, MissingObject>` + classify-as-interrupted; GC records sweep exceptions. |
| D-F-S/T7 | major | craft | `Session::open` is 460 lines with five full log scans; session crate needs splitting. | **Deferred**: concrete seams recorded (recovery.rs single-scan → branch.rs → switch.rs → elements.rs → commit.rs). Deliberately not executed in a review diff of this size. |
| D-F-T | minor | — | run_cmd unbounded RAM during streaming; timeout path skips output limit; `Principal{generation:0, run:Some(0)}` placeholders ×4; stale M3 doc; discarded schema lookup; `unreachable!` at spine.rs:1394; projects.jsonl first-parseable wins. | **Deferred** craft sweep. |
| T1 WireProtocol | minor | craft | `WireProtocol` on `SessionConfig` (wrong home; cost = 16 ProviderConfig literals). | **Deferred**, pick recorded: `protocol: Option<WireProtocol>` on `ProviderConfig` with `serde(default)`, one-line literal fixes. |
| T2 fork surface | minor | — | `ForkOptions` carries a whole `SessionConfig` (~80% silently overridden); grant set correct per arch:212 but the approval path made it toothless (now fixed); `truncate_log_at` splice verified correct. | **Partial**: the toothless-approval half is fixed by D-B-C; the facade-streamlining deferred with the candidate (record cut seq+offset in the `forked` fact). |
| T4 quiesce duplication | minor | craft | adopt/continue_from share ~50 copied lines. | **Deferred** (factor `quiesce(tail_cutoff)`). |
| T8 memory_fault | minor | craft | `#[allow(dead_code)]` field genuinely dead. | **Deferred** — now that lanes verified it, removal is 3 lines; lost to wave budget (recorded). |
| T9 gauges | major | eff | O(objects) scan per run outcome under otel. | **Fixed** in `c16ad63` (single pass). |

### E. Memory / context / retrieval (kanbei-memory, -context, -retrieval)

| ID | Sev | Axis | Finding | Disposition |
|---|---|---|---|---|
| E-F1 | major | eff | reconcile wiped edges/roots per scope (empirically reproduced). | **Fixed** (`b2f97d4`). |
| E-F2 | major | eff | Dedup adjacency-based (empirically reproduced). | **Fixed** (`b2f97d4`). |
| E-F3 | major | fid | Children projected the lifetime memory fragment (m4-report claimed otherwise). | **Fixed** (`c16ad63`). |
| E-F4 | major | fid | Promotion is vocabulary-only: no `Promotion` transition writer, no `PromotedFrom` edges, lifetime claims have no write path (`dispatch_memory_propose` hardcodes Project). | **Deferred**: the largest single memory-layer gap vs arch:456-468; promotion needs its milestone (approve/reject/request-evidence UX is unratified). Recorded with the reuse candidate (`approve_transition` w/ `TransitionKind::Promotion`). |
| E-F5 | major | fid | M-14/R-13 unenforced — and built-in `mem.*` fragments (`MemoryClaim` sources, `ScopeStable`, cache-eligible) render in the SYSTEM prefix. | **Design conflict** — untrusted claim content occupies system-authority position in every model call; but arch's conceptual ordering (145-146: stable project/lifetime memory near the front) and M-14 pull opposite ways. Proposed amendment in §Amendments; code follows the amendment, not the other way. |
| E-F6 | minor | fid | `applies_to` typed entity keys are dead vocabulary (content-derived entity projection half is implemented). | **Deferred** (wire an extractor or cut the field from MVP — decision recorded with the design). |
| E-F7 | minor | fid | Structural event/tool extractors (modified-file, failed-with edges) absent. | **Deferred** (explicitly listed as a design gap, not silently). |
| E-F8 | minor | fid | pins/open-loops never populated — the canonical layer-2 record is absent; salience weights for goals/pins permanently 0. | **Deferred**: either wire the `model_call` pin facts or zero+document the inert weights; needs an instrument decision. |
| E-F9 | minor | eff | Chronology check ignored `CompactionRange` refs. | **Fixed** (`c16ad63`). |
| E-F10 | minor | craft | ProjectRegistry: no fsync, torn line bricks the registry, read-then-append race. | **Fixed** (`c16ad63`): `sync_data` before ack + torn-tail tolerance; the duplicate-suffix race remains accepted (documented; last-wins lookup). |
| E-F11 | minor | fid | `PinnedAt` carries root digests, not a `TransitionId` (M-17's ratified shape). | **Deferred** (equivalent identity via one-transition-one-root; rename-vs-migrate is the decision). |
| E-F12 | minor | fid | Model-invocation record omits module hashes; projection evidence rides salience top-32, not the retrieval pipeline. | **Deferred** (populate from the pinned registry digest; record the evidence-source decision). |
| E-F13 | nit | — | Acyclicity is refs-to-committed only; 2-cycles between already-committed claims constructible across two transitions. | **Wontfix** with reason: bounding `from` would break the ratified two-transition supersede; the design's DAG claim should be annotated (commit-time-wise). |
| E-F14 | nit | — | Probe thresholds partially code-only vs `docs/memory-probes.md`. | **Deferred** (backfill the doc). |

### F. UI / telemetry / GC / workspace

| ID | Sev | Axis | Finding | Disposition |
|---|---|---|---|---|
| F-F1 | blocker | eff | GC root capture omitted workspace blobs. | **Fixed** (`c7c2162`). |
| F-F2 | major | eff | InputDecoder pending unbounded + O(n) rescan per feed on unterminated CSI-numeric. | **Deferred** (cap + reset — 5 lines + tests). |
| F-F3 | major | craft | Hot path: `Cell.style: String` (1920 allocs/frame), one `write()` per cell edit in `apply`/`paint_full`. Measured input-ACK p99 (1.38 ms) passes; paint cost is unbudgeted. | **Deferred** with reasoning: no violated budget today; style interning + run coalescing is the recorded pick when S16 becomes load-bearing. |
| F-F4 | major | fid | Grace clock never refreshed on re-quarantine. | **Fixed** (`c16ad63`). |
| F-F5 | major | eff | Gauges double-scan + per-file stat per run outcome; spine `?` fails `run_outcome` after commit on io error. | **Fixed** (single pass). The `?` demotion is **deferred** (one line). |
| F-F6 | minor | fid | Theme overlay bind swallows the apply error (partial application, silent). | **Deferred** (small). |
| F-F7 | minor | eff | Workbench exits on raw `0x03` inside pasted content. | **Deferred** (small). |
| F-F9 | minor | craft | `viewport_top` dead in three places (fields made `pub` to silence, not to use). | **Deferred** (consume or delete). |
| F-F10 | minor | eff | Decoded `PageUp/PageDown/Home/End/Delete` go nowhere. | **Deferred** (wire to focus/scroll or drop from the decoder). |
| F-F11 | minor | fid | a11y misses unfocusable interactive nodes; a11y-fail → class-2 (not class-1) mapping is an interpretation worth a ledger note. | **Deferred** + note recorded. |
| F-F12 | minor | — | No alt-screen / cursor-hide; main screen wiped. | **Deferred** (UX-visible; deliberate escape-sequence change). |
| F-N* | nit | — | caret invisible on empty input; dead `unreachable!` in focus.rs; `UiProvenance::Module` never constructed; workspace temp-file churn on crash; chmod-after-rename window; `now_nanos` duplication/pre-epoch panic; telemetry flush aborts on first sink error. | **Deferred** craft sweep. |

### G. Test surface / effectiveness

| ID | Sev | Finding | Disposition |
|---|---|---|---|
| G-F1 | high | Default gate not green on a clean tree: `crash_matrix_m5` fails without the guest wasm; kanbei-vm's stub freezes until `cargo clean -p kanbei-vm`. | **Fixed** (`b0ed06d`): skip guard mirroring siblings + freeze documented in the build warning. |
| G-F2 | high | Approval-queue bound/overflow/eviction had ZERO tests (a differentiator gate, arch:669). | **Fixed** in `399ae4a` (approval.rs eviction/deny/approve/park tests). |
| G-F3 | med-high | 8 of 9 numeric budgets unmeasured (event-commit ACK p99, rebuild rate, callback latency, closure dedup %, 5M rebuild time/RSS, export ≤2×, breaker ≤1 s as timing). | **Deferred**: instrument each as its budget becomes load-bearing; the two that gate (input-ACK, write amplification) are measured. |
| G-F4 | med | M8/M9 feature surface has no crash-injection mode (workspace/fork/adopt/GC/telemetry fail-soft only). | **Deferred**: real gap; needs fault-point vocabulary + crash-child modes for the M9 seams. |
| G-F5 | med | Consistency-12 (Projection) has no M1 gate test — the test named `acceptance_consistency_12` is the SQLite-deletion bullet; numbering silently drifted. | **Deferred**: rename/re-file in testkit. |
| G-F6 | med | Consistency-15 (Scope) missing at M1/M5/M6/M7 gates (present M2-M4 only). | **Deferred** (add per-milestone scope assertions). |
| G-F7 | med | `AfterEffectDispatch` is the one fault point never forced. | **Deferred** (needs the separate-caller-module dispatch shape). |
| G-F8/G-F10 | low | Instrument T2.2 weaker than registered (vacuous per-run); battery SIGKILL windows fixed not random (unregistered); scaled unattended hour; no skip guard for missing python3/git. | **Deferred**: backfill the instrument doc with deviations. |
| G-F9 | low | Two drifting skip idioms (`require_guest` vs `modules().is_none()`). | **Deferred** (craft; the m5 matrix now guards). |
| G-CLIPPY | — | `--all-targets` pre-existing findings (13 exact): scopes registry.rs cloned_ref; workspace tests dead_code; session tests/common (5 dead/BORROW); ui_multi needless_borrow ×3; gc.rs unused_vars/let ×3. | **Deferred**: enumerated precisely (below in lane G records) for a sweep commit; standing gate remains `--workspace`. |

---

## Compare-and-contrast (every implemented change)

| Finding | As-is | Candidates | Pick + why |
|---|---|---|---|
| B-B fs escape | `canonicalize().unwrap_or(joined)` + component-wise `starts_with` — nonexistent `..` targets escape. | (a) `openat2`-style sandbox; (b) lexical normalization before canonicalize, containment on both the spelled and resolved paths; (c) reject `..` outright. | **(b)**: no new libc surface, keeps absolute-in-root inputs working, and the existent-symlink case still gets the canonicalize pass. (c) regresses legitimately-pending writes inside the root (`sub/../inside.txt`). Test proves both directions. |
| B-F1/B-F9 retention | `if let Ok(admission)` — errors swallowed, `Stored` bytes dropped. | (a) propagate `?` and fail the commit; (b) match: Interrupted classification + store the admitted bytes (`Stored`→result, `Dropped`→null+`retained:false`). | **(b)**: R-04/D-07 prescribes fail-closed *and* "explicit parked/interrupted state" — failing the whole session commit on a plugin trap is harsher than the design's classification language; persisting the admitted bytes is the literal "gate stores exactly these bytes" contract. `retained` records the admission either way. |
| D-B-C approval | Park + self-approve and dispatch; `approval_for` dead. | (a) deny-by-default with no resolution API (safest, unusable); (b) park + `resolve_approval(digest, approve)` with digest re-derivation + version recheck + run-liveness handling; (c) auto-resolve inside `tool_call` via config flag. | **(b)** with a driver seam for (c)'s only legitimate use: unattended scripted batteries need decisions *in-loop*, so `SessionConfig::approval_resolver` (explicit, named, default-none) plays the user there instead of regressing to self-approval. NonActiveRun-tolerant dispatch accounting because parked approvals legitimately outlive their initiating run. |
| D-B-D Blocked FSM | Direct event commit; scheduler `active` stays occupied. | (a) call `record_outcome` directly and commit; (b) reuse `run_outcome` with a reason-carrying variant. | **(b)**: one commit path for terminal outcomes — the run FSM owns pairing; `record_outcome_reason` threads the responsible constraint (R-17) instead of losing it. |
| D-B-E profile/flush | `Profile::Fast` default; no barrier before effects. | (a) flush on every tool dispatch; (b) flush only before consequential tools via `consequential_tool`; (c) config-only. | **(b)** + default `Balanced` (arch:406 verbatim): effect-adjacent fsync is unconditional by design, but read-only tools paying an fsync per call buys nothing. |
| A-F1 data fsync | Rename + queued dirsync only (M1's documented relaxation). | (a) synchronous fsync before rename (slow path, per-object fsync in the caller); (b) keep the fd open across the rename and queue `SyncOp::Fsync(fd)` ahead of the dirsync on the same FIFO. | **(b)**: preserves the one-queue ordering invariant the whole crash argument leans on, adds one fsync per non-dedup install, and makes "a referencing event may commit only after the object's rename is dirsync-durable" actually mean the *contents* too. |
| A-F3 manifest | Raw `serde_json::from_slice` at every consumer. | (a) `#[serde(deny_unknown_fields)]`; (b) `from_bytes` rejecting `schema > current`. | **(b)**: deny_unknown_fields would break legitimate forward-compat within the current schema; a version gate is the semantic the field exists for. GC consumers stay conservative (unclassifiable ⇒ referenced). |
| A-F5 quota | No bound. | (a) byte check only at install; (b) install + read-side classification both. | **(b)** with `Quota` as its own error variant so workspace maps it into its corruption taxonomy rather than conflating it with hash damage. |
| E-F1 reconcile | Per-scope `DELETE FROM edges/roots`. | (a) per-scope deletes with prior-scope re-write; (b) hoist the deletes above the loop. | **(b)**: the tables are full-refresh by design; the loop placement was simply wrong. |
| E-F2 dedup | `sort_by` + consecutive `dedup_by`. | (a) sort by `(dedup_key, rank…)` then dedup; (b) `retain` with a seen-HashSet after the rank sort. | **(b)**: keeps the documented ordering semantics (authority rank first) and drops duplicates wherever they land; unit test pins the interleaved case directly. |
| F-F1 GC roots | Manifest referenced; blobs pinned only inside the manifest object. | (a) put blob digests in the event payload (`entries` today carries a count); (b) walk the manifest closure in the collector for `workspace_snapshot`/`workspace_restore`; (c) exempt workspace manifests from GC. | **(b)**: a–and-c both weaken the reference-set story (payload bloat / a root class GC ignores). The fork path already proved the walk exists (lib.rs:1695-1740) — the collector just never had it. Regression test sweeps with grace=0. |
| E-F3 child fragments | Lifetime fragment always included. | (a) gate on `is_child` (mirroring salience); (b) filter in `AuthorityFilter`'s read closure. | **(a)**: the gate lives where the source is constructed — one truth, and the read-closure approach would still build+serialize the fragment before rejecting it. |
| F-F4 grace clock | Quarantine rename preserves mtime. | (a) utimensat/libc; (b) std `File::set_modified`. | **(b)**: no new dependency (constitution stance); failure to stamp degrades to the old behavior (grace-from-first-quarantine), not corruption. |
| G-F1 gate guard | Crash matrix fails wasm-less. | (a) hard-fail with message; (b) skip guard; (c) build the guest in CI before the suite. | **(b) + documented (c)**: the suite must stay either-way green per its own contract; the build.rs warning now names the `cargo clean -p kanbei-vm` freeze, and (c) is the CI note for the owner. |

---

## Design conflicts and proposed amendments

None of these were resolved in code here — each needs the owner's sign-off per
the fidelity rule (cite both passages; amend; never silently reconcile).

1. **Post-MVP scope drift (D-B-G).** `architecture.md:701-704` lists
   fork/adopt/import, workspace snapshots, auto-GC, and OTel correlation as
   MVP non-goals; M9 shipped all of them (`m9-report.md` §4-5). Proposed
   amendment: move the four lines from "Explicit MVP non-goals" to a new
   "Shipped post-MVP increments (ratified 2026-08-30)" section with its
   acceptance evidence (gate_m6/m9 suites, m9-report §5) — plus the two
   honest restrictions: auto-GC is opt-in-only (`gc: None` default), and
   canonical-object GC being "disabled in MVP" (R-29's tradeoff register)
   should be rewritten as "off by default; the coordinated quadratic-GC design
   exists" since `run_gc` ships.
2. **M-14 vs. conceptual ordering (E-F5).** M-14 bans claim-sourced
   system/developer authority; arch:143-146 places stable project/lifetime
   memory in the stable front. The strict reading (evidence-role only, tail)
   makes cache stability for memory weaker; the loose reading (system-role
   framing of a claims block marked as evidence) preserves cache wins.
   Decision needed; the ledger's R-13/M-14 wording suggests the strict
   reading is the ratified one, in which case today's built-ins are
   non-compliant and `lower()` should move `mem.*` fragments out of the
   system prefix with a validator rule to pin it.
3. **op_check response shape (B-F10).** Extending the op-4 response with
   `requires_approval`/budget fields breaks the guest-visible host ABI;
   either ratify a `module_abi` bump carrying it, or accept that modules
   cannot programmatically drive exact approvals (the UI is the only
   approver). The second is coherent with this review's approval rework.
4. **Lifecycle metadata (C-F8).** arch:220 lists precedence/persistence/
   disable-allowed among the required module metadata; the manifest carries 5
   of 8. Amend the ledger to defer the three (with the schema-bump shape) or
   confirm them as required —silent absence is the third bad option.
5. **Grant/approval digest binding (B-F8).** arch:208/211's
   "digest-changes-re-prompt" cannot be enforced while digests bind no
   package digest/ProjectId/trust-class/ModuleId. Either ratify the
   additive fields (and a grant re-prompt flow on package change) or amend
   the passage to scope the binding to principal+generation+action+args
   (what ships today).
6. **WASI exposure in the guest (B-F6).** The guest-determinism claim
   ("no time, no randomness") is true of the Rust shim and false of the Lua
   environment (`os.time` via WASI host clocks, `math.random` seeded from
   real time). Amend the claim or harden the ctx (constant clocks, fixed
   RNG, nil the leaky globals) — the latter is the recommended direction.

## Deferred-work register (ranked)

1. R-24 host-import timeout wrappers + per-generation wall-clock budget
   (B-F2/F3) — vm hardening, highest risk-adjusted value.
2. State-schema wire-up + `module reset-state` + rollback leak (C-F1/C-F2)
   and the epoch-digest de-mixing (C-F5).
3. Registry-bypass decode boundary (D-F-Q) + `resolved_payload` Result
   (D-F-R).
4. Session recovery single-scan + module split (D-F-S/T7).
5. Breaker floors/paused restore + no-progress reset + responder preemption
   (D-F-K/L/J).
6. B-05 classification coverage + intent-pairing validation (D-B-F/D-F-I).
7. M8/M9 crash modes + budget instrumentation (G-F4/G-F3).
8. Trust-class intersection (B-F5), digest binding (B-F8), promotion (E-F4).

## Gates

- `cargo nextest run --workspace`: **544 passed, 2 skipped** (battery is
  `#[ignore]` by convention).
- `cargo nextest run --workspace --all-features`: green.
- `cargo nextest run --workspace --run-ignored all`: green (both battery
  suites).
- `cargo clean -p` on every touched crate then
  `cargo clippy --workspace -- -D warnings`: **clean** (fresh-linted).

Baseline comparison: `main@d84908c` = 535 default tests; final tree 544
(+9: approval ×4, retrieval ×2, gc ×1, objects quota ×1, plus guard changes).
