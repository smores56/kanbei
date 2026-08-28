# Agent Harness Architecture

Status: active design record
Last updated: 2026-08-28
Revised: 2026-08-28 — applied design-review reconciliation packet (R-01..R-29); see review-reconciliation.md.

## Problem

Design a greenfield, local-first agent harness for expert developers with:

- a performant Rust core;
- perpetual Headlong-style cognition;
- Cordis-style live extension lifecycle;
- Maki-style strong primitives exposed to configuration;
- capability-scoped agent-authored code;
- fully programmable semantic UI, keymaps, themes, tools, providers, cognition, and context projection;
- append-only, inspectable history;
- graph-based memory;
- single-binary distribution and one long-lived harness process.

Primary differentiator: trustworthy live extensibility. Agent-authored code is expected to become useful and reliable, but remains permanently capability-scoped to contain mistakes, confused authority, and lifecycle defects.

## Sources studied

### Headlong

- https://github.com/laude-institute/headlong
- https://github.com/laude-institute/headlong/blob/main/philosophy.md

Transfer:

- append-only typed trajectories;
- deterministic context as a projection of immutable history;
- causal run/trigger/fork/merge links;
- one bounded cognitive step per wake;
- separate responder and autonomous cognition paths;
- persistent backoff, liveness, queueing, and explicit idle/error outcomes;
- summaries as derived records with raw evidence retained.

Do not transfer:

- unrestricted Bash as the internal protocol;
- host fallback when sandboxing fails;
- silent queue loss;
- unbounded recursion/spend;
- environment variables as hidden APIs;
- raw generated chain-of-thought as a correctness dependency.

### DeepSeek Harness and Cordis

- https://github.com/deepseek-ai/deepseek-harness
- https://github.com/cordiverse/cordis

Transfer:

- plugins as lifecycle-owned fibers/scopes;
- dependency-gated activation and provider generations;
- every registration, task, listener, child, timer, and handle has one owner;
- async disposal reaches quiescence;
- immutable package identity separated from activation generation;
- stale-generation rejection;
- domain-owned service, registry, event, policy, workflow, and UI seams;
- monotonic security guards separated from cooperative middleware;
- staged replacement and cleanup.

DeepSeek does not define a universal cognitive-act registry. Plugins imperatively register on domain-specific seams. Ordinary features should not patch the concrete loop. The useful synthesis is domain-specific contracts plus one narrow replaceable cognition-step service.

### Maki

- https://github.com/tontinton/maki
- Personal design context inspected on `smores@smortress:~/dev/maki-plans/`, especially event-driven architecture, modular agent core, conversation tree, Lua providers, and typed actions.

Transfer:

- Rust owns protocol hot paths and hard invariants;
- typed tools and provider abstraction;
- strong primitives composed by config;
- deny-safe permissions;
- event-driven blocking rather than polling;
- one source of truth per fact;
- immutable snapshots on UI hot paths;
- non-destructive conversation branching;
- context as a pure fold;
- ADTs, branded IDs, smart constructors, and illegal states made unrepresentable;
- storage split by audience rather than a simplistic truth/cache distinction.

Greenfield is required. Maki moves toward the desired design but inherits global registries, one large Lua integration runtime, imperative UI ownership, and non-transactional reload boundaries.

## Locked architecture decisions

### Product and core

- Primary user: expert local developers.
- Greenfield Rust implementation.
- Semantic center: typed Rust FSMs make decisions; canonical events record facts.
- Events are not an untyped callback mesh.
- Hard invariants remain in Rust; modules influence explicit typed boundaries.

### Perpetual cognition

- Perpetual Headlong-style cognition is a first-class experiment, not merely reactive task execution.
- Cognition is a separately supervised service from the interactive responder.
- One bounded cognition step runs per accepted wake.
- A generic replaceable `CognitionProvider::step(context, trigger)` seam is used initially.
- The step proposes model, tool, memory, child, and scheduling operations through domain-specific typed seams.
- Kernel dispatch does not hard-code or expose a universal `think/learn/act` taxonomy.
- Optional labels such as reflect/plan/learn are analysis metadata only.
- Terminal outcomes: Progress, NoProgress, Waiting, Blocked, Failed, CompletedGoal.
- Cognition-step execution model (R-18/E-01): `step(context, trigger)` is a bounded orchestration body over a closed set of typed host commands (`model_call`, `tool_intent`, `memory_query`, `memory_propose`, `child_spawn`, `schedule_wake`), each committing through its owning FSM; the kernel checks wake deadline/budget at each host-command boundary; deadline expiry ⇒ run outcome `Failed(Deadline)`; committed intents remain canonical facts; the context parameter is a frozen immutable projection.
- Children are bounded by default. Persistent children require an explicit spawn primitive, capability, lifecycle, and budget.
- Child spawn routes through the tool intent pipeline: `child_spawn` is proposed and committed as a tool intent through the tool FSM, not a separate spawn path (R-09).

### Scheduler

- Luau policy proposes triggers, priority, coalescing, backoff, expected utility, cognitive step selection, and next wake.
- Rust enforces pause/shutdown, cancellation, deadlines, global/per-run concurrency, token/cost/tool/time budgets, stale-generation rejection, bounded queues/timers, responder priority, and circuit breakers.
- Canonical scheduler surface (R-09/E-09): canonical records are the wake-acceptance decision (coalesced triggers referenced as digest lists inside it), denials/circuit-breaks with the responsible constraint, run start, and terminal outcome; raw observed triggers and expected-utility scores are policy-private module state or ephemeral.
- Wake = Run (R-09/E-10): every accepted wake creates exactly one RunId with a kind discriminator (`CognitionStep | ResponderTurn | Child`) and typed trigger provenance; wake/outcome pairing is the run FSM lifecycle.
- Responder priority: responder commands never queue behind cognition at the session actor; an in-flight cognition model call may be cancelled at the stream boundary, classifying the run `Failed(UserCancelled)`; committed intents are never rolled back.
- Circuit breakers (R-17/E-02): kernel-owned breakers on canonical counters — consecutive `Failed`; consecutive `NoProgress`/`Waiting` without new causal events; N identical action digests within a window; spend per wall-clock window; kernel enforces minimum floors, policy tunes only above floors; a trip appends canonical `BreakerTripped` (responsible counter) and pauses cognition until explicit user resume.
- Reactive-only scheduling remains possible as a policy even though perpetual cognition is the experiment.

### History and context projection

- All observable execution facts have append-only durable history, with secrets represented by receipts/references rather than copied values.
- Raw latent chain-of-thought is not assumed available or authoritative.
- Provider-native reasoning artifacts remain opaque and separately classified where continuation requires them.
- The cognitive record schema remains experimentally configurable.
- Context projection is a typed staged pipeline, not one fixed algorithm or arbitrary unvalidated provider-request callback.
- Conceptual stages:
  - TrajectoryView
  - BranchSelection
  - CognitiveSelection
  - RetrievedEvidence
  - CompressedContext
  - BudgetedContext
  - ValidProviderContext
