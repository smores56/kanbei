# M8 Report — Post-MVP Deferred Items (R-20): Telemetry, GC, Dense-Retrieval Entry Criteria, Multi-Module UI

Gate: **green** — 457 tests / 73 suites default features, 458 with `--all-features` (full suite including the M7 battery instrument via `--run-ignored all`); clippy `-D warnings` clean on all 20 crates including the wasm guest (fresh-linted, both feature modes).
Commits: `22c721c` (wave 1 telemetry), `76ac00a` (wave 2 GC), `58e0d3b` (wave 3 UI composition), `6bbf38e` (wave 4a dense-retrieval ablation), `dc144fa` (battery behind `#[ignore]`).
Report date: 2026-08-30.

Testing note: the gate runner is now `cargo nextest run --workspace` (user request; installed 0.9.138, zero doctests). Full suite 186 s vs 235 s under `cargo test`. The two M7 battery tests are marked `#[ignore]`: the default gate is 14 s / 455 tests; the full gate is `cargo nextest run --workspace --run-ignored all` (~3 min). The instrument itself is unchanged and still runs in full gates.

## 1. OTel-compatible telemetry (wave 1, `22c721c`)

New crate `crates/kanbei-telemetry` + optional `otel` feature on kanbei-session (the workspace's first optional dependency / `[features]` section). Default builds carry zero telemetry fields or dependencies.

- Dep-free OTLP/HTTP protobuf-JSON emitter (hand-rolled, matching the workspace's minimal-dependency discipline; the guest already hand-rolls JSON). `Sink` trait (`kanbei-telemetry/src/lib.rs:29`) is the seam for future HTTP/gRPC exporters; `FileSink` appends newline-delimited OTLP payloads.
- Correlation: `trace_id` = session id (base64, protobuf-JSON bytes encoding), run span (`kind=2`, span id = run id) opened in `run_start` and closed in `run_outcome` with status + usage attributes; child spans for each commit (first/last seq, count, frame_len, objects from `CommitReceipt`), `checkpoint`, and `continue_from` (transition seq, branch id). Run span also closes on `cancel_active_run` and the quiesce path.
- Storage reporting gauges (emitted at run outcome + explicit `Session::report_storage()`): `kanbei.objects.count` / `kanbei.objects.bytes` (scan + `fs::metadata` — the store has no byte accounting, and none was added), `kanbei.log.bytes` / `kanbei.log.seq`, `kanbei.projection.bytes`.
- Canonical-only principle (dogfooding instrument, `dogfood.rs:5`): every span attribute and metric derives from canonical records (`run_start`/`run_outcome`, `CommitReceipt`, checkpoint payloads), never private session internals.
- Tests: 6 emitter unit tests (base64 RFC vectors, span/metric JSON shape, escaping) + `tests/telemetry.rs` integration test asserting trace id = session id, run span id = run id, canonical seq attributes, and storage gauges matching live canonical values.

## 2. Automatic canonical-object GC (wave 2, `76ac00a`)

Architecture mandate (`architecture.md:569`): "coordinated root capture, writer pins, quarantine, and a grace period from last reference". Implemented as a two-phase mark-quarantine-sweep in new crate `crates/kanbei-gc`, with quarantine primitives in kanbei-objects.

- **Root capture** (full canonical-reference analysis): session log walk (`Envelope.refs`, `snapshot`, `$object` promotion markers, checkpoint payload digests) + per-manifest `manifest_closure` expansion (which covers module package pins, memory roots, composition, config digests) + live session roots (current snapshot, config digest, pinned roots, branch records, compacted summaries). Memory side: transition-log walk + `RootManifest` expansion + live head/fold (`MemoryCollector`).
- **Writer pins**: `commit()` registers every digest it installs before install and releases after the log append (`GcPinGuard`, error-path safe). Pins are treated as referenced in collection and consulted again at delete time.
- **Quarantine**: unreferenced objects are renamed (same-filesystem, atomic) into a sibling `.gc/` directory; the quarantine file mtime is the last-reference clock. `scan()`/`prune_scan` semantics unchanged (directories skipped).
- **Grace period**: configurable (`GcConfig { grace: Duration, sweep: bool }`, default 7 days). Sweep re-runs the full reference walk, deletes only quarantined objects still unreferenced and older than grace, and removes duplicate/re-referenced quarantine copies (restoring when the main copy is missing — never loses the only copy). Re-validation makes concurrent commits safe: an object installed mid-collection that becomes referenced before the sweep is rescued, never deleted.
- **Canonical fact**: `Session::run_gc` appends a `gc.run` envelope with the serialized `GcReport` (scanned/quarantined/swept/cleaned counts, grace), snapshot-pinned like any state-changing record. Auto-GC at open (when configured) is best-effort and records nothing (an event per open would grow the log); the explicit `run_gc` is the inspectable fact.
- Memory GC via `MemoryRootActor::run_gc`; telemetry hook (otel-gated) re-reports storage gauges after GC.
- Tests (15): quarantine primitives; orphan collect/sweep; grace boundaries; closure survival (envelope refs, snapshot-closure objects, live snapshot, config, module package pins); writer-pin race; duplicate cleanup; canonical `gc.run` record; crash-safe reopen with auto-GC; post-GC export verifies with nothing missing.
- Honesty rule preserved: GC deletes only objects unreferenced by all canonical state; deleted bytes are never reconstructed from SQLite (out-of-band deletion semantics unchanged).

## 3. Multi-module UI composition (wave 3, `58e0d3b`)

Architecture gates (`architecture.md:607`, `high-level-architecture.md:357`): multi-module slots/reducers/atomic composition with a slot/focus/fallback spec and a latency budget.

- **Slots**: `UiMountContribution.slot: Option<String>` (serde default; `None` → `"main"`), charset-validated in the registry (`validate_ui_slot`), normalized to canonical form so composition digests stay canonical. Host op 7 accepts an optional `"slot"` in `ui` payloads.
- **Multi-mount bind**: `rebind_ui` binds ALL root-scope mounts ordered by (slot, scope path, name); `UiHost` now holds `Vec<BoundMount>` (slot/name/component/generation/tree/focus/reducer state/degraded/denied intents). Single-mount behavior is byte-identical (gate_m5 regression green).
- **Composite rendering**: `SemanticTree::compose` builds a synthetic never-focusable root with per-mount id-prefixed children; the existing kernel render pipeline is unchanged; kernel accessibility validation runs per mount. Composite materializes only for 2+ mounts.
- **Fan-out reducers**: input events dispatch to every mount's `ui_reduce` in slot order with the focused mount's slot as a `target` hint; each mount returns its own state + intents. `apply_ui_intents` intersects capabilities per mount's generation — a denied intent in one mount does not affect another. A mount fault degrades only that mount (placeholder subtree); the other mounts keep rendering and applying intents (upgrade over whole-UI degradation). Existing fallback classes (last-valid + staleness banner, kernel render fault → safe mode) unchanged.
- **Focus**: kernel-side over the composite tree; arrows move within the focused mount, Tab/Shift-Tab cycles mounts with remembered node restore.
- **Mid-session deactivation** (M5 deferred item): `replace_module` removes the replaced generation's mounts, stages the new generation's contributions (fixing a pre-existing re-mount gap), and rebinds on success and every failure path.
- **Latency gate** (input ACK p99 ≤ 50 ms with ≥ 1 background wake/s): measured **p99 = 1.38 ms** (max 1.41 ms) over 200+ iterations under 201 concurrent background wakes — 36× under budget, consistent with the ratified S2 3.4 ms figure.
- Tests (12): two-mount composition, fan-out, per-mount grants, focus cycling, deactivation, fault isolation, atomic fallback, latency.

## 4. Dense retrieval — entry criteria (wave 4a, `6bbf38e`)

Re-entry rule (`architecture.md:695,715,744`; `review-reconciliation.md:236`): dense retrieval ships only if the memory benchmark plan justifies it — "FTS-only vs +dense ablations on synthetic coding histories". Delivered the ablation instrument; the milestone decision is on its data.

Instrument `crates/kanbei-retrieval/tests/dense_benchmark.rs` (deterministic, 1.3 s): 500-claim synthetic coding corpus (40 hand-written targets + 460 seeded fillers, 3 scopes); 20 lexical queries (FTS should hit) + 20 hand-built gap queries (paraphrase/synonym/abbreviation/reorder, asserted token-disjoint from targets); reference dense stage = deterministic char 3–4-gram hashing → 512-dim, sublinear TF, L2 (prototype only, no production code); fusion = reciprocal-rank over pipeline top-20 + dense top-50.

| metric | fts-only | dense-only | fusion |
|---|---|---|---|
| recall@5 lexical | 1.000 | 1.000 | 1.000 |
| recall@5 gap | 0.000 | 0.100 | 0.100 |
| recall@10 gap | 0.000 | 0.200 | 0.200 |

Dense latency p95: 0.73 ms @500 / 2.93 ms @2000 / 7.52 ms @5000 (brute-force cosine; linear → ANN required at 100k). Determinism verified across runs.

**Decision: defer implementation.** The cheap embedded stage closes only form-overlap gap variants (4/20 @10: panic/boot/initrd, connections, checkpoint/pointer, kept/keeps); pure semantic paraphrases ("gcd" ↔ "greatest common divisor") share zero n-grams and remain unserved, and closing those requires an external embedding model + ANN infrastructure — against the architecture's minimal-dependency stance while exact-entity + FTS5/BM25 + one-hop already delivers the M7-measured recall@5 1.0 with zero regression risk (lexical stays 1.000). The ablation instrument remains in-tree as the re-entry benchmark: any future dense candidate (real embedding model, sqlite-vec or other index) must beat gap recall@5 ≥ 0.6 with lexical ≥ 0.9 and p95 < 5 ms @5k to re-open the lane. The deferral is a recorded milestone outcome, not a gap: the entry criteria now exist and are measured.

## 5. Engineering

- Gate runner switched to cargo nextest (user request): full gate 186 s vs 235 s; default dev gate 14 s with the battery instrument behind `#[ignore]` (run explicitly via `--run-ignored all` — unchanged instrument, always run at milestone/CI gates).
- First optional dependency / `[features]` section in the workspace (telemetry), no other new external dependencies across the milestone; kanbei-guest/vm untouched; kanbei-ui remains free of vm/modules dependencies (consistency-13).
- Workspace: 457 tests (default) / 458 (all-features) including the battery; clippy `-D warnings` clean fresh-linted on all 20 crates + guest, both feature modes.

Deferred-again (unchanged): replaceable no-effect retention policy runtime, upcaster framework machinery, working-tree snapshots, remote clients/A2A, lifetime automatic promotion — outside M8 scope. Dense retrieval now has a standing entry-criteria instrument instead of a bare R-20 marker.
