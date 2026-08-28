# S2 Spike Report — Session-actor throughput

Status: complete. Date: 2026-08-28. Disposable spike code: `spikes/s2-actor/` (never promoted to the implementation).

## Question

Is one serialized session actor sufficient for the canonical commit path, and do the provisional budgets hold? Budgets under test (architecture.md R-21/H-04): event-commit ACK p99 ≤ 10 ms at ≥ 100 events/s sustained; responder input latency under background load (related H-05 gate); durability profiles.

## Environment and method

- Host: AMD Ryzen 9 5950X, Linux (NixOS), ext4 (`rw,relatime`), release build
- Model: serialized actor thread; responder lane drained exhaustively before the outcome lane (blocking receive with 1 ms timeout otherwise); per commit: in-memory FSM (sequence bump) → one zstd frame (level 3, JSONL records) → durability profile → optional SQLite in-memory projection insert (one tx per commit) → stats
- Profiles: `fast` (no fsync), `balanced` (fsync every 10 frames — the documented default), `strict` (fsync per frame)
- Loads: sustained paced 100/s and 1000/s (5 s, mixed user msgs + 4-event chunk commits), burst 10k commands, wake chain (10 ms simulated model call, 100 cycles), responder-priority (1000 pending outcomes + 1 user msg)
- Filesystem fsync cost measured implicitly: strict-profile commit avg ≈ 8.5 ms

## Results

| Load | Profile | Result |
|---|---|---|
| Sustained 100 ev/s, 5 s | fast | p99 830 µs (user), 117 µs (chunk); achieved 286 ev/s |
| Sustained 100 ev/s, 5 s | balanced | p99 9.3 ms (user), 8.6 ms (chunk) — **inside 10 ms budget, no headroom** |
| Sustained 100 ev/s, 5 s | strict | p99 12.5 ms — **violates 10 ms budget** |
| Sustained 1000 ev/s, 5 s | balanced | p99 8.5 ms — still inside budget; achieved 2.7k ev/s |
| Sustained 100 ev/s | balanced, no SQLite | p99 9.0 ms — identical; **SQLite insert is not a bottleneck** |
| Burst 10k cmds (13.3k events) | fast | drain 57 ms, 58.9k ev/s ceiling |
| Burst 10k cmds | balanced | drain 2.85 s, 1.17k ev/s (fsync-cadence-bound) |
| Burst 10k cmds | strict | drain 28.8 s, 116 ev/s — **hard fsync wall; strict cannot sustain the rate budget** |
| Wake chain (10 ms delay) | balanced | wake-to-wake p50 10.2 ms, p99 19.3 ms (~1.7 ms actor overhead per link; +8.5 ms when a link pays the fsync) |
| Responder priority (1000 pending outcomes) | balanced | user-msg latency 3.4 ms (waits only for the in-flight commit, not the queue) |
| Responder priority | strict | 10.6 ms (waits for in-flight fsync) |

## Findings

1. **One serialized actor is sufficient** for the budgeted loads: commit-path processing (zstd frame + in-memory SQLite tx) costs ~45 µs; the actor sustains 58.9k ev/s with fsync off. The single-writer design (R-19) is validated.
2. **Fsync is the entire story.** Every commit that performs the periodic fsync pays the full fsync latency (~8.5 ms on this box, ext4). Balanced p99 9.3 ms barely passes at 100 ev/s; strict fails both the ACK budget (12.5 ms p99) and the sustained-rate budget (116 ev/s). The fast-profile numbers (p99 830 µs) show the actor itself has ~100x headroom.
3. **M1 design input — take fsync off the actor's critical path.** A dedicated fsync thread/queue: the actor ACKs after `write()` + handoff to the fsync thread; the fsync thread performs the bounded-interval sync; effect dispatch and terminal-fact commits wait on the fsync thread's completion (preserves fsync-before-consequential-effect). This converts balanced-profile p99 from 9.3 ms to fast-profile (~1 ms) while keeping the durability contract. Cadence should be time-based (e.g. every 100 ms) rather than frame-count-based, so the interval trigger never coincides with an interactive commit burst.
4. **SQLite in-memory projection insert is negligible** on the commit path (<1% vs fsync; identical p99 with and without). On-disk SQLite (WAL) is an M1 measurement, not a concern signal.
5. **Responder priority works at the actor**: under a 1000-outcome flood, a user msg waits only for the in-flight commit (3.4 ms balanced). The residual is bounded by the current commit's cost — which item 3 removes.
6. **Wake chain overhead is ~1.7 ms per link** at 10 ms delay (two commits + thread spawn/wake); the chain itself does not accumulate latency.

## Open questions for M1 spec

- fsync thread vs io_uring vs periodic timer flush — S3 (AppendLog framing spike) should benchmark the fsync-off-critical-path design and the durability-profile wording.
- Whether `fast` profile may ACK without the frame reaching the OS (`write`-only, "may acknowledge kernel-buffered writes" per architecture.md) — S3 drill.
- Thread-per-wake in the chain simulation is a proxy; the real scheduler reuses a completion channel — no signal of concern, but the wake-path cost should be re-measured at M3 with the real provider gateway.