- Config may replace stages where types permit.
- Final Rust validation enforces provider validity, token limits, tool/result pairing, artifact access, and opaque-reasoning rules.
- Summarization is an explicit model effect whose frozen output becomes a later pure projection input.
- Every model invocation records projection/module hashes, selected event IDs/ranges, rendered-context hash, model/provider parameters, observable result, and cache plan/outcome.
- Context projection is cache-aware: stable valid material is placed at the front when semantics and provider protocols permit.
- Conceptual order is stable harness contract, deterministic module/tool schemas, stable project/lifetime memory, stable conversation prefix/compaction, volatile active memory/recent events, then the current trigger.
- Projection fragments declare semantic ordering, stability class (`Static`, `ScopeStable`, `SessionStable`, `TurnVolatile`), content/dependency hashes, sensitivity, and cache eligibility.
- Provider lowering computes the longest legal stable prefix without semantically reordering messages.
- Tool schemas use deterministic ordering/canonical serialization. Timestamps, random IDs, counters, and volatile status stay out of stable prefixes.
- Provider cache controls and opaque-reasoning placement are negotiated capabilities, not assumed common behavior.
- Kernel-owned mandatory authority filter applies to fragment sources before any replaceable stage, using the run's trajectory/memory read capability; replaceable stages may only narrow (filter, summarize, drop), never add sources; the final validator rejects any fragment whose source references fail the run's read capability (R-05/E-03).
- Sensitivity non-escalation: an output fragment's sensitivity must be ≥ the max sensitivity of its input fragments; lowering sensitivity without an explicit typed transform is a validation error (R-05/E-14).
- Suppression ban: lowering must include all currently selected evidence — the cache must never suppress, defer, or stale-select a fragment; promotion-driven stable-segment invalidation is accepted and recorded in the cache outcome (R-05/E-05).
- Chronology check: any fragment carrying canonical conversation events beyond the current frozen prefix must be classified volatile; fragments referencing events carry event-range references the kernel checks against chronology (R-05/A-06).
- Opaque reasoning (R-18/E-07): provider manifests declare artifact replay/transfer rules (transferability default: none); the kernel validator strips untransferable artifacts at lowering; outcome events carry `ReasoningContinuity { Continuous | Broken(from_provider, at_event) }` — mandatory `Broken` on the first call after a provider change.
- Replay eligibility (R-04/E-08): outcome events carry `ReplayEligibility { Replayable | NonReplayable(reason) }`; `continue_from` over a frontier containing NonReplayable items is rejected by default; explicit user override appends a `ContinuityBoundary` event and the projector substitutes an explicit tombstone fragment.
- Compaction (R-18/E-06): operates only on causal-closed prefixes (all descendant events committed); the compaction selection is itself a canonical event referencing the covered range and the summary object; the session FSM rejects new events whose causal parents fall inside a compacted range.
- Intent provenance (R-08/E-13): the model-intent event carries the planned fragment-list digest plus cache plan; the outcome event repeats the rendered digest; the session actor validates equality at outcome commit.

### Language and execution model

- Luau is the coherent user-facing authoring language for configuration, hooks, projection stages, cognition policy, commands, tools, keymaps, themes, and semantic UI.
- Agent multi-tool code mode is Luau calling typed capability tools.
- No ambient `io`, `os.execute`, unrestricted require, sockets, environment, native modules, or arbitrary executables.
- Generated typed Luau stubs expose available tools and capabilities.
- Pipeline/composition helpers may provide shell-like ergonomics.
- A dedicated capability-shell syntax is deferred; it could later lower to the same typed effect IR.
- Wasm Components/WIT remain the portable plugin ABI.
- Native subprocesses cover existing developer tools and CLIs.

### One-process containment

- One long-lived harness OS process.
- Kernel tiers (R-19): (1) enforcement kernel — mechanisms and invariants only; it never depends on tier 2 for an invariant, and safe fallback UI stays tier 1; (2) native built-in services — Rust implementations of the same typed module service contracts (retrieval mechanics, render diffing, provider gateway mechanics, terminal renderer, domain projection ops behind the kernel write-gate); (3) Wasm-Luau module generations.
- Agent-authored Luau generations run as Luaur inside separate Wasmtime instances off the main thread.
- Wasm linear memory, traps, fuel/epoch interruption, deadlines, and separate stores are the intended fault boundary.
- Configured store limits (memory, table, instance count) per generation, epoch deadlines plus host-side timeout wrappers around every host import, and a per-generation wall-clock budget are required (R-24/D-10).
- This protects against guest faults represented as Wasm traps, not Wasmtime, unsafe host, process-wide allocator, or kernel faults.
- Canonical state is Rust-owned; Luau heaps, closures, coroutines, userdata, and capability proxies are disposable.
- Native-Luau trusted/low-latency tier is deferred post-MVP; MVP runs all Luau (config compilation included) in Wasm-hosted Luaur with one host ABI, and built-in defaults are native Rust implementing the same service contracts (R-19).
- Luaur is pure Rust and packages well, but is research-grade, includes substantial unsafe code, and is not itself crash isolation when run natively.

### Native tools and outer sandbox

- The harness does not impose an OS sandbox around every native subprocess.
- Users may sandbox the complete harness process tree when machine-level containment is desired.
- The capability broker still controls which module may request each host effect.
- Native launch controls: timeout, output limits, inherited-FD closure, process-tree cancellation, approvals/metadata, and default-deny environment are load-bearing and enforced; executable/argument/cwd/env constraint policies are recorded hygiene metadata in MVP, not isolation claims (R-28/D-S2).
- A permitted shell/compiler may exercise the aggregate authority available inside the user’s outer sandbox.
- Native execution is an explicit broad registered capability, never ambient language behavior.

### Capability model

Effective authority is the intersection of:

- module-declared requirements;
- user/workspace policy templates;
- spawning principal delegation;
- current session/run budgets.

- Caller principal (R-14/D-02): every invocation carries the initiating principal (session/run/generation); effective authority for a tool effect = caller ∩ tool-provider module ∩ policy ∩ budget; approvals attribute to the human-consent principal.

Properties:

- delegation is attenuation-only;
- grants are scoped by principal, module generation, resource, verbs, expiry, budget, and purpose where relevant;
- a model/module cannot widen its own authority;
- policy guards are monotonic and cannot be overridden later;
- exact unmatched consequential operations can require approval;
- the outer sandbox defines aggregate machine authority, not plugin-to-plugin authority.
- Grant establishment (R-13/D-01): consequential capabilities can only be granted by a user-authored/user-approved policy decision bound to (origin trust class, ProjectId, package content digest, capability set, purpose), recorded as a canonical approval fact; digest changes re-prompt; default-deny for workspace- and agent-origin modules; built-ins and explicitly user-installed user-level modules may auto-grant; policy-type modules (retention, scheduler, context projection) require the same consent on replacement.
- Policy templates are keyed by origin trust class (R-13/D-04).
- Dispatch-time re-verification and revocation (R-16/D-11/C-10): at dispatch, the kernel re-derives the digest from the committed intent, re-runs guards, and verifies policy/grant versions unchanged; approval attaches to the committed intent and execution re-checks the current composition's capability intersection — revoked ⇒ intent resolves `interrupted` with a user-visible reason; re-approval is a new intent.
- Approval digest (R-16/D-12): binds tool ModuleId+generation, action type, canonicalized arguments, and (for process tools) cwd/env fingerprint, domain-separated from object digests; approvals carry explicit scope (run/session/project) and expiry; standing approvals without scope are prohibited.
- Fork floor (R-24/D-08): `fork(checkpoint)` starts with read-only capabilities plus memory-propose; consequential effects require the standard approval path; the attenuated grant is recorded as a canonical fact.
- Credential custody (R-28/D-06): kernel-held via OS keychain, injected into provider requests at call time only; default-deny subprocess environment allowlist; credentials never enter canonical records, snapshots, or objects.
- Privacy sinks extend to provider egress, crash reports, and diagnostics (R-15/D-03/D-05): each model call records a canonical egress entry (provider identity, sensitivity classes egressed, origin snapshot); policy templates may restrict egress per provider/sensitivity class; kernel invariant: diagnostics carry digests/lengths/counts, never raw candidate/provider bytes.

### Unified module lifecycle

- Built-ins, user config, workspace config, themes, providers, projection policies, tools, and agent-authored extensions all use one immutable module-generation model.
- There is no privileged base-config lifecycle.
- Metadata distinguishes origin, trust class, scope, precedence, persistence, dependencies, requested capabilities, and whether disabling is allowed.
- Stable ModuleId, immutable content/package hash, and ephemeral GenerationId are separate identities.
- The active epoch is the derived immutable composition of active generations; EpochId = digest of the execution-snapshot manifest's composition section — "active epoch" is that digest, not a second concept (R-01).
- Registry conflict rules (R-19/A-11/C): kernel-owned mechanisms carry fixed typed conflict rules per structural contribution type (tools, services, UI slots, projection-stage slots, keymap tables, themes); modules contribute typed entries, never resolution logic; one kernel staging/validation/publish protocol is shared by all domain registries.
- Contribution types retain domain-specific conflict rules rather than one universal priority:
  - keymaps: layered match;
  - themes: validated overlay;
  - commands/tools: unique or explicit replacement;
  - projection stages: named slots and ordering constraints;
  - services: one provider per scoped key;
  - UI: named mount points;
  - guards: monotonic.
