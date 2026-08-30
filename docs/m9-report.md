# M9 Report — Deferred-Again Items: No-Effect Policy Runtime, Upcaster Machinery, Second Provider Protocol, Working-Tree Snapshots, Fork/Adopt/Import

Gate: `cargo nextest run --workspace` **535 passed** (2 skipped) / `--all-features` **536** / full suite incl. battery (`--run-ignored all`) **537** — clippy `-D warnings` clean, fresh-linted all changed crates, both feature modes. All work on `main`, one wave per commit: `89037d5` (wave 1), `4b6acb7` (wave 2), `2700090` (wave 3), `8ae4011` (wave 4), `b0cefc3` + `69349d2` (wave 5a/5b). No changes to kanbei-guest, kanbei-vm, kanbei-testkit; kanbei-ui untouched.

## 1. Replaceable no-effect retention policy runtime (wave 1, `89037d5`)

The R-20/R-28/D-S3 deferral: custom retention policies hosted in the same Wasm path with an **empty capability import set** (`architecture.md:378`). Delivered in `crates/kanbei-policy`:

- `WasmPolicyPlugin` (lib.rs:476) implementing the existing `PolicyPlugin` seam (decide/name/is_no_effect): compiles policy Luau through the standard guest ABI (`Vm::compile` + `instantiate`), calls `kb_hot` with a bounded base64-JSON candidate, maps the decision JSON to `RetentionDecision`. Guest failures map to explicit `PolicyError::Plugin` (fail-closed); trapped/limited instances respawn so one fault doesn't wedge the plugin.
- `DenyAllHost` (:404): every `Host::call` returns `Err` — the empty capability import set, enforced by construction; a policy that calls `kb_host_call`/`kb_host_double` traps (test-proven).
- `POLICY_VM_CONFIG` (:430): stock defaults are unusable for policies (1M fuel exhausted by `kb_init`; the epoch deadline is absolute while the watchdog counter grows) — sized from measurements: `fuel_per_call = 2^35` (~4× a max-bound redaction call; infinite loops trip deterministically), 5 s timeout, 64 MiB memory, `MAX_WASM_CONTENT_BYTES` = 700 KiB under the guest's 1 MiB scratch.
- Tests (11): store-all/redaction/drop admissions through the gate, no-effect enforcement, compile errors, determinism, over-bound failure. `kanbei-policy` gains its first kanbei-vm dependency (no cycle; vm depends only on kanbei-core).

## 2. Upcaster framework machinery (wave 2, `4b6acb7`)

Beyond the M1 version field + fn-pointer registry + one fixture (`architecture.md:698,387`): **declarative descriptors interpreted by the kernel** — never module-executed code on the reconstruction path.

- `UpcastDescriptor` in `crates/kanbei-core/src/registry.rs`: closed JSON op set — `add` (absent-only), `set`, `rename {from,to}`, `remove`, `wrap`, `unwrap`, `map {from,to,cases}` (constant case-table; required for the tool_result conditional fixture), plus `require` preconditions; dotted nested paths. Parse is fail-closed (unknown op/field/shape → typed `DescriptorError` naming the offending op/path); `Deserialize` re-validates via `from_value` (fail-closed persistence).
- `Registry` map value is now `UpcastEntry { Fn, Descriptor }`; `register_descriptor` (:88) enforces target schema > source at registration (`RegistryError::InvalidTargetSchema`); `upcast` walks mixed fn+descriptor chains (fn steps +1, descriptors jump to `schema_target()`; gap ends the chain at the last applied step).
- **Precise partial availability** (`architecture.md:387`): descriptors may carry a `package` ref; `note_missing_package` + `missing_package_reason` make a chain with an unavailable package return `Ok(None)` (opaque-but-inspectable, rebuild continues) with the reason naming kind/schema/package; `KindStat.descriptor_package` records provenance. kanbei-projection has an additive hook (:184-200).
- Tests (21): per-op apply, parse rejections, mixed chains, missing-package, determinism, 3-way fixture equivalence as descriptors, serde round-trip.

## 3. Second provider wire protocol (wave 3, `2700090`)

`architecture.md:700` — the provider had one wire protocol (OpenAI-compatible Chat Completions). Delivered an **Anthropic Messages API** engine behind the same `ProviderEngine` trait:

- `AnthropicEngine` (kanbei-provider lib.rs:447): `POST {base}/v1/messages`, `x-api-key` + `anthropic-version: 2023-06-01`; `system` = joined system turns; tool results fold into `tool_use`/`tool_result` blocks (assistant `tool_use` replayed from a new `CompletionRequest.tool_calls` field — `Message` couldn't carry them: testkit has untouchable full literals); `tools` mapped OpenAI→Anthropic shape; `max_tokens` required — req → cfg → 1024 fallback, never null. `parse_anthropic_response` (:609) maps text/tool_use blocks, `stop_reason` → FinishReason, usage; malformed → typed errors.
- Protocol selection: `WireProtocol` enum lives on `SessionConfig` (not `ProviderConfig` — testkit builds `ProviderConfig` with full literals at 7 sites; all 95 `SessionConfig` literals use `..Default::default()`, so the gate passes untouched), default OpenAI; `engine_for` (:696) selects, exercised end-to-end by a session test.
- Tests (12): wire mapping both directions, malformed bodies, selection, backward-compat deserialization. No network in tests.

## 4. Working-tree snapshots (wave 4, `8ae4011`)

`architecture.md:702` — new crate **kanbei-workspace**: content-addressed snapshot of a directory tree into the object store, and restore.

- Manifest (schema 1): `{schema, entries}` bytewise-sorted; tagged entries `file {path, digest, executable}` / `symlink {path, symlink}`; dirs implicit; deterministic (same tree → same digest, dedup-verified). `SnapshotOptions.ignore` = top-level dir names, default `[".git", "target"]`. Unreadable/non-UTF-8/unsupported entries (fifo/socket) fail with path context — never silent.
- Restore: additive/overwrite, never a wipe; temp-write + rename per file, chmod +x, symlink recreation; path guards (no absolute/`..`/empty components, canonicalized parent under canonical root — no symlink-intermediate escape); missing manifest/blob → typed errors with digest+path (partial count included).
- Session commands `snapshot_workspace`/`restore_workspace` (kanbei-session/src/workspace.rs) commit canonical `workspace_snapshot`/`workspace_restore` events with the manifest digest as ref; restore failure commits nothing.
- Tests (18): round-trips, determinism, ignores, overwrite semantics, escape rejection, missing objects, symlinks.

## 5. Independent-session fork/adopt/import + workspace restoration (wave 5, `b0cefc3` + `69349d2`)

`architecture.md:338,394-401,212,701`. In-session branching (M6 `continue_from`) existed; independent-session fork did not.

**5a — `Session::fork(&self, checkpoint, ForkOptions)`** (lib.rs:1671): pure snapshot read, source untouched (no quiesce, no source events).
- Validation shared with continue_from via extracted `validate_checkpoint` (:1460) + `CheckpointFacts` (:436) — continue_from behavior byte-identical (54 pre-existing tests green).
- Seeds the new session by file-copy: checkpoint closure objects (engine/toolchain digests excluded, matching continue_from), memory scope dirs spliced to the checkpoint-pinned root (`truncate_log_at` :3095 — post-checkpoint source transitions never leak; replay yields exactly the pinned head), projects.jsonl, config manifest from the last `config_choice`/`composition_changed` at/before the checkpoint (`config_choice_at` :1886; unreadable → documented storage-only fallback). `projection.sqlite` disposable, `state/` opaque — both skipped, documented.
- **Fork floor** (R-24/D-08, `architecture.md:212`): `fork_floor_broker` (:3040) — read-only allow list (fs.read/fs.search/git.status/git.diff/memory.query) + `memory.propose` approval-required; 6 session-scoped grants, digests derived and recorded.
- Canonical `forked` event: `{source_session, checkpoint_seq, checkpoint_snapshot, follow, grants, config, frontier_seq}` — the explicit source reference (`architecture.md:338`) and the attenuated-grant fact. Workspace manifests+blobs (event-referenced, outside the ExecutionManifest closure) are copied and ref'd so `restore_workspace` resolves and stays GC-rooted.
- Tests (9): fresh SessionId, memory-root preservation incl. post-checkpoint truncation + project scope, read-only broker floor, config same-digest activation, source untouched, invalid checkpoints (no orphan dir), double-fork determinism, workspace restore.

**5b — `Session::adopt` + `Session::import`** (lib.rs:1926, 2120):
- `adopt(&mut self, fork, label)`: validates the fork's `forked` fact (`source_session` match, head beyond the forked event), get-hash-verifies every byte in the fork before installing anything, quiesces self's active run (faithful replication of continue_from's block), commits canonical `fork_adopted` `{fork_session, fork_seq, fork_snapshot, follow, label, quiesce, frontier_seq}` with refs = [snapshot, fork roots], sets `pinned_roots` to the fork's pins. Fork never modified.
- `import(source_dir, target_dir)`: byte-copies log.zst + objects/ + memory/ + state/ preserving IDs (session id recovered from canonical identity markers — memory proposal owner, transition origin, project registry `created_session` — since the id is not on-disk; marker-less dirs get a fresh id, documented); projection.sqlite rebuilt. Imported session opens, commits, reopens (tested twice).
- Tests (8): adopt happy path (fact fields, refs, pinned_roots, fork untouched + still writable), active-run quiesce (+2 events, `Failed(Quiesced)`, no intent_classified), wrong-source/outcome-less/missing-object rejections (nothing committed), FollowHead, import round-trip (identical session_id, identical `Vec<Envelope>` — event ids/seqs/payloads/refs/snapshots, identical branch records + memory head), import validation.

## 6. Engineering notes

- Deferred to M10: **remote clients/A2A scheduling** (`architecture.md:709`) — needs daemon/background operation (itself deferred); **VM/heap snapshot persistence** remains EXCLUDED — constitutionally rejected as canonical module state (`architecture.md:537`); lifetime automatic promotion unchanged (prohibited without a recorded root-approval fact).
- M9 added one new crate (kanbei-workspace), one new cross-crate dependency (kanbei-policy → kanbei-vm), no new external crates. Workspace: 20 members, 535 tests default / 536 all-features / 537 full.
- `clippy --workspace --all-targets -D warnings` has pre-existing findings in untouched files (session tests/common, gc, ui_multi; scopes lib; workspace tests); the standing gate (`--workspace -D warnings` fresh-linted) is clean — noted for a future lint-fix sweep, not this milestone.
- Feature-gate note: `WireProtocol` on SessionConfig avoids breaking testkit's 7 full-literal `ProviderConfig` constructions; `CompletionRequest.tool_calls` is a `#[serde(default)]` additive field.
