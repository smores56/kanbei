# M2 Milestone Report — Live module substrate

Status: complete. Date: 2026-08-29. Milestone: M2 per the per-milestone acceptance matrix (architecture.md R-21/H-06). Code: workspace crates 14 (+2,494 lines net on top of M1: `kanbei-guest`, `kanbei-vm`, `kanbei-capabilities`, `kanbei-policy`, `kanbei-services`, `kanbei-modules`, `kanbei-scopes` + session/testkit extensions). 202 tests, 14 s warm / 28 s cold-cache. Clippy `-D warnings` clean on all 14 crates (guest included).

## Deliverables

| Crate | Contents |
|---|---|
| `kanbei-guest` | wasm32-wasip1 cdylib embedding luaur 0.1.8: dispatcher ABI (`kb_host`, `kb_host_buf`), exports kb_scratch/compile_out/init/hot_call(_str)/run, hand-rolled deterministic JSON marshalling (integer-exact, sorted object keys), no time/random (S17) |
| `kanbei-vm` | wasmtime 48 host: sync execution (wasi_p1), fuel + epoch-deadline watchdog + StoreLimits + host-side call timeout (R-24/D-10), `Host` trait with generation-token passed on every op, `engine_digest()` for manifests, serialized-module cache (see §Perf), trap mapping (Fuel/Epoch/OOM/StaleGeneration/GuestReturn) |
| `kanbei-capabilities` | Principal, Capability, Grant (digest-bound, domain-separated, scoped/expired/budgeted), ApprovalIntent (R-16/D-12), PolicyTemplate by trust class (R-13/D-04), Broker: intersection check, attenuation-only, monotonic policy guards, dispatch-time recheck (R-16/D-11) |
| `kanbei-policy` | RetentionGate: two-phase admission (size gate → plugin decision), R-04 replay bit (conservative default, kernel-owned), Store/Transform/Drop/RejectExecution + NonResumableBoundary, `PolicyPlugin` no-effect seam (deferred runtime hosts the same trait, R-28/D-S3), built-ins: store-all, pattern redaction (regex) |
| `kanbei-services` | Versioned ServiceContract, ScopePath/ServiceKey, publish with ReplaceIntent, same/ancestor-scope resolution only, dependency-DAG cycle rejection, `replacement::plan_replacement` (rebind vs restart, restart-failure fails the transaction, R-25/C-05) |
| `kanbei-modules` | PackageManifest (origin/trust class/deps/capabilities/source), StateStore with the R-07/B-01/F2 head contract (digest+schema+checksum+last_pinned+seq, CAS through the durability queue, generation-token check, oversize fail-closed, prune-unpinned), ModuleManager (activate/deactivate/replace, disposal record), ModuleHost routing 7 ops (log/state_get/state_set/service_call/check/require_approval/service_publish) with generation-token check FIRST |
| `kanbei-scopes` | Contribution types + fixed kernel conflict rules (commands/tools unique-or-replace, services one-per-key, keymaps layered, themes overlay, stages slots+ordering, UI mounts, guards monotonic), transactional clone-and-swap apply, ScopeTree (root + ephemeral single-level children, owner lease, recursive disposal), CompositionStore with OCC + domain-separated epoch digest (R-01) |
| `kanbei-session` | Wired everything into the canonical commit path: `activate_config` (atomic config reload: stage→validate→OCC-publish→composition_changed event, rollback retains last valid composition), `replace_module` (generation replacement + delta event), `effect_dispatch`, `module_state_cas` (head update through the actor), `retain_candidate` (policy before storage; boundary facts), safe mode on invalid config (R-01/C-02), manifest schema 2 with ModulePins + composition digest, 6 new fault points |

## M2 gate results

