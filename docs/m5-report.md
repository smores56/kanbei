# M5 Report — Semantic Workbench

Gate: **green** — 390 tests / 59 suites (~32 s warm), clippy `-D warnings` clean on all 20 crates including the wasm guest.
Commits: `7a772e0` (kanbei-ui), `0b7ccf6` (modules op 7), `3d2b3bd` (session workbench), `c0a27d3`/`f1b4a39` (gate restoration + gate_m2 arms).
Report date: 2026-08-30.

## Scope delivered (architecture.md M5, milestone 5 of high-level-architecture.md)

- **Kernel terminal/fallback boundary** — new `kanbei-ui` crate (kernel API group "terminal safety": init/restore/read_input sanitized/render_snapshot/fallback, R-27).
- **One built-in UI as an immutable module generation** through the standard contribution contract.
- **Composition-failure fallback** — the three R-27 fault classes.
- Acceptance: "UI composition publishes atomically", "terminal restoration/fallback remains reliable", consistency 13 (Hot path).

## Wave 1 — kanbei-ui (kernel-owned boundary)

Pure Rust; **no kanbei-vm/kanbei-modules dependency** (structural consistency-13 guarantee, asserted by `hot_path_structure`).

- `tree.rs` — `SemanticTree`/`Node`/`NodeKind` (root/header/status/list/list_item/text/input/button/placeholder), module wire shape `{"root":...}`, fail-closed parse (unknown kind, bad root, depth ≤32, nodes ≤4096).
- `theme.rs` — named styles (16 colors + attributes), overlay merge (later top-level keys replace, mirroring kanbei-scopes), unknown colors fail closed, default theme.
- `frame.rs` — deterministic layout: banner/header rows, body (list items, wrapped text, status/button nodes), kernel status bar, focused input line with reverse-video caret; viewport keeps the focused line visible; `MIN_ROWS` guard.
- `diff.rs` — cell-level frame diff; `paint_full` (clear+full) and `apply` (changed cells only) emit minimal ANSI SGR.
- `terminal.rs` — `TermiosTerminal` over an owned fd (rustix 1.1 termios): raw mode (`cfmakeraw`), restore, size (`tcgetwinsize`); `TerminalGuard` RAII (restore on any drop path, disarmed explicitly for handoff); `openpty()` for hermetic tests; `TestTerminal` with injected write failures.
- `input.rs` — stateful `InputDecoder`: escape sequences (arrows/home/end/delete/pgup/pgdown/shift-tab), Ctrl keys, bracketed paste, UTF-8 (partial sequences buffered), **sanitization drops all unrecognized C0 controls and invalid bytes**; CR/LF both normalize to Enter; `UiEvent` with kernel-assigned provenance (`User` | `Module(gen)`).
- `focus.rs` — kernel-owned focus model (invariant: focus names a focusable non-disabled node; clamped on tree change; caret in focused input); `KeyClassifier` for the reserved interaction set: Ctrl-C cancel, Ctrl-L repaint, Ctrl-X Ctrl-S safe-mode chord.
- `accessibility.rs` — kernel validation: focusable-without-label, focusable-in-disabled-subtree, control chars; errors are render faults (placeholder+degraded).
- `fallback.rs` — the three fault-class surfaces: `staleness_text` banner overlay, `placeholder_tree` (component fault), `FallbackUi` (kernel fallback UI, module-free).
- `builtin.rs` — the built-in workbench UI Luau source (`BUILTIN_UI_SOURCE`), `kb_on_activate` publishes a UI mount + theme overlay via op 7; `kb_hot` implements `ui_reduce`/`ui_render`.

## Wave 2 — module substrate

- **Host op 7 `contribution_publish`** (kanbei-modules): a generation stages `ui` mounts and `theme` overlays during activation. Recorded per generation in the host (not the live registry — the staged delta is validated + atomically OCC-published by the session). `ui_components` maps component → generation; `drop_generation_contributions` removes both on disposal (displaced mounts unresolvable, R-02/C-03).
- `ModuleManager`: `published_contributions(generation)`, `ui_generation(component)`, `call_generation(generation, args)` (the kernel side of `service_call`, used by the UI host), `ModuleError::Call`.

## Wave 3 — session integration (kanbei-session/src/ui.rs)

