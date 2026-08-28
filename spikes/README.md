# Spikes

Disposable research code per the architecture constitution (`high-level-architecture.md`,
"Mandatory design-review gate"): explicitly classified, never promoted into the
implementation, never merged into M1+ crates. Each spike produces a report in
`docs/spikes/` and the code is discarded or archived once the report is ratified.

## Register

| Spike | Topic | Gate | Status |
|---|---|---|---|
| S1 | Wasm-hosted Luaur hosting: cold start, hot callback, async host-call round trip, fuel/epoch interruption, store limits, config-compile latency, trap+respawn | M2 precondition + hosting fallback tree | complete — `docs/spikes/s1-report.md` |
| S2 | Session-actor throughput under mixed command/outcome + wake chain + chunk commits | Kernel review | complete — `docs/spikes/s2-report.md` |
| S3 | AppendLog framing: append latency vs JSONL, chain verify, torn-tail drill, zstd frame-size sweep, kill -9 durability profiles, dirsync cost | Kernel review (format freeze) | pending |
| S4 | Snapshot-closure growth + manifest materialization + prune | Kernel review | pending |
| S5 | Rebuild throughput/memory (1–10 GB streams, 5M-event session) | Kernel review | pending |
| S6 | Version-pinned reconstruction across a kernel-upgrade fixture | Kernel review | pending |
| S17 | Luau/Wasmtime cross-version determinism | M1 manifest freeze | pending |