| Bullet (architecture.md) | Test | Result |
|---|---|---|
| Generation replacement leaves no stale registrations/tasks/scopes | `acceptance_generation_replacement_leaves_no_stale_state` | pass (old service gone, old token stale, composition delta recorded) |
| Wasm traps do not corrupt the session actor or canonical state | `acceptance_wasm_traps_do_not_corrupt_session` | pass (trap → Err; session commits, recovers, reopens) |
| Capabilities attenuate and stale generations cannot act | `acceptance_capabilities_attenuate_and_stale_cannot_act` | pass (broker denial, attenuation, stale-token rejection) |
| No-effect policy plugins cannot invoke effect capabilities | `acceptance_retention_policy_no_effect` | pass (seam trait is decision-only; redaction leaves no secret bytes in log/objects) |
| Config reload publishes atomically | `acceptance_config_reload_publishes_atomically` | pass (success bumps epoch + event; conflict → Err, epoch untouched, no event; restart with invalid config → safe mode + fact) |
| Crash injection at effect dispatch / config activation / head update | `acceptance_crash_m2_points` | pass — 12-row matrix (6 points × dispatch/head flows); every crash: explicit valid recovery, log contiguous, composition epoch consistent with log, closure refs present |

Consistency tests exercised at M2: 1 Owner (disposal/replacement), 2 Authority (broker), 9 Privacy (redaction before storage), 10 Replay honesty (drop → explicit non-resumable boundary fact, reconstructable), 15 Scope (lifecycle + ephemeral on restart). M1's suite (3,4,5,6,7,11,12,14) stays green — gate_m1 updated only for the manifest schema-2 shapes (M1 semantics preserved: dedup, closure, chain, contiguity).

## Perf fix (post-lane, coordinator)

The first gate run burned 42 min CPU / 294 s wall: wasmtime's default `parallel-compilation` fans a rayon pool (all cores) per Module compile, and the 1.4 MB guest was recompiled per Vm instance. Fix in kanbei-vm: `parallel_compilation(false)` + a serialized-module cache (`Module::serialize`/`unsafe deserialize` — wasmtime 48 has no cache-config API) keyed by wasm digest + wasmtime version at `$KANBEI_VM_CACHE_DIR`/`~/.cache/kanbei`; trust boundary documented (own artifact, local user state). Cold: 22 s one-time codegen; warm: 0.03 s per Vm. Suite: 294 s → 14 s.

## Spec decisions / interpretations (lane-level, all documented in code)

1. **Host ABI** (internal/unstable): 7 ops over the `kb_host_buf` dispatcher (log, state_get, state_set, service_call, check, require_approval, service_publish); JSON payloads marshalled through the guest scratch; generation token checked first on every op.
2. **Activation entry**: modules define `kb_on_activate(ctx)`; the kernel runs it via a shim over the cached VM (the vm's `call_json` only exposes `kb_hot`). Top-level module code must be pure — documented as the M2 module contract.
3. **Activation staging**: `activate_config` removes the module's own publications from the shared registry before validate/publish (otherwise self-conflict), then commits the `composition_changed` event with package + composition refs and state_head = composition digest (closure-valid, R-10).
4. **Generation identity**: u64 counters (consistent with services/capabilities); ModuleId = branded `mod_` Id128 (core BRANDS extended).
5. **Manifest schema 2**: `ExecutionManifest` gains `modules: Vec<ModulePin>` + `composition: Option<Digest>`, `module_abi Some(1)`; schema-versioned extension (no envelope drift; M1 freeze unaffected). engine_digest now populated (guest wasm digest); toolchain_digest still None (documented).
6. **AfterEffectDispatch crash coverage**: the config generation calling its own service is rejected (re-entrant instance lock) before that point; the matrix asserts crashes only where the seam is reached (`crashed=false` row) and the abort path is covered by the session's recorder-injector test.
7. **replace_module rollback** is best-effort (modules' replace is not rollback-atomic; failure leaves the old generation active when it was already displaced — documented).
8. **No process-tree/kill in M2** (no native tools yet — M3); disposal drain is quiesce(no-op) → force with a `cleanup_forced`-shaped record.

## M3 inputs

- Typed tools + effect dispatch (the broker gate becomes the real dispatch path; approval loop), async provenance, interrupted/ambiguous recovery (R-02/C-03).
- Provider gateway (one provider) + model intents/outcomes; per-generation wall-clock budgets already enforced by the vm.
- The pre-registered dogfooding instrument must exist before M3 begins (high-level-architecture.md M3).
- Native process execution (R-28/D-S2 launch controls) — the capability surface exists; the concrete tool set lands at M3.
- Replaceable no-effect policy runtime (host the `PolicyPlugin` trait in the wasm empty-import path — the seam and the guest are both ready).