- Activation canonicality (R-01/C-01): activation is canonically recorded only when a session observes it — mid-session reloads and scope publishes append one typed `composition_changed` event through the session actor (pre-event snapshot = old manifest; payload = epoch delta: added/removed generations, scope, initiator principal). Startup/root-scope activation is non-canonical: rebuilt from desired state (config) at restart; on root-scope validation failure, kernel safe mode activates only kernel-shipped built-in generations with an actionable error. No global module log; UI composition is a pure derivation of the composition epoch (no second commit point). Canonical order: install package objects → install snapshot manifest → commit event → CAS-publish epoch → drain displaced generation.
- Disposal drain (R-24/C-04): quiesce cancellable effects → bounded deadline (default 5 s) → force-terminate (drop Wasm store, kill process trees) → commit canonical `cleanup_forced` fact → finish disposal; no per-resource policy variants in MVP.
- Service replacement policy (R-25/C-05): service contracts carry a version; dependents declare a required version; on provider replacement, version-compatible dependents rebind atomically and version-incompatible dependents restart inside the same transaction; "pinned" is prohibited (it creates owner-less live effects); reject is subsumed (a dependent that cannot restart fails the transaction).
- Service keys are `ScopePath/Name`, namespaced by owning module (R-25/C-06): publication requires the key free or an explicit `replace` intent validated against capability/precedence; the typed conflict error names holder, challenger, and key; dependencies may point only to same-scope or ancestor-scope services; parent→child dependencies are rejected in MVP.
- State migration is fail-closed (R-07/C-07): activation validates the incoming generation's declared state schema against the existing head; incompatible ⇒ activation rejected atomically with a typed error and the old head untouched; an explicit user `module reset-state` starts a fresh head and records reinitialization as a canonical fact; no migration transforms in MVP.

### Kernel API inventory (working contract from design review)

Twelve groups (R-19; accepted as the working kernel contract):

- identity/codec;
- session-actor typed commands: CreateSession, AppendUserMessage, CommitModelIntent/Outcome, CommitToolIntent/Outcome, CommitCognitionOutcome, CommitMemoryTransitionRef, CommitModuleActivation, Branch/Fork/AdoptFork, Pause/Resume, Shutdown;
- `AppendLog<T>`: open/append/flush/verify/recover/iterate/segment;
- object store: install/verify/read/exists;
- execution snapshots: materialize/verify_closure;
- module lifecycle: install_package, activate, dispose, resolve_service, generation-token check on every host import, fuel/deadline/cancellation;
- capability broker: check/attenuate/require_approval (principal resolution internal, not a seam);
- retention gate: no-effect admission, ordering, bounded candidates, kernel-owned replay bit;
- scheduler bounds: admit/budgets/deadlines/concurrency/breaker floors;
- terminal safety: init/restore/read_input (sanitized)/render_snapshot/fallback;
- projection framework: write-gating, watermarks, rebuild orchestration/verification;
- upcasting: declarative/pinned-pure, no effects.

### Dynamic registration

- Modules use Cordis-like imperative registration APIs, but structural updates occur inside atomic transactions.
- Initial activation is a root scope transaction.
- MVP scopes are children of the root only (single level), always ephemeral, created with an explicit owner lease (generation or run), name-unique within parent, and staged via OCC against the current epoch (R-26/C-09).
- A child scope stages, validates, and atomically publishes a coherent contribution set.
- Parent disposal recursively disposes child scopes.
- Generation replacement rejects stale/duplicate/cancelled/invalid effect publication; outcomes of already-dispatched host work are always committed as facts (origin generation + commit snapshot), classified interrupted/ambiguous when the origin generation is stale (R-02/C-03).
- Ephemeral scopes vanish on restart.
- Durable desired scopes require a canonical domain fact and are reconstructed from desired state (post-MVP design; durable desired scopes and nested scopes are MVP non-goals — R-26).
- Ownership solves cleanup; transactions solve partial visibility and grouped conflict validation.

### Module state

- Module/runtime heaps are not canonical state.
- Host-owned typed state is exposed through familiar SolidJS-signal/React-state-style handles.
- Example:

```lua
local state = ctx.state.session({
  id = "planner",
  version = 1,
  default = { attempts = 0 },
})

state:update(function(s)
  s.attempts += 1
end)
```

- Updates operate on a copy, validate, and commit atomically.
- Accepted state is stored as an immutable typed content-addressed snapshot plus an atomically replaced scope/module head pointer.
- Consequential session events and model-call records pin the exact state digests that influenced behavior. Private updates never referenced by canonical behavior need not remain forever.
- Module-state head pointers are canonical current state, not merely caches. Canonical events that depend on state pin the relevant immutable snapshot digest for historical audit.
- Private updates that never influenced a canonical event remain durable current state but need not have permanent mutation history; losing a corrupt/unbacked head may lose the latest unpinned private state.
- Head contract (R-07/B-01/F2): location `sessions/<SessionId>/state/<StateKey>.head`; format = digest + schema version + checksum + last-pinned snapshot digest + sequence; snapshot objects live in the per-session object store; head replacement = temp+fsync+rename+dirsync, performed only by the session actor (or a kernel state-store actor it owns); the kernel checks the generation token on every head CAS — displaced generations' updates are rejected and recorded as rejected stale effects; MVP supports session-scoped state only; a corrupt head ⇒ refuse silent substitution, restore the newest canonically pinned snapshot, emit an explicit "unbacked private state lost" user-visible fact, and reconstruction reports it like any partial-audit source.
- Rust initially enforces maximum encoded current state and maximum snapshot object size. Oversized updates throw atomically and leave the old head active.
- Unreachable snapshots are GC-eligible only after canonical-reference analysis and a configurable grace period from last reference.
- MVP `prune-unpinned-state` (R-08/B-02): user-invoked (optionally automatic with a large default grace); deletes private state-snapshot objects referenced by neither the current head nor any execution-snapshot manifest pinned by a committed event; purely local; unpinned private history is explicitly non-guaranteed.
- Deltas, collection-specific limits, rate quotas, and VM/heap snapshots remain deferred or rejected.
- One immutable content-addressed execution-snapshot manifest pins active module packages/generations/scopes, module-state heads, project/lifetime memory roots, tool-registry snapshot, context projection, cognition scheduler/provider, retention policy, model/provider capabilities, and capability-policy versions. The manifest also carries kernel/bootstrap-schema version, module-ABI version, event-envelope schema version, and engine/toolchain (Wasmtime+Luaur) digests (R-08/A-07/E-12); reconstruction validates against the pinned versions; cross-engine re-derivation is marked unverifiable per-fragment, never failed.
- Snapshot manifests are acyclic digest/ID structures, never mutable-head references and never self-referential. A genesis event uses an explicit kernel bootstrap snapshot. Required manifest closure installs before event commit.
- Every canonical event references its pre-event commit-snapshot digest. State-changing events describe transitions; subsequent events reference the resulting snapshot. Content addressing deduplicates unchanged manifests.
- Manifests materialize only at event commit, never per private update; intermediate heads are simply superseded (R-08).
- Pinning granularity (P9 clarified, R-08): manifests pin at state-changing transitions, run/branch genesis, and authority/policy changes; pure events reference the last-pinned manifest.
- Async intents establish an `origin_snapshot`; completion/outcome events reference both origin and current commit snapshots. Origin explains the implementation/state/authority that produced work; commit snapshot explains the FSM/policy environment that accepted or classified it.
- Export and verification traverse the execution-snapshot closure into retained packages, state objects, memory roots, tool schemas, and policy descriptors. Missing closure items make audit explicitly partial/unavailable.
- Reconstruction qualifier (R-12/M-11): effect-requiring projections (dense-vector rebuilds) are excluded from reconstruction validity — re-embedding is explicit and asynchronous; audit validity never requires them.
- Context re-derivation is two-tier honest (R-05/E): selection provenance (recorded event/claim IDs, digests, plan) is always reconstructable; byte-level re-derivation requires retained packages and the pinned engine and is best-effort with explicit unverifiable marks — never silently repaired.

### UI

- UI is a semantic component tree with module reducers and typed intents.
- Modules may replace the complete visible layout and interaction map.
- Rust owns terminal initialization/restoration, input decoding/sanitization, focus/modal invariants, render diffing, accessibility validation, effect dispatch, and crash fallback UI.
- Conceptual model:

```text
UiState + UiEvent -> UiState + [DomainIntent]
UiState -> SemanticTree
SemanticTree + Theme -> TerminalFrame
```

- Hot paths consume immutable Rust snapshots; Luau/Wasm does not draw terminal cells directly.
- No UI events are canonical.
- Persist normalized domain intents/facts, not gestures such as key presses, focus, scroll, hover, animation, or modal closure.
- Drafts and presentation state may live in memory or disposable SQLite.
- UI reducer state is always ephemeral; durable drafts persist only via module-state heads (R-27).
- Three fault classes (R-27): composition-validation failure → last-valid/core UI with a staleness banner; runtime component fault → component-level placeholder with the module marked degraded; kernel render fault → kernel fallback UI.
- The kernel assigns input provenance on every `UiEvent` (`User` or `Module(gen)`); module-emitted intents are subject to the standard capability intersection (R-27).
- The kernel reserves a minimal interaction set (focus/modal escape, core navigation, safe-mode entry) that modules cannot rebind (R-27).

### Identity model

- Occurrence/logical identities use distinct Rust newtypes wrapping one common validated UUIDv7 value type.
- The common `Id128` owns UUIDv7 validation, canonical Base58 encoding/decoding, Serde behavior, compact 16-byte database/binary representation, hashing, and diagnostic timestamp extraction.
- Domain wrappers include `SessionId`, `BranchId`, `RunId`, `EventId`, `ToolCallId`, `ClaimId`, `ModuleId`, and `GenerationId`. Message identity is its committing event — `MessageId` is dropped as a separate branded identity (R-19/A-S4); `ToolCallId` remains for calls spanning intent/outcome records.
- Domain wrappers prevent cross-type use at compile time and own their human/wire-text affixes.
- Text forms should use branded prefixes such as `ses_`, `br_`, `run_`, `evt_`, `call_`, `clm_`, `mod_`, and `gen_`; parsers reject the wrong prefix even when payload bytes are valid.
- SQLite and typed binary protocols store the 16-byte payload without redundant text affixes; schema/column type provides the domain.
- UUID payloads are never intentionally reused across branded types.
- Session, genesis event, initial branch, and first message have independent IDs joined by explicit typed references.
- Session sequence and explicit causal edges are authoritative. Base58 order and UUIDv7 timestamps are never used for replay, authorization, causality, or freshness.
- Backup/import of the same fact preserves IDs. An independent session fork gets a fresh SessionId and explicit source reference; an in-session branch keeps SessionId and gets a fresh BranchId.
- `SessionId` identifies the storage/lifecycle container; `BranchId` identifies a causal conversation future; `RunId` identifies one bounded supervised execution lifecycle.
- Immutable byte/package identities use versioned content digests rather than UUIDv7; object file names carry an algorithm version prefix (`<alg>:<digest>`) and a hard per-object size quota applies (R-22).
- `ProjectId` is another branded UUIDv7 newtype used for XDG project memory identity.
- Project identity is stable and generated, while Git common-dir identity, normalized remotes, observed roots/worktrees, repository fingerprints, and user aliases are locators/evidence rather than identity.
- Moves and remote changes add locator observations. Ambiguous clone/fork matches never silently merge; explicit link/split operations preserve provenance.
- A small canonical `$XDG_STATE_HOME/<harness>/projects/events.jsonl.zst` stream records ProjectId creation, locator observations, aliases, confirmed links, splits, and deprecations. SQLite projects current matching state. Writes are rare, so its single writer is acceptable and it is not a session catalog.
- Exact Base58 width compatibility with the existing Maki ID implementation: ratified at kernel review before M1 spec freeze; no format drift after first events (R-22/H-07).

### Canonical append-log format

Every canonical append-only log uses one shared Rust `AppendLog<T>` protocol built from concatenated independent Zstandard frames containing complete JSONL records. This includes session trajectories, project/lifetime memory root-transition streams, the project locator registry, and any future canonical module/scope/policy streams.

The shared implementation owns framing, sequence assignment, checksums, durability profiles, schema versions, recovery, and SQLite projection watermarks. Projection watermarks commit in the same SQLite transaction as the rows they cover; rebuild ignores watermarks (R-23). Separate subsystems must not invent ad hoc canonical log formats.

Exceptions are not append logs: immutable content-addressed artifacts retain native bytes with media-appropriate compression; SQLite remains an ordinary disposable database; mutable UX caches/config source files use natural formats; installed modules/Wasm are immutable packages.

### Canonical session storage

- JSONL is canonical truth; SQLite is disposable and recreatable.
- One logical ordered event stream per session. V1 may physically use `events.jsonl.zst`, but one-file layout is a replaceable `AppendLog<T>` backend detail rather than a permanent invariant.
- Future immutable physical segments may improve corruption isolation, backup, parallel rebuild, retention, and synchronization without changing event/DAG semantics; V1 segment rollover triggers on frame-count and byte thresholds (R-23).
- Callers depend on event IDs, session sequence positions, and committed batches — never paths, offsets, or frame digests; frame metadata is internal to AppendLog and valid only within a physical epoch (R-23).
- Use concatenated independent ordinary Zstandard frames, not the seekable format.
- Frame layout (R-23/F7): each frame is one typed metadata record followed by its event records; an event never spans frames. Frame metadata is excluded from session sequence and from `zstdcat` event output — `zstdcat` yields ordinary canonical JSONL events.
- Frames include stream/schema identity, first/last sequence, event count, previous-frame digest, and a digest over canonical records plus frame metadata. Zstd content checksums detect frame damage; the local hash chain detects missing, reordered, or substituted frames within the observed chain.
- The chain does not prove that a final suffix was not removed without an external trusted head. Signatures and Merkle manifests are deferred.
- One Rust writer assigns authoritative session sequence numbers.
- Prior frames are never rewritten.
- `zstdcat` yields ordinary JSONL.
- A truncated final frame is discarded/truncated during recovery; mid-file corruption is an explicit error.
- Large bytes live in a per-session immutable content-addressed object store referenced by events.
- Storage uses a hybrid inline threshold: small messages/results/state remain inline and grep-friendly; large logical payloads become typed object references. Inline versus object is physical representation, not event meaning.
- Object references include a domain-separated logical-byte digest, logical length, media/schema type, physical encoding, and required trust/audience/sensitivity metadata.
- Hashes cover canonical logical bytes, not compressed bytes. Text/JSON objects may use Zstd; already-compressed media retains an appropriate native encoding.
- Object installation precedes event commit and uses the required protocol (R-10/B-03): validate and hash logical bytes, temp write, fsync temp, atomically rename to the digest path, fsync the parent directory (dirsync); a referencing event may commit only after the referenced object's rename is dirsync-durable. Crashes may create harmless orphan objects but cannot create a newly committed dangling reference.
- Missing or hash-invalid required objects are explicit canonical corruption/unavailability, never silently replaced from SQLite or regenerated from mutable renderers.
- Avoid separate canonical message/tool/state sidecar logs; they would reintroduce cross-log atomicity and recovery ambiguity.
- Tools emit named output candidates with extensible role, audience, sensitivity, replay requirement, media/schema type, provenance, and bounded content.
- Replaceable classification/redaction/retention plugins decide `Store`, `Transform`, `Drop`, or `RejectExecution` before persistence; `ExternalReceipt` is removed from the MVP decision set until an external store exists (R-28/D-S1). Defaults are ordinary replaceable built-in modules; Rust contains no mandatory secret-classification algorithm.
- These plugins run in a kernel-enforced no-effect policy runtime: bounded candidate access and pure transforms, with no network, native process, model, arbitrary filesystem, memory-write, tool-call, or installation capabilities; the runtime is the same Wasm hosting path with an empty capability import set, not a second bespoke interpreter (R-28/D-S3).
- Rust hard invariants: policy runs before harness-controlled logs/objects/SQLite/telemetry receive bytes; size/decode/backpressure limits cannot be bypassed; transformations and policy generation are canonical facts; dropped model-influential content makes the boundary explicitly non-resumable/non-replayable or rejects execution.
- The kernel guarantees policy ordering/authority restriction, not classifier correctness. Native tool side effects inside the user's outer sandbox are outside output-retention guarantees.
- Retention replay bit (R-04/A-04): the kernel owns a conservative replay-relevance default — candidates whose role can enter model context are replay-relevant unless the kernel-validated tool manifest explicitly declares otherwise; policy may only narrow retention upward and never writes the bit; `Drop` on a replay-relevant candidate forces an explicit non-resumable boundary or `RejectExecution`.
- Retention failure semantics (R-04/D-07): policy-runtime trap/timeout/limit ⇒ fail-closed (`RejectExecution` or an explicit parked/interrupted state); nothing unclassified commits; two-phase streaming classification must span record boundaries (line/UTF-8-safe window with bounded lookahead).
- Redaction is a pre-persistence typed transform pipeline. Large streaming output is classified/transformed incrementally before canonical chunks commit.
- Exact rich UI data may be canonical when it cannot be regenerated; terminal rendering remains derived. Retention policy decides rather than payload type being hard-coded.
- Public source hashes may be omitted or keyed for removed low-entropy secrets to avoid dictionary disclosure.
- Conceptual event envelope includes schema version, EventId, session sequence, RunId, causal parents, principal, generation, trust/audience classifications, payload, artifact refs, and integrity metadata; `lane` is dropped (undefined, shadows the one-logical-stream rule) and trigger/correlation-ID fields are replaced by typed causal references (R-09/A-S2).
- Custom schemas and upcasters (R-06/A-08/B-08/C-08/F4): the kernel validates and stores every event envelope invariant regardless of module schema; module-custom payloads are retained verbatim as canonical schema-versioned JSON, decoding as opaque-but-inspectable records; typed interpretation is a projection layered on only when the package is installed; bootstrap upcasters are kernel Rust; custom upcasters are declarative descriptors interpreted by the kernel or pinned pure functions in the no-effect runtime — never module-executed code on the reconstruction path; reconstruction never loads or executes old module packages; packages referenced by an event's schema descriptor are pinned by that event's snapshot closure (GC-exempt); a missing package ⇒ precise partial availability and the rebuild continues.
- A session is a causal event DAG serialized through append order.
- Parents must already be committed, preventing cycles.
- Conversation is a non-destructive tree projection, not the universal canonical structure.
- Root and children use RunIds in the same stream.
- Headlong history branching means creating a new causal future from a context checkpoint, never deleting or pretending to reverse prior experience.
- `continue_from(checkpoint)` quiesces the current root and starts a successor root from a historical causal frontier.
- `fork(checkpoint)` keeps the active root and starts a bounded, reduced-authority speculative run.
- `adopt(fork)` explicitly changes the active perpetual root after reconciling domain state.
- Workspace restoration, module-state restoration, memory retraction, and external-effect compensation are separate typed operations; conversation branching does not imply them.
- Forks merge selected artifacts, evidence, plans, patches, or memory proposals through domain seams. They do not merge Luau heaps, scheduler queues, or active-memory scores.
- Exact checkpoint shape and restoration UX remain pending detailed review.
- Replay scope is intentionally narrow: Cordis-style lifecycle reversibility cleans up owned live effects, and Headlong-style audit reconstruction rebuilds projections from canonical facts without invoking effects.
- Deterministic execution/effect replay is not planned initially. `continue_from` and `fork` are new execution, not replay.
- Streaming UI deltas may be ephemeral; canonical model/tool output is committed in bounded chunks and a final structured completion.

