# M6 Report — Historical Correction

Gate: **green** — 413 tests / 62 suites (~32 s warm), clippy `-D warnings` clean on all 20 crates including the wasm guest.
Commits: `68ee8b8` (wave 1 branch substrate), `4cafeb9` (wave 2 memory follow + config resume), `fe97e77` (wave 3 discontinuity + opaque artifacts), `df74c8b` (wave 4 export/closure + crash + gate_m6).
Report date: 2026-08-30.

## Scope delivered (high-level-architecture.md milestone 6)

- **Checkpoints and `continue_from`** — new execution from a historical causal frontier, never rewriting history.
- **Current-config staged resume** — the branch records the current-vs-historical config choice; the live config stays in effect.
- **Explicit provider-reasoning discontinuity** — the model can flag that its reasoning does not follow from the projection.
- **Export/closure verification prepared** for the dogfooding evaluation; crash injection at the session-head update (the M6 acceptance-matrix item).

## Wave 1 — checkpoint + branch substrate (kanbei-session, kanbei-core, kanbei-scheduler)

- `BranchId` brand (`branch_`) in kanbei-core; root branch is session-lifetime state (no genesis event exists — a fresh session commits nothing; resumed logs without transitions get a fresh root id; branch records carry `from`).
- `Session::create_checkpoint(label)` — one canonical `checkpoint_created` event whose own seq **is** the frontier; payload: label, frontier_seq, post-event manifest digest (pre-computed via the extracted `build_manifest`, byte-identical to commit step 5 — `debug_assert_eq!` guards parity), memory root pins, composition, branch. Label capped at 200 chars. Commits with a post-snapshot pin.
- `Session::continue_from(&CheckpointRef)` — validates (session match, committed seq, event kind + frontier, **snapshot closure** via `kanbei_snapshot::manifest_closure`), quiesces (active run → `run_outcome Failed(Quiesced)` — new `FailureKind::Quiesced`; pending intents → `quiesce.cancelled`; interrupted/ambiguous tail intents → `quiesce.ambiguous`; the listing is a field of the transition record, R-11/E-04/B-10/M-17), then commits one `branch_transition` event {branch, from_branch, frontier_seq, checkpoint_event, checkpoint_snapshot, follow, config_choice, quiesce, memory roots} and switches the session.
- **Branching is derived, not stored in envelopes**: the envelope schema is unchanged; the path filter (`on_path`, `path_ranges`) excludes exactly `(frontier, transition]` per record. The checkpoint event stays on-path; the transition event is the old root's final fact. Trajectory rendering filters the ring by path and the conv.prefix fragment covers only on-path ranges (R-05 chronology — the prefix never claims the abandoned tail).
- Branch state rebuilds from the log on reopen (branch_transition scan, like compaction_selected).
- Fault points `Before/AfterCheckpointCommit`, `Before/AfterBranchTransition`, `Before/AfterSessionHeadAdvance` (the last fires in `commit()` around the `next_seq` advance — the M6 "head update" crash point).

## Wave 2 — memory follow policy + current-config staged resume