- `Session::activate_ui(manifest)` — the standard-contribution UI activation (validate → OCC publish → canonical `composition_changed`), then binds the UI host to the composition's root-scope mount. `activate_builtin_ui` additionally installs the **kernel default policy** (a Builtin-class template allowing `session:append`/`session:cancel` — only when no Builtin template is configured; explicit policy always wins) plus generation-scoped grants for the builtin's generation. Custom UI modules carry no grants → intents denied by default (R-13).
- `UiHost` — bound generation, opaque reducer state, focus/classifier/decoder/theme/last-frame; `degraded`, `staleness`, `safe_mode`, `denied_intents` counters.
- `ui_handle_input(bytes)` — decode+sanitize → reserved keys (cancel run, repaint, safe-mode chord with canonical `safe_mode_activated`) → focus navigation (kernel-side) → text/activate events to the module reducer → intents through the **capability intersection** (`session:append`/`session:cancel`; denied intents are dropped and counted, never canonical) → `SubmitText` commits a canonical **`user_message`** fact + responder trigger (the kernel AppendUserMessage command, first wired here).
- `ui_render_frame` — module render → a11y validation → kernel render → diff. Safe mode and degradation render kernel-authored trees.
- `ui_present(terminal)` — size-aware, diff-only writes; a write failure is a kernel render fault → `FallbackUi` + terminal restoration.
- `activate_config` now stages non-service contributions into the same atomic publish; failures mark the UI stale (banner).
- Fault points `Before/AfterUiReduce`, `Before/AfterUiRender`; `crash_child` m5 mode + `verify_m5_recovery` (reopen, atomic composition intact, UI re-activates, submit commits canonical `user_message`, no `ui_*` gesture events ever).

## R-27 fault classes exercised (gate_m5)

1. **Composition-validation failure** → last-valid composition retained (epoch unchanged) + staleness banner rendered (conflicting re-activation).
2. **Runtime component fault** → kernel placeholder + degraded: host-side render fault (invalid tree) degrades and clears on a later successful render; a guest trap kills the instance (M2 documented) and the module stays degraded (recovery = generation replacement).
3. **Kernel render fault** → kernel fallback UI (pty: raw mode → failing write → fallback frame; terminal restored).

## Acceptance matrix

- UI composition publishes atomically; in-process failure retains last-valid + banner — `composition_failure_retains_last_valid`.
- Terminal restoration/fallback remains reliable — `terminal_restoration_reliable` (pty raw/restore/drop/guard), `kernel_render_fault_falls_back`.
- Crash injection at the UI boundary — 4 points × 2 ack offsets, SIGABRT + `verify_m5_recovery`.
- Consistency 13 (Hot path): `kanbei-ui` has no wasm dependency (structural); render/diff/input are pure Rust.
- Consistency 15 (Scope): UI is in MVP scope per architecture.md; no speculative multi-module composition (deferred R-20).

## Key decisions / gotchas

- **LF normalization**: raw terminals deliver CR for Enter; tests fed LF. The decoder now maps both (and CRLF) to Enter.
- **Broker default-deny (R-13)**: a grant alone is insufficient — `check` applies the union of templates and denies verbs no template mentions. The kernel default policy template for the builtin UI is installed only when no Builtin template exists, so user policy always wins (verified: deny template beats the kernel grant).
- **Guest `error()` traps the instance** (wasm `unreachable`): a Lua error is a hard component fault; the module stays degraded. Host-side parse/validation faults leave the instance alive and recoverable.
- **Empty Lua tables marshal as `{}`** (not `[]`): `intents:{}` must not be parsed as an array (host-side `as_array` fallback).
- **testkit debug cleanup hazard**: `rm -rf` of the tests directory removed the M1–M4 gate suites; restored from git immediately (never delete whole test dirs during debugging).

## Follow-ups (post-MVP, per architecture)

- Multi-module UI slots/reducers/atomic composition (R-20) — the host currently binds the first root-scope mount.
- Deactivation of a UI module mid-session (mount removal from the composition) — currently only restart resets the composition.
- `user_message` payload object promotion above the inline band (M1 inline/object rule) — currently always inline.
- Mouse input, bracket-paste end-to-end, real stdin wiring in a workbench binary (M7 dogfooding).