Durability:

- required install protocol everywhere (objects, head files, session creation): temp write, fsync temp, rename, fsync parent directory; a referencing event may commit only after the referenced object's rename is dirsync-durable (R-10/B-03);
- balanced profile is default;
- completed frames flush to the OS before commit acknowledgment;
- fsync-before-consequential-effect applies under every durability profile; profiles vary only non-effect-adjacent fsync cadence (fsync after terminal run/session facts and at a bounded interval otherwise);
- strict profile fsyncs every frame;
- fast profile may acknowledge kernel-buffered writes;
- effect intent is committed before dispatch; an intent and its outcome never share a frame — committed-intent-without-outcome is the sufficient condition for interrupted/ambiguous classification (B-05);
- memory-scope transition logs always fsync before ack; the session backlink appends only after the durable transition ack (R-11/M-05).

### Catalogless sessions

- No global session catalog or session append lock; the rare project-registry stream has one writer by design (R-23/F8).
- Sessions are discovered by enumerating XDG state directories and projecting genesis frames/events into SQLite — the genesis frame is the header (R-03).
- Session-existence predicate (R-03): a session exists iff its stream contains ≥1 verifiable committed genesis frame (a torn tail with ≥1 complete record qualifies); zero-record directories are orphans — invisible to discovery, reported by the inspector, and deletable.
- Session genesis records an explicit typed ProjectId binding (R-11/M-15), resolved from locators when unambiguous; ambiguity is user-visible (prompt or explicit refusal of memory-write tools) and recorded; propose/transition actors reject claims when origin binding is ambiguous.
- Session IDs are generated without coordination.
- Cross-session relationships are unilateral references and reverse-indexed by SQLite.
- No canonical global total order exists.
- Immutable modules/artifacts are content-addressed and installed atomically.
- Scope logs are introduced only for proven global mutable state.

### Memory

Memory quality is a load-bearing requirement for perpetual cognition.

Research conclusions:

- Graphiti has the strongest existing semantics: immutable source episodes, bi-temporal facts, contradiction/supersession, and hybrid graph retrieval, but its Python/service and graph-server architecture does not fit the initial single-process binary.
- Cognee is practical and has a Rust client but is still service-oriented and has weaker temporal semantics.
- MemOS/MIRIX are rich but heavy/new.
- HippoRAG contributes useful local-PPR and associative retrieval research, not a complete memory lifecycle.
- GraphRAG/LightRAG/LlamaIndex property graphs are document-KB systems, not sufficient agent memory.
- Embedded default should be SQLite adjacency tables + FTS5 + optional sqlite-vec. CozoDB is the graph-native experiment; it is pre-1.0. Kuzu is archived.

Locked memory model:

Memory has three layers that must not be conflated:

1. **Immutable experience DAG** — the canonical session trajectory: messages, cognition steps, tools, model calls, decisions, children, and outcomes. This is episodic evidence.
2. **Per-run active-memory projection** — currently salient goals, unresolved hypotheses, recent causal history, activated claims, retrievals, and pins. This is Headlong working memory. Activation logs are disposable SQLite rows, never session-stream events (R-12/F-S5); current activation scores are disposable projections.
3. **Immutable durable claim/provenance DAG** — reusable source-backed decisions, constraints, procedures, preferences, lessons, recurring failures, and relationships. Content-addressed DAG roots replace an eternal memory-operation log.

Implications:

- Short-term memory means per-run cognitive salience, not another durable copied-fact store.
- Long-term project memory and lifetime/user memory are durable scoped views over one provenance model.
- One provenance graph has scoped, capability-filtered views.
- Claims are immutable and globally identified.
- ClaimId (UUIDv7) names the claim occurrence created in a session; the claim object embeds its ClaimId and is content-addressed over its canonical serialization; DAG edges reference ClaimIds; retrieval dedups by content digest over claim content + kind (not provenance) (R-12/M-02).
- Entities are derived projection keys extracted deterministically at SQLite-projection time from claim content; `applies_to` edges carry typed entity keys; one-hop retrieval = claims sharing an entity key with query entities, validity-filtered; no canonical entity nodes in MVP (R-12/M-03).
- Deterministic event/tool extractors add relationships already proven by structure, such as modified-file, failed-with, changed-file, and derived-from edges.
- A configurable asynchronous memory curator proposes reusable semantic claims and promotions.
- Explicit user/agent writes use the same schema, provenance, and validation path.
- Child/background curators write run-private candidates. The root agent accepts, rejects, requests evidence, or leaves them unresolved before project promotion.
- Accepted project claims append to the project XDG stream with source and root-decision provenance.
- Lifetime promotion is explicitly user-gated initially.
- Correlated child assertions derived from one source do not count as independent corroboration.
- Promotion changes visibility/eligibility and appends a destination-scoped assertion; it never mutates or silently overwrites the source claim.
- Child runs default to private active memory, private claim proposals, and attenuated inherited reads.
- Children do not see sibling-private memories or all user/lifetime memory by default.
- Corrections, contradiction, refinement, support, supersession, retraction, merge, and promotion append nodes/edges and preserve historical evidence.
- Canonical dependency edges point only to already committed events/claims, making the provenance structure acyclic. SQLite may derive reverse convenience edges.
- Memory content is untrusted evidence, never instruction authority.
- Projection-stage invariant (R-13/M-14): claim-sourced fragments can never render as system/developer authority fragments — claims render only in evidence-role fragments.
- Memory claim/edge/root-manifest objects are kernel-validated bootstrap meta-schema structures (schema version, digest, acyclicity, refs-to-committed objects); the edge vocabulary stays schema data (R-12/M-01).
- Semantically meaningful claim proposals, root approval/rejection, promotions, contradictions, and memory-root transitions are canonical session/domain events.
- Project and lifetime memory use immutable content-addressed claim/provenance DAG objects plus atomic mutable root pointers in XDG state.
- Root manifests are deltas with an explicit parent edge; the current set is the projection-time fold (R-12/M-09).
- Each project/lifetime scope has one narrow canonical `transitions.jsonl.zst` stream and one writer/CAS actor. A transition supplies expected old root, accepted new root, origin session/event, decision principal, and TransitionId.
- Scope-transition commit is authoritative; the originating session records the accepted TransitionId as a backlink. Stale competing roots fail and must reconcile/rebase. `head.json` is repaired from the scope log.
- Transitions carry an idempotency key (originating session/event + decision digest); the CAS actor rejects a second transition with the same key. Recovery scans referenced scope logs for accepted-but-unbacked transitions and appends the backlink idempotently by TransitionId; a second transition is never constructed for an already-committed one (R-11).
- Rebase rebuilds the delta over the winner's root and retries CAS, bounded (≤3) retries, else records an explicit "promotion deferred" canonical fact; failed-CAS installed objects are recorded as expected orphans.
- The transition actor verifies the origin reference is a typed root-approval event matching the claim digest and current generation (R-11/M-12).
- Branch records carry `MemoryFollowPolicy { FollowHead, PinnedAt(TransitionId) }`, default `PinnedAt(checkpoint's pinned root)` for `continue_from`; retrieval for a run resolves claims against the run's pinned root, never the head; re-following the head is an explicit recorded transition. Quiesce before a branch transition is a canonical fact (or a field of the branch-transition record) listing cancelled/ambiguous pending intents (R-11/E-04/B-10/M-17).
- Model calls and consequential events pin exact project/lifetime memory-root digests, preserving historical knowledge visibility for audit reconstruction.
- SQLite projects graph/query indexes from immutable DAG objects, scope transition logs, and canonical session facts.

Conceptual graph:

```text
MemoryClaim {
  ClaimId (UUIDv7), kind, content, owner, visibility_scope,
  provenance, observed_at, valid_from, valid_until,
  validation_status (derived), sensitivity
}

MVP claim-DAG edges (6):
  evidence_for, supports, contradicts, supersedes,
  promoted_from, applies_to
```

- MVP edge vocabulary (R-12/M-13): `derived_from`, `supports`, `contradicts`, `supersedes`, `promoted_from`, `applies_to`; merge = supersession with two predecessors; the claim-DAG provenance edge formerly named `derived_from` becomes `evidence_for` (the experience-DAG structural edge keeps `derived_from`).
- Curator-asserted `confidence` is dropped; `validation_status` is derived from canonical events only (`Proposed`/`Approved`/`Active`/`Superseded`/`Retracted`), never curator-written (R-12/M-07).
- Validity ends only via supersession/retraction edges in MVP; retraction is expressed as `supersedes` without a successor (no separate `retracts` edge in the six-edge vocabulary); wall-clock temporal fields are display/heuristic only, never ordering (R-12/M-08).

Active-memory default:

- Per-run active memory is disposable projection state. Semantically meaningful pins/open loops and the selected context at model-call boundaries are canonical; every intermediate score change is not.
- Activation logs are disposable SQLite rows, never session-stream events; only pins, open loops, and selected-context at model-call boundaries are canonical (R-12/F-S5).
- Current salience is a deterministic configurable projection, combining causal recency, repeated use, unresolved goals, pins, and bounded graph relationships.
- No additional LLM call is required on every wake merely to reconstruct working memory.
- The scoring module and version are recorded for replay and evaluation.
- Generic unconstrained spreading activation is not required initially.

Minimal retrieval pipeline:

1. Resolve caller capabilities and allowed memory scopes.
2. Parse exact project entities: paths, symbols, commits, errors, tickets.
3. Run FTS5/BM25 lexical search.
4. Apply validity/supersession filter: superseded claims excluded from the result set but annotate survivors as contradictions (R-12/M-04).
5. Apply authority ordering and lineage-union dedup (R-12/M-07).
6. Fuse rankings.
7. For relational/temporal queries, expand one or two graph hops or run bounded local graph retrieval.
8. Rerank source-backed evidence.
9. Return bounded claims with provenance and contradictions.

Dense semantic search is deferred (R-20); it re-enters as one more stage behind the memory benchmark plan.

Do not initially build generic spreading activation, always-on PageRank, or community summaries. Add them only after retrieval benchmarks show need.

Memory evaluation must separately measure writing fidelity, evidence retrieval, temporal/stale-fact handling, answer support/abstention, downstream coding utility, latency/cost, and graph growth. Useful benchmarks include LongMemEval, LoCoMo, MemoryAgentBench, MemoryArena, LongMemEval-V2, NoLiMa-style lexical-gap tests, and private executable coding histories.

## Important corrections and rejected simplifications

- Threads isolate ordinary errors and unwinding panics, not segfaults/UB/abort. Wasm is the chosen in-process fault boundary for agent Luau.
- A restricted Bash `PATH` is not a safe capability shell; Bash retains redirection, absolute-path execution, source, builtins, inherited FDs, and other ambient effects.
- Start with Luau capability code instead of designing a shell-like DSL.
- One language should not be mistaken for one security/ABI layer. Luau is the authoring surface; WIT and subprocess protocols are backend contracts.
- A universal CognitiveAct registry was rejected after checking DeepSeek Harness. Use one narrow cognition-step seam plus domain-specific registries/services.
- Nested child log files were rejected in favor of one session stream because cross-log spawn/join/recovery adds ambiguity and coordination with no likely performance need.
- A global session catalog was rejected. SQLite provides disposable discovery.
- Persist domain facts, not UI gestures.
- Full VM/heap snapshots are rejected as canonical module state.
- Fine-grained module-state quota machinery was deferred; simple pre-append size errors suffice initially.
- Native per-call OS sandboxing was rejected for initial scope; outer sandboxing is a user concern, while module capability mediation remains internal.

## Memory storage and deletion

Memory is stored in XDG state, never inside project repositories.

Canonical memory layout:

```text
$XDG_STATE_HOME/<harness>/
├── sessions/<SessionId>/events.jsonl.zst
├── memory/
│   ├── lifetime/
│   │   ├── transitions.jsonl.zst
│   │   ├── head.json
│   │   └── objects/<digest>
│   └── projects/<ProjectId>/
│       ├── transitions.jsonl.zst
│       ├── head.json
│       └── objects/<digest>
└── projection.sqlite
```

- Run-private experience and semantically meaningful memory decisions are canonical in the originating session stream (root-selection transitions themselves commit in the per-scope transition log; the originating session records the backlink — R-11).
- Project/lifetime claims and provenance are immutable content-addressed objects assembled into immutable DAG root manifests.
- Each scope's `transitions.jsonl.zst` is the canonical ordered root-selection history. Its single writer/CAS actor requires an expected old root, commits the accepted new root, and records origin session/event and decision principal.
- Atomic `head.json` is a convenience pointer repaired from the scope transition log.
- Promotion creates a destination-scoped immutable claim linked to the source, with a bounded evidence excerpt, source event references, and hashes. The scope transition commits first; the originating session then records the accepted transition ID as a backlink.
- Promoted claims therefore survive later source-session deletion. Missing original episodes make provenance partially unavailable rather than fabricating evidence or automatically retracting the promoted claim.
- SQLite joins all scopes into one disposable provenance graph and records indexed root digests.
- Automatic canonical DAG-object GC is disabled in the MVP. A later GC requires coordinated root capture, writer pins, quarantine, and a grace period from last reference.

