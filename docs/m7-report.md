# M7 Report — Dogfooding Gate

Gate: **green** — 423 tests / 65 suites (~4 min warm, dominated by the battery + scaled unattended run), clippy `-D warnings` clean on all 20 crates including the wasm guest (fresh-linted).
Commits: `bdd3701` (wave 1 battery harness), `27e86cb` (fix: object-promoted intent recovery), `c152577` (wave 2 memory probes), `457d366` (wave 3 chained upcasters), `8caf6a3` (wave 4 workbench binary), plus the final usage-check / lint-clean commit.
Report date: 2026-08-30.
Instrument: docs/dogfooding-instrument.md (ratified pre-M3; thresholds fixed). Memory probes: docs/memory-probes.md (thresholds tuned now, per "No thresholds are enforced until M7 tuning").

## The instrument, exercised

The battery runs the six tasks against real fixture git repositories through the real session kernel — tool FSM (intent commit → approval broker → dispatch → retention gate → outcome), memory substrate (propose → root approval → query), checkpoint/`continue_from`, and SIGKILL recovery — with scripted cognition (the kernel is the object under test, not the model). Every metric is computed from canonical log records only (`run_outcome`, `breaker_tripped`, `tool_intent`/`tool_outcome`/`intent_classified`, `model_outcome` egress, `wake_acceptance`); nothing derives from self-assessment.

### Section 1 — unattended outcome rates (battery task runs: t1–t4 + t5a + t5b)

| Metric | Value | Threshold | Verdict |
|---|---|---|---|
| T1.1 completed goal | 6/6 = 100% | ≥ 80% | PASS |
| T1.2 failed | 0/6 = 0% | ≤ 5% | PASS |
| T1.3 breaker trips | 0 / 6 wakes | ≤ 1 per 1000 | PASS |
| T1.4 stall rate | 0% | ≤ 2% | PASS |
| T1.5 progress rate | 6/6 = 100% | ≥ 90% | PASS |

### Section 2 — interrupted-recovery success (task 6 SIGKILL matrix)

Six kill windows + control: `ready-1..3` (clean pre-step kills), `torn-slow-test` (deterministic poll-kill inside the intent-committed/outcome-uncommitted window — the slow unittest step widens it), `ready-5`, `ready-6`, control.

| Metric | Value | Threshold | Verdict |
|---|---|---|---|
| T2.1 recovery validity | 7/7 reopen valid (log recover, contiguous seqs, no dangling refs, checkpoint closure, reopen+append) | 100% | PASS |
| T2.2 classification honesty | every committed-intent-without-outcome carries `intent_classified` (the torn run produced 1 torn intent, classified `interrupted` at reopen) | 100% | PASS |
| T2.3 resume success | 6/6 killed runs resume to `CompletedGoal`; git log exactly 2 distinct commits, notes files at final content, no duplicate effects | ≥ 90% | PASS |

### Section 3 — cost ceiling (reference rates $5/$15 per 1M in/out)

| Metric | Value | Threshold | Verdict |
|---|---|---|---|
| T3.1 per-task tokens | max 2.2k in / 0.34k out (task 4) | ≤ 250k / 25k | PASS |
| T3.2 battery total | $0.0789 (6 tasks; t5a+t5b $0.0087+$0.0159) | ≤ $6.00 | PASS |
| T3.3 unattended hour | 20 wakes in 60 s at a paced 3 s cadence → $0.0175 → **$1.05/hr scaled** (180 s measurement in the threshold test) | ≤ $2.00 | PASS |
| T3.4 spend breaker | run at 125 tokens trips with `breaker_tripped` {counter: Spend, value: 125, threshold: 50} and pauses cognition (wake denied until resume); under-floor control never trips | canonical + correct | PASS |