- `MemoryFollowPolicy { FollowHead, PinnedAt { lifetime_root, project_root } }` in kanbei-memory; **default for `continue_from` is PinnedAt(checkpoint's pinned root)** (architecture.md:478). Pinned roots validated against actor history (`MemoryRootActor::contains_root` — every committed root is an ancestor of the head fold) before the transition.
- Pinned roots substitute the live actor heads at every consumption point: projection folds (folding the pinned root yields the historical claim set), `ProjectionState.memory_roots`, model-call root pins, cache invalidation, and memory-query index reconciliation. `Session::memory_follow(policy)` records a canonical `memory_follow_changed` event (re-following the head is an explicit recorded transition).
- `ExecutionManifest` config fields now populated at pin time (`build_manifest`): `tool_registry` + `provider_config` (content digests over canonical bytes, objects installed so the closure is verifiable), `scheduler_policy` ("builtin-default"). `kanbei_snapshot::manifest_closure` walks every digest field; `continue_from` and export use it.
- `config_choice` = `{ mode: "Current", current: <live config digest>, historical: <checkpoint manifest's provider_config>, composition }` — the record is the deliverable; module-state/config restoration is explicitly out of scope (architecture.md:396).

## Wave 3 — provider-reasoning discontinuity + opaque artifacts (S9)

- `CompletionResponse` gains `discontinuity: Option<String>` (the model flags its reasoning does not follow from the projection) and `opaque_artifacts: Option<String>` (opaque reasoning bytes, base64, stored verbatim).
- The model's flag takes precedence over the provider-change heuristic: `ReasoningContinuity::Broken { from_provider, at_event, reason: Some(flag) }` (new `reason` field, `serde(default)` for old records).
- **Artifact replay is same-provider only** (E-07 transferability default none): `last_opaque` keeps the artifacts paired with their provider; a later same-provider call replays them, a different provider never sees them. The outcome records the artifacts verbatim (byte-exact round-trip) and the raw flag for audit; artifacts never enter the projection.

## Wave 4 — export/closure + crash m6 + gate_m6

- `Session::export_bundle(dir)` — plain JSONL log + raw frame copy, every distinct pinned manifest (`manifests/<digest>.json`), the full object closure (`objects/<digest>.bin`), and a `closure.json` report {frames, envelopes, manifests, objects, missing, identity_pins, verified}. Missing/unreadable objects are **reported, never fatal** (honest partial availability, R-06). Engine/toolchain digests are kernel-embedded binary identity pins, not store objects — excluded from the closure, recorded as identity_pins (mirror of continue_from).
- `crash_child` m6 mode + `verify_m6_recovery`: 6 points × Before/After × 2 ack offsets (24 crash children); recovery asserts M1 invariants, ≤1 branch_transition with parseable fields, the referenced checkpoint at frontier_seq, branch-state rebuild on reopen (branch, records, on_path, path_ranges), idempotent double reopen.
- `gate_m6` (10 tests): checkpoint canonicality, history preservation (envelope set equality — **no rewrite**), path derivation + trajectory exclusion, quiesce of an active run with intent listing, invalid-checkpoint rejection (future seq / wrong session / non-checkpoint / damaged closure with no transition appended), config choice record, export closure verify + honest missing report, crash matrix, clean completion, reopen-commits-on-path.

## Acceptance matrix

- `continue_from` creates a valid new branch without rewriting history — envelope-set equality test (`continue_from_preserves_history_and_derives_path`).
- Crash injection at head update — `Before/AfterSessionHeadAdvance` in the crash matrix; also checkpoint/branch-transition points (M6 owns the branch-transition crash surface).
- Export at M6 — `export_bundle` + closure verify + honest partial report.
- Consistency 15 (Scope): no `fork`/`adopt` (post-MVP per architecture.md:608); no deterministic replay (new execution only, :400).

## Key decisions / gotchas

- **The envelope stays linear**; the causal fork is derived from branch-transition records at projection time. This keeps the envelope schema and the append-log format frozen.
- **PinnedAt pins root digests**, not TransitionIds (the spec's "checkpoint's pinned root"); the digest is what the checkpoint captures, and the TransitionId is recoverable from the memory transitions log.
- **Config choice is always Current** — "current-config staged resume" is the named feature; historical config restoration is module-state restoration territory (separate typed operation, out of scope).
- **Clippy cache-masking discovered**: kanbei-ui (7 lints) and kanbei-session/src/ui.rs (7 lints) carried pre-existing rustc-1.98 lints that the M5 "clippy clean" gate never actually linted (warm per-crate caches). Fixed minimally with clippy's own suggestions (collapsible_if/while_let/question_mark/io::Error::other/needless_borrow/derivable Default); semantic-only, no fmt churn. Gate now run with `cargo clean -p` on freshly-linted crates.
- `verify_m6_recovery` signature follows `verify_recovery_tolerant`'s `(dir, acked, reopen_extra) -> Result<usize, String>` convention.
- gate_m1 object-count assertions updated (32→33) for the new tool-registry object installed at pin time.

## Next

M7 dogfooding gate: threshold tuning of the pre-registered instrument (docs/dogfooding-instrument.md) + memory probes (docs/memory-probes.md); broaden custom-schema/upcaster coverage; NVMe re-bench before evaluation.