Post-review annotation (R-11/R-12): this layout matches the locked model — per-scope transition logs are the canonical root-selection record and sessions record backlinks; root manifests are deltas (R-12/M-09). Only framings that put root selection solely in the session stream, or omit per-scope transition logs, would be superseded.

Out-of-band filesystem changes are treated literally, not assigned invented domain semantics:

- deleting a whole canonical stream physically deletes that history;
- deleting or editing bytes in the middle is corruption;
- a truncated final Zstd frame is recoverable as a torn tail;
- removing a file is not interpreted as a semantic retraction event;
- SQLite reconciliation removes or marks unavailable derived rows, and a clean rebuild projects only surviving canonical data;
- deleted canonical bytes are never reconstructed from SQLite.

Explicit in-band retraction/deletion commands may be designed later. Their absence does not block the initial architecture.

### Administrative operations

- There is no hidden system/admin session.
- Project creation/link/split operations use the canonical project-registry stream.
- Project/lifetime memory transitions originate in visible sessions; manual maintenance uses a normal visible session.
- Immutable module packages and per-event execution snapshots preserve module audit. No module-operation or global administrative log is introduced without a concrete unmet requirement.

## MVP scope

The first runnable milestone is a perpetual-cognition vertical slice intended to establish architectural feasibility before subjective expert dogfooding.

In scope:

- branded Base58 UUIDv7 IDs, including SessionId, ProjectId, BranchId, RunId, EventId, ToolCallId, ClaimId, ModuleId, and GenerationId (message identity is its committing event — R-19/A-S4);
- one Rust session actor, typed FSMs, canonical Zstd-framed JSONL audit, per-session immutable objects, execution snapshots, and disposable SQLite reconstruction;
- kernel bootstrap meta-schema, one provider engine/provider, normalized provider lifecycle, and cache-aware typed context projection;
- interactive responder plus perpetual root cognition, configurable scheduler under Rust bounds, and Rust-supervised bounded child agents;
- Luaur inside per-generation Wasmtime instances;
- unified modules, explicit packages/reactive services, transactional root-child dynamic scopes (single level, ephemeral — R-26), capability handles, and required-dependency cycle rejection;
- immutable-after-intent tools: typed filesystem read/search/write/patch, Git status/diff, broad process execution, memory query/propose, todo/task state, and child spawn;
- no-effect classification/redaction/retention with two-phase streaming handling, shipped as a Rust built-in default policy (store-all or simple pattern redaction) with the module seam defined; the replaceable no-effect policy runtime is deferred (R-20);
- immutable experience DAG, deterministic active-memory projection, root-approved project claims, SQLite exact-entity + FTS5/BM25 fusion, temporal/supersession filtering, and deterministic one-hop graph expansion (dense retrieval deferred — R-20);
- per-project memory DAG/root transition actor and ProjectId locator registry;
- composed semantic UI: kernel terminal/fallback boundary plus one built-in UI authored as an immutable module generation through the standard contribution contract, with composition-failure fallback; multi-module slots/reducers/atomic composition deferred post-MVP (R-20/R-27);
- `continue_from(checkpoint)` only for initial branching;
- crash recovery to explicit interrupted/ambiguous outcomes;
- separate optional OpenTelemetry-compatible telemetry (OTel correlation and storage reporting deferred — R-20; a manual usage check before the dogfooding gate (M7) compensates deferred GC growth).

Architecture acceptance before dogfooding:

- SQLite deletion followed by audit reconstruction succeeds;
- crash injection at object install, event commit, effect dispatch, outcome, memory-root transition, config activation, and head update produces explicit valid recovery;
- generation replacement leaves no stale registrations/tasks/scopes;
- Wasm traps do not corrupt the session actor or canonical state;
- capabilities attenuate and stale generations cannot act;
- project-memory root CAS handles concurrent sessions deterministically;
- execution-snapshot closure verifies and exports;
- config reload and UI composition publish atomically; on in-process failure retain the last valid composition; on restart with invalid config, activate built-in safe mode with an explicit error (R-01/C-02);
- custom schemas/upcasters reconstruct or report precise partial availability (kernel-validated envelopes, opaque-but-inspectable module payloads, no module code on the reconstruction path — R-06);
- no-effect policy plugins cannot invoke effect capabilities;
- `continue_from` creates a valid new branch without rewriting history;
- terminal restoration/fallback remains reliable.

Per-milestone acceptance matrix (R-21/H-06): M1's gate covers kernel-local crash points only; later milestones exercise their owning subsystem. Crash-injection harness and property-test framework are explicit M1 deliverables.

| Acceptance bullet | Earliest milestone |
|---|---|
| SQLite deletion followed by audit reconstruction succeeds | M1 |
| Crash injection at object install / event commit produces explicit valid recovery | M1 |
| Crash injection at effect dispatch / outcome / memory-root transition / config activation / head update | owning milestone (M2/M4/M6) |
| Generation replacement leaves no stale registrations/tasks/scopes | M2 |
| Wasm traps do not corrupt the session actor or canonical state | M2 |
| Capabilities attenuate and stale generations cannot act | M2 |
| No-effect policy plugins cannot invoke effect capabilities | M2 (built-in default; seam defined) |
| Project-memory root CAS handles concurrent sessions deterministically | M4 |
| Execution-snapshot closure verifies | M1; export at M6 |
| Config reload publishes atomically; UI composition publishes atomically | M2 (config); M5 (UI) |
| `continue_from` creates a valid new branch without rewriting history | M6 |
| Custom schemas/upcasters reconstruct or report precise partial availability | M1 (version field + one fixture); M7 (broaden coverage) |
| Terminal restoration/fallback remains reliable | M5 |

15 consistency tests → milestone that first exercises them:

| Test | Milestone |
|---|---|
| 1 Owner (disposal) | M2 |
| 2 Authority | M2 |
| 3 Canonical fact | M1 |
| 4 Snapshot | M1 |
| 5 Payload | M1 |
| 6 Crash | M1 |
| 7 Recovery | M1 |
| 8 History (branching) | M6 |
| 9 Privacy | M2 |
| 10 Replay honesty | M2 |
| 11 Causality | M1 |
| 12 Projection | M1 |
| 13 Hot path | M5 |
| 14 Evolution (upcast) | M1 (fixture); M7 (broaden) |
| 15 Scope | every milestone (review gate) |

Differentiator failure-mode gates (R-17/H-05):

- runaway-wake breaker trips within budget and the trip is a canonical inspectable fact;
- responder input latency stays within budget under background cognition;
- approval queue has an explicit bound with defined overflow/eviction semantics;
- budget exhaustion produces an explicit `Blocked` terminal outcome recorded canonically.

Provisional numeric budgets (R-21/H-04) — provisional; ratified at kernel review from spike data:

| Budget | Value |
|---|---|
| Interactive input ACK | p99 ≤ 50 ms with ≥1 background wake/s |
| Event-commit ACK | p99 ≤ 10 ms at ≥100 events/s sustained |
| Projection rebuild | ≥10k events/s streaming |
| Wasm callback / instance cold start | p99 ≤ 1 ms / ≤ 100 ms |
| AppendLog | write amplification ≤ 2× raw JSONL; O(1) per-frame verify; torn-tail recovery O(tail) |
| Breaker trip | ≤ 1 s |
| Snapshot closure | O(active) not O(history); ≥90% dedup |
| Rebuild 5M-event stream | ≤ 15 min, < 512 MB RSS |
| Export/closure | ≤ 2× read time |

## Explicit MVP non-goals

