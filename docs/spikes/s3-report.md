# S3 Spike Report — AppendLog framing

Status: complete. Date: 2026-08-28. Disposable spike code: `spikes/s3-appendlog/` (never promoted to the implementation).

## Question

Does the R-23 frame format hold up, and what are the concrete numbers for the format freeze? Frame = one zstd frame per commit containing a typed metadata JSONL record + event JSONL records; blake3 digest over canonical records + metadata (digest field excluded, self-reference avoided); local hash chain via `prev`; zstd content checksums; pledged content size in the frame header gives O(1) boundaries. Budgets: write amplification ≤ 2× raw JSONL, O(1) per-frame verify, torn-tail recovery O(tail), balanced-profile commit ACK (S2: 9.3 ms p99 — retested with fsync off the critical path).

## Results

| Test | Result |
|---|---|
| Append latency (level 3): 1 / 8 / 64 / 1024 events per frame | 9.5 / 1.5 / 0.3 / 0.1 µs per event |
| Compression level 1 vs 19 | no measurable difference at these sizes — level 3 is fine |
| Size amplification vs raw JSONL: 1 / 8 / 64+ events per frame | 2.71× / 0.37× / ≤0.06× — **1-event frames breach the ≤2× budget** |
| Chain verify (10k frames × 100 events) | 24.7 µs per frame (O(1): header length + one zstd decode + blake3) |
| Torn tail (truncate inside final frame) | exact truncation to last good offset; 999/1000 frames recovered; `truncated` flagged |
| Mid-file corruption (bit flip in frame 250) | explicit `Corruption` naming the frame and offset; no silent repair |
| kill -9 drill (strict profile, 2 runs) | recovered == acked exactly (run 1); recovered == acked + 1 frame (run 2, ack in flight) — every acked event survived |
| dirsync cost (temp-write + rename + parent-dir fsync) | **8.5 ms avg per install — same cost as a file fsync on this FS** |
| fsync-off-critical-path commit ACK (5k ev/s) | **p99 17.7 µs** (vs 8.5 ms synchronous balanced — 480×) |
| fsync-off-critical-path flush (effect-dispatch wait) | p50 10.2 ms, p99 17.0 ms — the fsync price, paid only where the contract requires it |
| `export` (zstdcat equivalence) | 200k events emitted as plain JSONL, exact |

## Findings

1. **Frame format is sound and cheap.** Metadata record + events per frame, pledged-size header for O(1) boundaries, magic check + digest + chain + count + seq-continuity verification. Torn-tail recovery is exact; mid-file corruption is an explicit error; kill -9 under strict profile loses nothing acked. No sidecar index needed.
2. **Batching is the size lever**: 1-event frames cost 2.71× amplification (metadata ~120 B dominates) and 9.5 µs/frame overhead; ≥8-event frames are below raw JSONL size. The "bounded chunk" commit path (tool/model output in chunks) is therefore also the size strategy; single-event commits (e.g. user messages) should be coalesced where the causal structure allows.
3. **fsync-off-critical-path is validated at 480×**: ACK p99 17.7 µs at 5k ev/s. The design from S2 is correct: actor ACKs after write + queue-to-fsync-thread; flush() (fsync-before-consequential-effect) waits for the background thread — 10–17 ms on this box. This is the M1 commit-path shape.
4. **dirsync costs the same as fsync (~8.5 ms)**: the object-install protocol (temp+fsync+rename+dirsync, R-10/B-03) pays ~17 ms per install if done strictly per object. Design lever for M1: per-directory batched dirsync (install group, one parent-dir fsync before the referencing event commits) preserves the ordering guarantee at ~1/N the cost. Worth a targeted M1 drill, not a spec change.
5. **Blake3 at 24.7 µs/frame verify is not a rebuild bottleneck** (S5 will confirm at 5M-event scale).

## Format-freeze inputs (for the kernel review)

- Frame: one zstd frame per commit; first record = metadata JSONL; `{stream, schema, first_seq, last_seq, count, prev, digest, created_us}`; digest = blake3(canonical minus digest field + event lines); `prev` = previous frame's digest; zstd content checksum ON; pledged content size ON (enables O(1) boundaries).
- Compression level 3, framing overhead ~9 µs + ~120 B per frame.
- Recovery: magic mismatch → corruption; incomplete final frame at EOF → truncate to last good offset; digest/chain/count/seq violations → explicit corruption with frame + offset.
- `export` = plain JSONL events, no metadata.
- Commit-path: append + queue-to-fsync-thread; flush() before consequential effects and terminal facts.
