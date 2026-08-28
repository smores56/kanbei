# S5 Spike Report — Projection rebuild throughput and memory

Status: complete. Date: 2026-08-28. Disposable spike code: `spikes/s5-rebuild/` (never promoted to the implementation).

## Question

Can the disposable SQLite projection be rebuilt from a large canonical stream within budget? Budgets (R-21/H-04): rebuild ≥ 10k events/s streaming; 5M-event stream ≤ 15 min and < 512 MB RSS.

## Method

- Generate a canonical stream with the S3 frame format (64 events/frame, level 3, checksums + chain).
- Rebuild = streaming verify (S3 `for_each_frame`: chain + digest checks per frame, one frame in memory at a time) + SQLite insert (prepared stmt, tx per 1000 events, WAL, `synchronous=OFF` — the projection is disposable per the architecture; watermarks ignored on rebuild as specified in R-23).
- RSS = `VmHWM` from `/proc/self/status`.
- Host note: root FS is the 5400 rpm disk (see S4); rebuild is read-dominated and not fsync-sensitive.

## Results

| Stream | Rebuild mode | Time | Throughput | RSS |
|---|---|---|---|---|
| 1M events (4.4 MB zst) | memory | 0.96 s | 1.04M ev/s | 158 MB (the in-memory DB, not the streaming path) |
| 1M events | file (WAL, sync=OFF) | 1.03 s | 974k ev/s | 7.7 MB |
| **5M events (21 MB zst)** | file (WAL, sync=OFF) | **4.85 s** | **1.03M ev/s** | **8.3 MB** |

Generation: 5M events written in 1.8 s (2.7M ev/s); 550 MB raw JSONL → 21 MB stream (26×).

## Findings

1. **Budget cleared by ~100× on throughput and ~60× on memory**: 1.03M ev/s streaming vs the 10k ev/s target; 8.3 MB RSS vs 512 MB. The 15-minute budget is actually ~185× headroom.
2. **Streaming verify is not the bottleneck** (S3's 24.7 µs/frame includes it); SQLite insert at ~1M ev/s dominates and is fine. No rebuild redesign needed for the MVP scale (even 100M events ≈ 2 min).
3. **Memory mode RSS is the database, not the reader**: 158 MB for 1M in-memory rows. The disposable-projection design (file SQLite) keeps the rebuild path at single-digit MB — confirms "SQLite is disposable" is also the memory strategy.
4. `synchronous=OFF` + WAL is appropriate for a disposable projection: rebuild ignores watermarks, and the projection is recreated from canonical truth; no durability claim is made for it.

## Open questions for M1 spec

- None blocking. Watermark commit-in-same-tx (R-23) adds one row per batch — negligible at this throughput.
- If a single session ever exceeds ~100M events, measure again; until then the budget holds with ~100× headroom.
