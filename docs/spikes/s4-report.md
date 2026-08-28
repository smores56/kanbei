# S4 Spike Report — Snapshot closure, manifest materialization, object store at scale

Status: complete. Date: 2026-08-28. Disposable spike code: `spikes/s4-snapshot/` (never promoted to the implementation).

## Question

Do execution-snapshot manifests materialize cheaply, does content addressing deliver the "closure O(active) not O(history)" claim, and does the per-session object store survive 1M-file scale? Budgets: snapshot closure O(active) with ≥90% dedup; object-install protocol (R-10/B-03) cost; closure verification cost.

## Environment note (load-bearing)

The root filesystem (`/`, where XDG state lives) is a **5 TB 5400 rpm spinning disk (sda, ST5000LM000)**. The box's NVMe drives (980 PRO, 970 EVO) are not mounted for state. Every fsync here costs ~8.5 ms. Absolute numbers below are spinning-disk numbers; relative findings transfer, and the dogfooding box should re-ratify absolute budgets on NVMe.

## Results

| Test | Result |
|---|---|
| Manifest size | 130–153 B JSON |
| Pin cost (full install protocol: temp-fsync + rename + dirsync) | 17.3 ms avg — fsync (8.5 ms) + dirsync (8.5 ms) |
| Pin cost (batched dirsync) | 8.6 ms avg — the per-object temp-fsync remains |
| Pin cost (write+rename only, durability-queue design) | 22 µs |
| Manifest dedup | mechanism exact (43/43 identical manifests deduped); ratio is a function of change frequency |
| Closure size (100k events, state change every 100) | 2052 unique referenced objects for 1000 pins — **closure scales with change frequency, not event count** |
| Closure verify (hash-check each object) | 6.6 µs per referenced object |
| Object store install: 100k objects, full protocol | 116 obj/s (8.7 ms/obj — temp-fsync bound) |
| Object store install: 1M objects, write+rename | 45.5k obj/s (22 µs/obj) |
| Random read + hash verify | 6.6 µs @100k objects, 48 µs @1M |
| List (readdir) | 100k: 56 ms; 1M: 559 ms |
| Prune scan (orphan detection against referenced set) | 1M objects: 1.15 s |

## Findings

1. **Content-addressed dedup works exactly**: identical environment pins map to the same object, and the closure stays proportional to *change* frequency (2052 objects for 1000 pins at change-every-100 — the "O(active) not O(history)" claim holds; the ≥90% dedup budget refers to the closure, which is satisfied structurally once manifests pin stable references).
2. **The install protocol cannot block the session actor on this hardware**: full-protocol installs cost 17.3 ms (temp-fsync + dirsync, both ~8.5 ms on the 5400 rpm disk). Any object-bearing event commit that waits on its object install violates the 10 ms p99 ACK budget by ~2×.
3. **M1 design input — extend the S3 durability queue to objects**: the actor installs objects as write+rename and enqueues the object's dirsync, then appends the referencing event frame and enqueues its fsync; the background durability thread executes both in FIFO order, so the object's dirsync is durable before the referencing frame is fsync-durable — the R-10 ordering invariant holds without blocking the actor. Event ACK stays at the S3 fast-profile cost (~20 µs); effect-dispatch flush waits for both.
4. **Flat `<alg>:<digest>` directory is viable at 1M files**: reads 48 µs, list 559 ms, prune scan 1.15 s. No sharding needed for the MVP; sharding becomes a scale decision, not a correctness one.
5. **Read-verify is cheap** (6.6–48 µs), so closure verification per event (S4's other half) is not a rebuild bottleneck.

## Open questions for M1 spec

- Whether the per-object temp-fsync can be relaxed to write+rename when the durability queue covers the dirsync (the ordering argument in finding 3 holds for the *reference*; the temp-fsync protects against a damaged-but-named object after power loss, which hash-verification already detects as explicit corruption — worth a spec decision at M1, not a spike).
- Shard depth if a single session exceeds ~10M objects (post-MVP).
