# S1 Spike Report — Wasm-hosted Luaur hosting

Status: complete. Date: 2026-08-28. Disposable spike code: `spikes/s1-hosting/` (never promoted to the implementation).

## Question

Can Luaur run as a `wasm32-wasip1` guest inside wasmtime with acceptable cold start, hot callback, async host-call, interruption, containment, and respawn behavior? This is the M2 precondition and the input to the hosting fallback tree.

## Environment

- Host: AMD Ryzen 9 5950X (16C/32T), Linux (NixOS 26.11)
- rustc 1.98.0, cargo 1.98.0 (rustup; wasm targets `wasm32-wasip1`)
- wasmtime 48.0.1 (`async` feature), wasmtime-wasi 48.0.1 (`p1`)
- luaur 0.1.8 (`default-features = false`, `features = ["send"]`)
- Guest: `wasm32-wasip1` cdylib, 755 KB, release profile (LTO off, see gotchas)
- All numbers single-run or small-N; treat as order-of-magnitude, not benchmarks

## Results vs provisional budget (architecture.md R-21/H-04)

| Budget | Target | Measured | Verdict |
|---|---|---|---|
| Wasm cold start (engine+module+instance+first call) | p99 ≤ 100 ms | 375 µs total; instance-cold call p99 153 µs | 260x headroom |
| Config-compile latency in guest (9.6 KB source) | (none) | avg 293 µs, p99 433 µs | fine |
| Hot callback (host→guest, cached Luau fn) | p99 ≤ 1 ms | 304 ns/call | 3000x headroom |
| Sync host-call round trip (guest→host) | (none) | 283 ns/call | fine |
| Async host-call round trip (async store + async import) | (none) | 335 ns/call (+18% over sync) | fine |
| Fuel interruption | breaker trip ≤ 1 s | 88 µs @100k fuel, 237 µs @1M; exact, deterministic | far inside |
| Epoch interruption | breaker trip ≤ 1 s | 9.9 ms with 10 ms epoch bump | far inside |
| Memory limit containment | — | guest allocator panic → contained `UnreachableCodeReached` trap at 8 MB and 64 MB; store dropped, host unaffected | contained |
| Trap + respawn (module cached) | — | avg 169 µs, p99 300 µs per cycle | fine |

## Findings

### Viable: no fallback needed
Wasm-hosted Luaur clears every provisional budget with at least two orders of magnitude headroom. The hosting fallback tree resolves to: **keep Wasm-hosted Luaur as the single MVP substrate** (R-19). Native-Luau tier stays deferred.

### Gotchas that transfer to M2
1. **rust-lld drops same-signature imports on wasm32** at final link (observed with two `(i32) -> i32` imports and with one). Fix: build the guest with `RUSTFLAGS="-C link-arg=--allow-undefined"`. Additionally, prefer a **single dispatcher host import** (`kb_host(op, args…)`) over per-function imports — smaller ABI surface, sidesteps the linker quirk entirely.
2. **luaur default features break wasm builds**: `default = [typecheck, cli]`; `cli` pulls `luaur-repl-cli` → `rustyline` → `fd-lock`, which does not compile for `wasm32-wasip1`. Use `default-features = false` (+ `send` if a persistent VM across calls is needed, e.g. a guest `static`).
3. **wasmtime 48 API drift**: no `Store::new_async` / no-op `Config::async_support`; async-ness is per-entry-point (`limiter_async`, `instantiate_async`, `call_async`). `StoreLimits` no longer implements `ResourceLimiterAsync` (async stores can't use it). Fuel exhaustion is `Trap::OutOfFuel`, distinct from `Trap::Interrupt` (epoch).
4. **Fuel metering counts VM initialization**: opening stdlibs in `Lua::new()` burns ~80 µs of fuel per fresh VM. A per-call fuel budget must include init headroom (or pre-warm generations) or the first call traps before guest code runs.
5. **Epoch deadline defaults to 0** → every call traps immediately unless `set_epoch_deadline` is set explicitly. Interruption checks land at loop backedges; a pure `while true do end` loop has no fuel cost (loop/branch are 0-fuel instructions) — metering tests must use arithmetic loops.
6. Guest OOM under wasmtime memory limits surfaces as a guest-side allocator panic → `UnreachableCodeReached` trap (not wasmtime's `OutOfMemory` type). Containment is the point; classification should treat both as store-fatal.

## Open questions for M2 spec

- Whether the kernel wants per-generation fuel budgets vs epoch deadlines as the primary step bound (fuel is exact and cheap; epoch is coarse and needs a bump thread — a timer is required either way; the harness already needs wall-clock budget enforcement per R-24/D-10).
- Whether `luaur`'s `send` feature is acceptable in the guest (enables `static` VM caches; otherwise per-call VM creation costs ~167 µs and recompilation).
- Wasm-ABI freeze inputs: single-dispatcher host ABI confirmed; argument marshalling (scratch buffer vs malloc/allocator export) not yet tested at scale.
