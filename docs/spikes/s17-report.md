# S17 Spike Report — Luaur/Wasmtime cross-version determinism

Status: complete. Date: 2026-08-28. Disposable spike code: `spikes/s17-determinism/` (uses the S1 guest). Gate: M1 manifest freeze (informs E-12: engine/toolchain digests in the execution-snapshot manifest).

## Question

Is byte-level re-derivation (two-tier honest reconstruction, R-05/E) viable — i.e., does the same Luaur version produce identical bytecode across instances, processes, native vs in-wasm, and clean rebuilds — and is the guest wasm binary itself reproducible so the manifest's toolchain digest is stable?

## Method

- Same source compiled: (a) natively, 1000× in-process; (b) in the S1 wasm guest (`kb_compile_out`), 10 fresh wasmtime instances and 2 separate processes; (c) wasm binary rebuilt twice from clean (`cargo clean -p kb-guest` + build), sha256 compared.

## Results

| Check | Result |
|---|---|
| Native, 1000 compiles in-process | all identical (296 B bytecode, blake3 `c7a39160…`) |
| In-guest, 10 fresh instances | all identical, same digest `c7a39160…` |
| In-guest, cross-process (2 runs) | identical digest |
| Native vs in-guest digest | **exact match** (same luaur version ⇒ same bytecode everywhere) |
| Wasm binary, two clean rebuilds | byte-identical sha256 `683be3f6…` |

## Findings

1. **Determinism within a pinned luaur version holds completely** — in-process, cross-instance, cross-process, and native-vs-wasm all produce identical bytecode. The re-derivation path (context/bytecode) is deterministic given the pinned engine; the manifest's engine/toolchain digests are therefore meaningful and sufficient.
2. **The wasm binary is reproducible across clean builds** with the current toolchain (no embedded build stamps observed). The manifest can pin a stable `kb_guest` digest; E-12's version fields are validated as load-bearing.
3. **Version-pinning is the entire contract**: determinism is *within* a version; the manifest must pin luaur version + wasmtime version + guest digest (as specified in R-08/E-12), and a change in any of them invalidates byte-level re-derivation — the two-tier honest story (R-05/E): provenance always reconstructable, byte-level best-effort with explicit unverifiable marks.

## M1 inputs

- Manifest version fields: luaur crate version, wasmtime version, guest wasm digest — all cheap and now empirically justified.
- No additional determinism machinery needed for the MVP; byte-level re-derivation can rely on pinned digests.