Methodology note on T3.3: measured as a dedicated unattended session at a paced cadence (the fake engine's calls are instantaneous; the cadence models real provider latency), spend extrapolated linearly to an hour. The token accounting itself is canonical (egress records).

### Section 4 — per-task success criteria (verified by `battery_task_success_criteria`)

1. **Bug fix**: `mathlib.py` differs by exactly the one-line fix (`return lo` → `return x`); fix commit touches only `mathlib.py`; unittest suite green on the fix commit. PASS
2. **Feature with tests**: `fib` matches the spec byte-for-byte; suite (base + sequence) green. PASS
3. **Refactor**: `parse_csv_line` split into `split_fields` + `unquote`; diff is `csvlib.py` only; no test edits; suite green. PASS
4. **Investigation**: `investigation.md` names the non-atomic write root cause, cites the torn `state.json` prefix as evidence, proposes `os.replace`; the committing diff is the report only; the failing test still fails (no code change required). PASS
5. **Cross-session continuity**: part A implements + commits `gcd`, proposes the memory claim (approved to the project root), checkpoints; part B `continue_from`s, queries memory and cites part A's claim ("gcd implemented in mathlib.py…"), adds only `lcm` (post-transition intents contain no `def gcd`), commits once per part, combined suite green. PASS
6. **Interrupted task**: matrix above; pre-crash effects intact and correctly recorded (file contents + git log). PASS

Battery passes **6/6 tasks** and every section 1–3 threshold.

## Memory probes (docs/memory-probes.md, thresholds tuned at M7)

All 14 probes run against the testkit harness (fake engine, `MemoryRootActor` seeding; no live provider); raw numbers printed by the probe test and recorded here:

- **W1** writing precision 0.667 (20/30 proposed claims matched the human-labeled gold set; ≥ 0.5). **W2** approval rate 1.000 (10/10; ≥ 0.9).
- **R1** recall@5 = 1.000 on the 50-claim seeded fold (20 hand-labeled queries; ≥ 0.8). **R2** superseded best-match queries surface the survivor with `supersedes: true`. **T1** re-proposed contradiction retracts the older claim from later queries; annotation preserved. **T2** healthy recency (4 of the last 10 seeded claims returned; oldest hit index 0 by design).
- **A1** 10/10 seeded projections carried memory fragments vs 0/10 fresh (memory changes the answer surface). **A2** cache outcomes observed: 1 Hit, 1 Invalidated, 2 Misses (stable-prefix reuse works; churn on state change).
- **C1** claim short-circuit: 2 `fs.read` after the claim vs 3 without. **C2** child query returns the project claim; ChildDone wake produces a productive follow-up.
- **L1** projection latency p50 5.52 ms / p95 5.57 ms on the 64-event ring + 50-claim fold (≤ 100 ms). **L2** fragments 775 (mem.project) + 917 (ev.memory) tokens ≤ 4096/2048 budgets.
- **G1** 50 sessions: 40 active / 10 edges / 10 retracted — no 2:1 retraction flag. **G2** reconcile 200=26 ms, 500=52 ms, 1000=98 ms (≤ 5 s).

## Consistency 4/14 — custom schemas/upcasters broadened (M7 scope item)

`Registry::upcast` now walks the registered chain (v1 → v2 → v3); a gap ends the chain; `Ok(None)` only when nothing is registered at the record's own schema; mid-chain errors propagate into `upcast_errors`. New v2→v3 fixture + five chain tests; projection test proves a mixed v1/v3/future-kind log reconstructs with precise partial availability (v1 upcasted to v3, v9 opaque with the `no upcaster` reason, no errors).

## M5 deferral — workbench binary with real stdin wiring

`kanbei-session`'s `workbench` binary opens a session, activates the builtin workbench UI, and drives it from real stdin: raw termios when stdin is a tty, direct reads when piped; escapes, bracketed paste, and mouse sequences flow verbatim into `ui_handle_input`; Ctrl-C exits with the terminal restored. Smoke test pipes a paste burst + mouse escape + Enter + Ctrl-C and asserts the committed `user_message` text and a clean reopen. Two decoder-boundary findings shaped the design: byte-at-a-time feeding breaks escape sequences (`finish()` drains incomplete sequences after every input), and the bracketed-paste terminator defers its ESC so a trailing Enter must arrive as a separate read — real terminals deliver it that way.

## Manual usage check (architecture.md line 610 — deferred-GC compensation)

Task-1 session dir after the full battery: 3 objects on disk, 3 referenced, **0 orphans** — no growth beyond the referenced closure; compaction deferral holds for the battery's workload. The check is part of `run_battery`'s report (`usage_check`).

## Hardware re-ratification (M1 caveat)

The dogfooding box's root FS is `/dev/nvme0n1p2` (ext4, 915 G) — the M1 "5400 rpm" label was inaccurate; it has been the NVMe all along. Fresh numbers: fsync 8.45–8.60 ms avg (50 samples) — identical to the recorded value, so no latency conclusions change. The battery itself is the S2/S3-scale re-run on this box (13+ sessions, ~2 500 events incl. the crash matrix) and completed within the recorded envelopes.

## Findings the dogfooding caught

1. **Kernel bug (fixed, `27e86cb`)**: tool intents promoted to the object store (process.exec payloads > 1 KiB) were invisible to B-05 recovery — `scan_pending_intents`/`scan_classified_intents` read `call_id` straight from the envelope, so a torn promoted intent was dropped from classification entirely (silent unclassified intent, violating T2.2's honesty invariant). Fixed with a store-backed `resolved_payload()`; the battery's torn-window run is the regression test.
2. **Instrument pitfalls (harness-side)**: an empty fixture repo silently produced `CompletedGoal` runs (the tool FSM records error outcomes and the scripted plan continues) — the per-task success criteria are what catch it, and the battery now populates fixtures; FTS5 AND-joins query tokens, so the citation query must use tokens present in the claim; search-result claims carry `text` not `content`; part-B effect dedup must filter by branch (the transition seq), since part A's intents share the log.
3. **Task 5 follow policy**: the transition recorded `FollowHead` — the propose tool is project-scoped and no lifetime claim existed, so the checkpoint pins no lifetime root and PinnedAt is unavailable by design (M6). The head fold equals the checkpoint era (no later claims), so the citation works; PinnedAt engages once a lifetime root exists.

## Qualitative axes (review input, per instrument §4)

- **Coherence**: single canonical event stream end-to-end; the battery reads it for metrics, the session rebuilds state from it, and the gate asserts the same facts.
- **Memory usefulness**: probes above; the citation flow (propose → approve → checkpoint-pin → query in a fresh session) works end-to-end.
- **Extension ergonomics**: the workbench binary and the chained upcasters both landed as additive, pattern-following changes; no kernel design changes were needed for either.
