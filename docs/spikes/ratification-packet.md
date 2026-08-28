# Phase 0 Gate — Kernel-review ratification packet

Status: ready for review. Date: 2026-08-28. Source: spike reports S1–S6, S17 (`docs/spikes/*.md`, code in `spikes/`). Input to the kernel review per the review-reconciliation record: S1/S2 report back, ratify the provisional budget table and hosting fallback tree; R-22 pre-M1 ratifications (Base58 width, bootstrap descriptor schema v1); S3 format freeze.

## 1. Hosting fallback tree (S1) — R-19

**Keep Wasm-hosted Luaur as the single MVP substrate.** Every provisional budget cleared with ≥2 orders of magnitude headroom (cold start 375 µs vs 100 ms; hot callback 305 ns vs 1 ms; async host-call round trip 335 ns; fuel/epoch interruption deterministic and fast; memory-limit traps contained; respawn 169 µs). Native-Luau tier stays deferred.

Two M2-relevant constraints found: guest builds need `--allow-undefined` (rust-lld import quirk) and a single dispatcher host import; luaur must be built with `default-features = false` (the `cli` feature fails on wasip1).

## 2. AppendLog frame format freeze (S3)

- One zstd frame per commit: first record = typed metadata JSONL; then event JSONL records; events never split across frames.
- Metadata: `{stream, schema, first_seq, last_seq, count, prev, digest, created_us}`; `digest` = blake3 over canonical bytes (metadata minus digest field + event lines); `prev` = previous frame digest (zeros for genesis).
- zstd content checksum ON; **pledged content size ON** (gives O(1) frame boundaries and exact torn-tail truncation without a sidecar index — the key S3 finding).
- Level 3; framing overhead ≈ 9 µs + ~120 B per frame.
- Recovery: magic mismatch → explicit `Corruption {frame, offset, reason}`; incomplete final frame at EOF → truncate to last good offset; verify chain + digest + count + seq continuity.
- `export` = plain JSONL events (zstdcat equivalence), verified at 200k events.
- kill -9 under strict profile: every acked event survives (verified).
- Write-amplification budget (≤2×): holds at ≥8 events/frame (0.37×); **breached at 1 event/frame (2.71×)** — commit path must coalesce; single-event frames are a size tradeoff to be avoided where causality permits.

## 3. Durability queue — M1 commit-path design (S2/S3/S4)

Replace "actor performs fsync" with: **actor ACKs after write() + enqueue; a background durability thread executes fsync/dirsync in FIFO order; effect dispatch and terminal facts call flush() and wait.**

- Event-commit ACK: 17.7 µs p99 at 5k ev/s (vs 9.3 ms synchronous-balanced, 12.5 ms strict) — 480×.
- Ordering invariant (R-10) preserved: object dirsync is enqueued before its referencing event frame, so the object is durable before the frame is fsync-durable.
- Object installs (S4) run through the same queue: write+rename + queued dirsync (22 µs on-path), instead of 17.3 ms blocking install.
- Strict profile still fails the ACK budget (12.5 ms p99) if done synchronously; the queue makes profile differences a flush-cadence question, not an ACK-latency one.

## 4. Budget table ratification (measured vs provisional R-21/H-04)

| Budget | Provisional | Measured | Verdict |
|---|---|---|---|
| Interactive input ACK | p99 ≤ 50 ms @ ≥1 wake/s | 3.4 ms under 1000-outcome flood (balanced) | ratify |
| Event-commit ACK | p99 ≤ 10 ms @ ≥100 ev/s | 9.3 ms sync-balanced; **17.7 µs with durability queue** | ratify (with §3 design) |
| Projection rebuild | ≥10k ev/s streaming | 1.03M ev/s | ratify |
| Wasm callback / cold start | p99 ≤ 1 ms / ≤ 100 ms | 305 ns / 375 µs | ratify |
| AppendLog | amp ≤2×; O(1) verify; O(tail) recovery | 0.37× @≥8 ev/frame; 24.7 µs/frame verify; exact torn-tail | ratify (coalescing note) |
| Breaker trip | ≤ 1 s | fuel 88 µs; epoch 9.9 ms | ratify |
| Snapshot closure | O(active), dedup ≥90% | closure ∝ change frequency (2052 objs / 1000 pins); exact content dedup | ratify |
| Rebuild 5M-event | ≤ 15 min, < 512 MB | 4.85 s, 8.3 MB | ratify |
| Export/closure | ≤ 2× read time | closure verify ≈ read cost | ratify |

**Hardware caveat**: absolute fsync numbers measured on a 5400 rpm disk (root FS). The dogfooding box should re-run S2/S3 quick checks on NVMe before M7; the relative design conclusions hold.

## 5. Base58 width — R-22/H-07 (Maki source: `maki-storage/src/id.rs`)

Maki's `MakiId`: 16-byte UUIDv7 payload, `bs58 = "0.5"` default (Bitcoin) alphabet, canonical form = bare base58, **stable 21 chars for v7 ids** (variable 21–22 for legacy v4), parse accepts legacy hex UUIDs.

Ratification: kanbei `Id128` uses the same bs58 alphabet and 16-byte payload; text form = branded prefix + 21-char body (`ses_<21>`, `br_<21>`, …); parsers require the prefix and accept 21–22-char bodies; legacy-hex acceptance only where Maki data is actually read (not planned for MVP). No format drift after first events (M1 freeze).

## 6. Bootstrap descriptor schema v1 (pre-M1)

Items with spike validation: event envelope (S6: `{env, seq, evt, kind, schema, payload, refs}` — kernel-validated, opaque custom payloads, versioned-record registry), frame metadata (S3 §2), execution-snapshot manifest (S4: environment pins incl. module/state/memory/tool/projection/provider/policy + version fields per R-08/E-12: kernel/bootstrap schema, module ABI, event-envelope schema, engine/toolchain digests — S17 justifies stable digests), memory claim objects (R-12 — not spiked, M4). The S6 reconstruction-report shape (per-kind upcasted/opaque counts, missing objects, upcast errors) is the proposed audit-reconstruction output contract.

## 7. Inline/object threshold (S4)

Data: frame inline cost ≈ 9 µs + ~120 B + payload (compresses ~3–26×); object install 22 µs write+rename + read 6.6–48 µs + reference plumbing; cross-session dedup prohibited (R-29). **Propose: inline ≤ 1 KB, object ≥ 8 KB, 1–8 KB at the kernel's discretion by media type** (grep-friendly small payloads stay inline; large payloads are objects). Ratify or adjust at the kernel review.

## 8. M1 design inputs consolidated

1. Durability queue (§3) — the single biggest commit-path decision.
2. Frame format freeze (§2) with pledged content size.
3. Object installs via the durability queue; per-object temp-fsync decision (S4 finding 5: hash-verification detects damaged objects; relaxing temp-fsync to write+rename is a spec decision, not a spike one).
4. Manifest version fields incl. engine/toolchain digests (§6, S17).
5. Coalesced frames for small events (§2 amplification).
6. Upcast registry + one fixture + reconstruction report shape (S6) as M1 deliverables.

## Open items for the kernel review

- Approval of §5 Base58 (needs Maki parity confirmation), §6 schema v1, §7 threshold.
- Strict-profile semantics with the durability queue (flush cadence for terminal facts).
- Whether the reconstruction report shape (§6) becomes the M1 audit contract verbatim.