- deterministic execution/effect replay;
- daemon/background operation after interactive exit;
- hosted multi-tenant isolation;
- per-tool OS sandboxing;
- lifetime memory and automatic project promotion (automatic = promotion without a recorded root-approval decision fact — prohibited; approved promotion remains in MVP);
- durable desired scopes and nested scopes (R-26);
- replaceable no-effect retention policy runtime (Rust built-in default ships — R-20);
- dense retrieval (re-entry via the memory benchmark plan — R-20);
- distributed multi-module UI composition (R-20);
- OTel correlation and storage reporting (R-20);
- upcaster framework machinery beyond the M1 version field, versioned-record registry, and one exercised fixture (R-20);
- PPR, unrestricted spreading activation, community summaries, general path search, and learned reranking;
- multiple provider wire protocols;
- full independent-session fork/adopt/import and workspace restoration;
- working-tree snapshots;
- public language-neutral Wasm Component plugin ABI beyond the internal Luaur host ABI;
- automatic package dependency solving;
- automatic canonical-object/package GC;
- raw Bash/capability-shell DSL;
- VM/heap snapshot persistence;
- canonical UI gesture history;
- remote clients or A2A scheduling.

## Other unresolved implementation details

- Exact memory claim/entity schemas and deterministic versus LLM extraction boundaries (claim/entity schemas specified in-body — R-12; entity extraction is deterministic at SQLite-projection time; LLM extraction boundary stays open).
- Root-agent project-memory review mechanics and lifetime-memory user approval UX (idempotency/reconciliation/kernel-verified provenance specified — R-11; UX stays open).
- `sqlite-vec` versus another embedded dense index; embedding provider/model versioning (deferred with dense retrieval; re-entry via the memory benchmark plan — R-20).
- Detailed provider contracts, conformance tests, cache capabilities, and auth primitives.
- Tool schema/stream/error/idempotency contracts and exact approval digest (approval digest specified in-body with dispatch re-verification — R-16; tool schema contracts stay open).
- Checkpoint payload and current-versus-historical memory/config mixing UX (branch follow policy specified — R-11; payload fields stay open).
- Exact event payload vocabulary, frame batching thresholds, durability intervals, and upcaster fixtures (event payload vocabulary and frame layout specified in-body; upcaster fixtures = M1 version field + versioned-record registry + one fixture, machinery deferred — R-20; frame batching thresholds and durability intervals ratified at kernel review from spike data — S3/S4).
- Inline/object threshold, object encoding, quotas, export format, and future GC protocol (per-object size quota specified — R-22; inline/object threshold ratified at kernel review from spike data — S4).
- Module ABI versions, Luaur/Wasm host ABI, compilation cache, schema descriptor format, and old-package retention (module-ABI version carried in manifest contents; state schema descriptor fail-closed rule specified — R-07; Wasm host ABI explicitly unstable/internal until hosting spike S1; old packages pinned by snapshot closure — R-06).
- Semantic UI node/slot/reducer ABI, focus model, and fallback composition.
- Base58 fixed-versus-variable width compatibility with the Maki implementation (ratified at kernel review before M1 spec freeze; no format drift after first events — R-22/H-07; with bootstrap descriptor schema v1, an explicit precondition of M1 approval).

## Recommended architecture one-pager

### Problem statement

How might we build a fast local Rust agent workbench whose perpetual cognition is understandable and auditable, whose agent-authored behavior can change live without ambient authority, and whose context, memory, tools, providers, and interface remain deeply configurable?

### Recommended direction

Build a minimal Rust enforcement kernel around typed domain FSMs, one session actor, canonical append-only audit events, immutable content-addressed objects/snapshots, capability enforcement, Wasmtime/Luaur hosting, and terminal safety. Put product behavior into unified immutable Luau module generations with Cordis-style service dependencies, lifecycle-owned effects, transactional dynamic scopes, and atomic composition. One replaceable cognition-step service runs per accepted perpetual wake; domain operations remain in their own typed services and FSMs.

Store causal session history as checksummed/hash-chained Zstd JSONL frames, query disposable SQLite projections, and pin every state-changing event to an immutable execution snapshot (pure events reference the last-pinned manifest — P9 clarified, R-08). Separate memory into immutable experience, disposable per-run activation, and immutable project/lifetime claim DAGs whose roots are serialized through narrow per-scope transition logs. Use a typed cache-aware context pipeline and hybrid source-backed retrieval. Agent-authored Luau executes in per-generation Wasmtime instances; ordinary native developer tools remain subprocesses governed by capability policy and the user's optional outer sandbox.

### Key assumptions to validate

- Wasm-hosted Luaur has acceptable startup, callback, async-host-call, and diagnostic behavior for live modules.
- One session actor is sufficient for all expected root/child/event throughput.
- Independent Zstd frames plus a local hash chain provide acceptable append latency, compression, recovery, and verification.
- Execution-snapshot closure remains understandable and does not become a dependency-management bottleneck.
- Composed semantic UI (one built-in UI module generation plus kernel fallback) can remain safe, low-latency, and versionable; multi-module composition is post-MVP (R-20).
- Exact-entity + FTS5/BM25 + one-hop memory retrieval is good enough for useful perpetual cognition; dense retrieval re-enters only via the memory benchmark plan (R-20).
- Root-reviewed project memory avoids both pollution and excessive review burden.
- Perpetual cognition provides enough subjective continuity/value to justify spend and lifecycle complexity.

### Mandatory pre-implementation design review

No implementation begins until subsystem architecture reviews and a final integrated review approve the design. Reviews cover kernel/storage, module/Wasm/capabilities, FSM/tools/providers, context/memory, semantic UI, historical correction/resume, and integrated threat/failure/concurrency/performance behavior. Each produces an approve/revise/reject decision; at least one adversarial pass uses fresh context. Disposable research spikes must be explicitly classified and cannot silently become production foundations.

### MVP scope

After review approval, use the broad architecture-first vertical slice and seven invariant-gated milestones recorded in the high-level architecture document. The MVP includes the durable kernel, live module substrate, perpetual agent spine, hybrid project memory (exact-entity + FTS5/BM25 + one-hop; dense deferred — R-20), composed semantic UI (one built-in UI module generation; multi-module composition deferred — R-20), and `continue_from`. Architecture feasibility tests precede subjective dogfooding.

### Not doing initially

- deterministic execution replay;
- daemon/background lifetime;
- hosted multi-tenancy or per-tool OS sandboxing;
- automatic package solving;
- lifetime memory and automatic promotion (automatic = promotion without a recorded root-approval decision fact — prohibited; approved promotion remains in MVP);
- durable desired scopes and nested scopes;
- dense retrieval;
- distributed multi-module UI composition;
- replaceable no-effect retention policy runtime;
- upcaster framework machinery beyond the M1 version field, versioned-record registry, and one exercised fixture;
- OTel correlation and storage reporting;
- advanced graph algorithms/reranking/community summaries;
- multiple provider protocols;
- full fork/adopt/merge or workspace restoration;
- public language-neutral plugin ABI;
- automatic canonical-object GC;
- raw Bash or a new capability-shell DSL;
- VM heap snapshots;
- canonical UI gesture logs;
- remote clients/A2A.

### Open implementation questions

The remaining questions are contract details rather than unresolved high-level architecture: exact event/schema vocabularies, Base58 width (ratified at kernel review pre-M1 — R-22), provider/tool/Wasm/UI ABIs (Wasm host ABI unstable/internal until S1), checkpoint fields, memory entity schemas, embedded vector index (deferred with dense retrieval), object thresholds/quotas (quota specified — R-22), and post-MVP GC.

## Candidate final shape

```text
Rust harness process
├── typed domain FSMs
├── canonical per-session event writers
├── SQLite disposable projections
├── scheduler supervisor
├── cognition-step service
├── context projection pipeline
├── capability/policy broker
├── module lifecycle and transactional scopes
├── semantic UI renderer
├── model/provider gateway
├── native tool subprocess launcher
└── Wasmtime instances
    └── Luaur module generations

XDG state
├── sessions/<SessionId>/events.jsonl.zst
├── sessions/<SessionId>/objects/<digest>
├── memory/lifetime/{transitions.jsonl.zst,head.json,objects/}
├── memory/projects/<ProjectId>/{transitions.jsonl.zst,head.json,objects/}
├── projects/events.jsonl.zst
├── modules/<package-digest>/...
└── projection.sqlite
```

## Design principle

Rust defines the valid, durable, and safe execution space. Luau defines behavior inside that space. Canonical logs record domain facts; SQLite and UI are projections. Modules are live and dynamic, but structural changes are transactional and lifecycle-owned. Memory remains source-backed, temporal, scoped, and capability-filtered.