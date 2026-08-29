# M1 Milestone Report — Durable kernel

Status: complete. Date: 2026-08-29. Milestone: M1 per the per-milestone acceptance matrix (architecture.md R-21/H-06). Authorized by the Phase 0 gate ratification ("Ratify all + authorize M1"). Code: `crates/kanbei-{core,log,objects,session,snapshot,projection,testkit}` (Cargo workspace, edition 2024, rustc 1.98.0).

## Deliverables

| Crate | Contents |
|---|---|
| `kanbei-core` | `Id128` (Maki-parity UUIDv7 + Base58, branded `ses_`/`br_`/`ev_`), `Digest` (`blake3:<64hex>`), `Envelope` (S6 shape + M1 `snapshot` field), upcast `Registry`/`Report` (S6 shape), `DurabilityQueue` (FIFO fsync/dirsync executor) |
| `kanbei-log` | AppendLog with the frozen S3 frame format (pledged content size, checksums, blake3 chain, level 3), profiles Fast/Balanced/Strict through the durability queue, torn-tail truncation, `for_each_frame` streaming verify, `export` |
| `kanbei-objects` | Flat `<alg>:<digest>` store, install = write+rename + queued dirsync (per-object temp-fsync relaxed per packet §8.3 — hash verification detects damage), hash-verified `get`, `scan`, `prune_scan` (counts only; GC is post-MVP) |
| `kanbei-session` | Serialized single-writer commit path: object installs → ref verification (no dangling refs, R-10) → payload classification (§7) → envelopes against the pre-event snapshot (R-08) → one frame per commit → post-event manifest pin on state changes. `FaultPoint` injection for crash testing. Ack = write+enqueue (§3); `flush()` before consequential effects |
| `kanbei-snapshot` | `ExecutionManifest` (env pins + version fields incl. envelope/kernel schema, module ABI, engine/toolchain digests), `bootstrap()`, `pin` (content dedup), `verify_closure` |
| `kanbei-projection` | `reconstruct` (audit contract: per-kind upcasted/opaque + precise missing_objects + upcast_errors, S6 shape), `rebuild` (disposable SQLite: WAL, `synchronous=OFF`, tx/1000, watermark in same tx as rows — R-23) |
| `kanbei-testkit` | Crash-injection harness (`crash-child` subprocess with abort-at-fault-point, env-driven), seeded xorshift property RNG, `verify_recovery` invariant checker, M1 gate tests |

Workspace: 94 tests, ~7 s. Clippy clean (`-D warnings`) on all crates.

## M1 gate results

Acceptance bullets (architecture.md:631-643):

| Bullet | Test | Result |
|---|---|---|
| SQLite deletion → audit reconstruction | `acceptance_consistency_12_sqlite_deletion_reconstruction` | pass (identical Report after DB deletion, incl. WAL siblings) |
| Crash injection at object install / event commit → explicit valid recovery | `crash_matrix_object_install_and_event_commit` (4 points × {fast, strict}), `flood_kill_seeded` (5 seeded SIGKILL floods) | pass — every crash recovers; acked ≤ R ≤ acked+1; no dangling refs; reopen-and-append works |
| Execution-snapshot closure verifies | `acceptance_consistency_4_snapshot_closure_verifies` | pass (closure hash-verified; dedup: identical manifests → 1 object; prune_scan orphans = 0) |
| Custom schemas/upcasters reconstruct or report precise partial availability | `acceptance_consistency_14_upcast_fixture` | pass (future_kind schema 9 → opaque with exact reason; empty registry → all opaque) |

Consistency tests exercised at M1 (architecture.md:645-663): 3 Canonical fact, 4 Snapshot, 5 Payload, 6 Crash, 7 Recovery, 11 Causality, 12 Projection, 14 Evolution — all pass (`tests/gate_m1.rs`).

Crash-matrix detail (acked=4, crash during commit 5): `BeforeObjectInstall` R=4 orphans=1/13 · `AfterObjectInstall` R=4 orphans=2/14 · `BeforeFrameAppend` R=4 orphans=3/15 · `AfterFrameAppend` R=5 orphans=0/15 (ack-in-flight, S3 drill shape). Orphan growth with crash depth is the designed R-10 semantics: crashes may leave orphans, never a committed dangling reference.

## Spec decisions taken at M1 (packet open items)

1. **Per-object temp-fsync relaxed** (packet §8.3): install = write+rename + queued dirsync; `get` hash-verifies and reports `Corruption` naming expected/actual digests. The ordering argument holds: the object's dirsync precedes the referencing frame's fsync on the same FIFO queue.
2. **Inline/object threshold applied** (packet §7): inline ≤ 1024 B verbatim; > 1024 B → object + `{"$object": "<digest>"}` payload marker with the digest in `refs`; the 1–8 KB middle band stays inline (M1 kernel default).
3. **Coalescing** (packet §8.5): one frame per commit call; the coalescing lever is caller-side bounded-chunk commits (the S2 measured load model). ≥8-event frames hold the ≤2× amplification budget (test `write_amplification_below_raw`). A kernel-side hold-buffer is a small M2 addition if the review wants it.
4. **Envelope gains a `snapshot` field** (R-08): the S6 shape `{env,seq,evt,kind,schema,payload,refs}` + `snapshot` = pre-event execution-snapshot digest (null on resume). Kernel-owned field; part of the M1 freeze. No events exist yet, so no drift concern.
5. **Seq base 1**: M1 envelopes start at seq 1 (the S3 spike was 0-based); byte-parity test proves encoder identity modulo this field.
6. **Session resume**: a resumed session commits with `snapshot: null` (manifest state is not re-derived at open); audit reconstruction is the authority for resumed-session truth. Re-derivation of `current_snapshot` from the log is a possible M2 refinement.
7. **No threaded actor at M1**: the session is a synchronous single-writer struct; the threaded actor with responder lanes ships at M2 with outcome processing (S2's numbers already validated the design; nothing to re-measure at M1).

## Format freeze

- Frame metadata `{stream, schema, first_seq, last_seq, count, prev, digest, created_us}`; digest = blake3 over canonical (metadata minus digest + event lines); zstd level 3, checksums + pledged content size ON. Byte-identical to the S3 encoder modulo the seq base (verified by test).
- `Id128`/`Digest`/envelope JSON shapes frozen as of this milestone; no format drift after first events (R-22/H-07).
- Hardware caveat stands: absolute fsync numbers are 5400 rpm disk; dogfooding box re-ratifies on NVMe before M7.

## M2 inputs

- Threaded session actor + responder lane priority (S2 design, measured).
- Module ABI + engine/toolchain digests populate manifest fields; wasm hosting joins via the S1 path.
- Kernel-side coalescing if the write-amplification budget tightens.
- `current_snapshot` re-derivation on resume, if the audit contract demands it.
