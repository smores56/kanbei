# Agent Harness Design Review Handoff

Status: ready for independent architecture review
Last updated: 2026-08-26
Revised: 2026-08-28 — applied the accepted design-review reconciliation packet (`review-reconciliation.md`).

Primary design documents:

- `high-level-architecture.md` — architecture constitution and load-bearing principles
- `architecture.md` — detailed research, decisions, alternatives, corrections, MVP scope, and open implementation contracts

This document is a handoff for multiple independent reviewing agents. It describes what to review, how to challenge it, which principles are intentional, and what evidence a useful review must return.

Fidelity rule: the high-level constitution governs load-bearing principles and subsystem boundaries. The detailed ledger governs compatible contract detail and records unresolved questions. Do not silently reconcile conflicts: cite both passages, classify one as stale or propose a constitutional change, and record the disposition.

## Shared terminology (glossary)

Distinct snapshot kinds:

- `ExecutionSnapshot` — immutable content-addressed manifest pinning module packages/generations/scopes, module-state heads, memory roots, and capability-policy versions.
- `StateSnapshot` — immutable module-private state snapshot object referenced by a state head.
- `UiComposition` — the derived immutable semantic tree published at an activation-transition commit (no separate commit point).
- `MemoryRoot` — immutable content-addressed DAG root selected by a per-scope transition log.

Distinct context meanings:

- `ProjectionOutput` — the assembled provider context produced by the context projection pipeline.
- `StepContext` — the frozen immutable projection passed to a cognition step.
- `CompressedSegment` — a compacted causal-closed prefix and its summary object.
- `ProviderRequest` — the concrete bytes/messages sent to a model provider.

Other:

- "composed semantic UI" — the reduced MVP UI (kernel terminal/fallback boundary + one built-in UI module generation), not "distributed" multi-module composition (post-MVP).
- desired-state (mutable config source) vs immutable generation — config is mutable desired state; historical behavior is pinned by immutable execution snapshots.

## Review objective

Determine whether the proposed harness can be implemented as a coherent, maintainable local system without its abstractions contradicting one another or expanding the trusted Rust core into a framework monolith.

The desired result is not approval by default. Reviewers should identify:

- inconsistent semantics across subsystems;
- abstractions that duplicate one another;
- unnecessary machinery;
- hidden mutable sources of truth;
- persistence formats that grow pathologically;
- lifecycle or crash-consistency gaps;
- places where plugin behavior leaks into the kernel;
- places where configurable behavior can bypass kernel invariants;
- UX that exposes architectural complexity to ordinary users;
- performance assumptions without credible boundaries;
- premature public APIs or storage formats;
- features that should be removed, combined, deferred, or redesigned.

The review should reduce unjustified complexity while preserving complexity shown to protect a required capability or invariant. More documentation alone is not a correction.

## Reviewer stance

Act as an adversarial architecture reviewer, not an implementation agent and not an advocate for the current proposal.

Do not:

- write production code;
- create project scaffolding;
- select dependencies merely because they are familiar;
- assume a locked decision is correct because it is labeled locked;
- reward novelty by default;
- preserve complexity to avoid disagreeing with the design;
- propose distributed systems or hosted infrastructure for hypothetical future scale;
- confuse configurable behavior with a weak kernel;
- confuse append-only audit with deterministic effect replay;
- assume all persisted bytes belong in one file;
- assume SQLite must be canonical because it is convenient to query;
- assume one plugin language must also be the wire ABI, storage schema, and security boundary.

Do:

- prefer the smallest design that preserves the intended product value;
- trace every consequential operation through authority, state, persistence, failure, and recovery;
- distinguish canonical facts, immutable objects, canonical current-state heads, and disposable projections;
- identify where a type or state machine should make an invalid state unrepresentable;
- demand explicit ownership for every live effect;
- test whether the same plugin model applies consistently to built-ins, user config, workspace config, and agent-authored extensions;
- preserve good expert UX even when internal architecture is sophisticated;
- recommend deferral when a feature does not test the primary hypothesis;
- state uncertainty and required experiments explicitly.

## Product being designed

A greenfield, local-first agent workbench for expert developers with:

- a small, strongly typed Rust enforcement kernel;
- perpetual Headlong-style cognition;
- Cordis-style reversible live composition;
- Maki/Neovim-style strong primitives exposed through Luau;
- capability-scoped agent-authored extensions;
- configurable context projection, memory, scheduling, tools, providers, retention, and semantic UI;
- one long-lived interactive process initially;
- append-only audit history and immutable payload objects;
- SQLite and other indexes as efficient disposable projections;
- no deterministic execution/effect replay initially.

The primary differentiator is trustworthy live extensibility. An agent or user should be able to add and replace meaningful behavior during runtime without restarting the harness, while lifecycle ownership, capabilities, transactions, typed boundaries, and Wasm guest isolation constrain defects.

## Primary hypothesis

The architecture exists to test this hypothesis:

> A perpetual agent with configurable cognition, context, active memory, durable project knowledge, and live capability-scoped extensions can provide better continuity and adaptability than a conventional reactive agent loop without becoming impossible to understand, recover, or safely configure.

A subsystem that does not help test this hypothesis or protect a necessary invariant should face a strong presumption of deferral.

## Load-bearing design principles

Reviewers may challenge these principles, but must identify the product capability or invariant lost by changing one.

### 1. Keep the Rust core small and strong

Rust should own enforcement mechanisms and hard invariants, not default product behavior.

Expected Rust kernel responsibilities (three tiers — R-19):

Tier 1 — enforcement kernel (mechanisms and invariants only; never depends on tier 2 to enforce an invariant; safe fallback UI stays here):

- branded identity types and canonical codecs;
- bootstrap meta-schema for packages, ABIs, services, capabilities, event envelopes, execution snapshots, module schema descriptors, and memory claim/edge/root-manifest objects;
- typed domain FSM transition validation;
- one session actor owning canonical transitions;
- append-log framing, integrity, durability, and recovery;
- immutable object installation and verification;
- execution-snapshot validation and closure traversal;
- module/Wasm lifecycle, cancellation, fuel, resource limits, and stale-generation rejection;
- capability and exact-approval enforcement;
- provider/tool protocol safety boundaries;
- schema versioning and bootstrap upcasting (kernel Rust); custom upcaster descriptors interpreted by the kernel or pinned pure functions in the no-effect runtime (M1: version field + versioned-record registry + one fixture; generic machinery deferred — R-20);
- projection write-gating and rebuild-verification framework (domain projection operations are tier-2 native services);
- terminal initialization, restoration, native interaction primitives, and safe fallback UI;
- structural accessibility invariants (focus reachability, labeled interactive nodes, modal escape).

Tier 2 — native built-in services (Rust implementations of the same typed module service contracts):

- default context-projection stages;
- memory retrieval mechanics;
- native render diffing;
- provider gateway mechanics;
- domain projection operations (SQLite).

Tier 3 — replaceable module behavior (Wasm/Luau module generations):

- cognition-step implementation;
- scheduler policy within hard Rust bounds;
- context projection stages;
- memory curation and ranking;
- classification, redaction, and retention policy in a no-effect policy runtime (seam defined; replaceable runtime deferred — R-20);
- tools and provider manifests;
- prompt composition and workflows;
- semantic UI tree, reducers, slots, keymaps, commands, and themes (MVP: one built-in UI module generation; multi-module composition deferred — R-20).

Review test:

> If a proposed Rust subsystem encodes a product preference rather than an integrity or enforcement invariant, explain why it cannot be a module or typed primitive.

The inverse also applies:

> If a proposed module may redefine persistence validity, authorization, recovery semantics, schema bootstrapping, terminal safety, or resource enforcement, it probably belongs in the kernel.

### 2. Commit to one consistent plugin structure

Built-in behavior, user modules, workspace modules, and agent-authored modules should use one module-generation and service model wherever trust constraints permit.

Expected common concepts:

- stable `ModuleId`;
- immutable package/content identity;
- ephemeral `GenerationId`;
- explicit required and optional service dependencies;
- one active provider per scoped service key;
- required activation dependencies form a DAG;
- lifecycle-owned registrations, tasks, timers, services, callbacks, handles, and child scopes;
- staged activation and atomic publication;
- complete disposal/quiescence;
- provider-generation changes have an explicit dependent policy—restart, rebind, reject, or remain pinned—defined by the service contract; the exact policy remains under review;
- stale generations cannot publish effects;
- dynamic structural changes occur through transactional child scopes;
- domain registries own their conflict and composition semantics.

Reviewers should reject accidental parallel systems such as:

- one config API for built-ins and another for external plugins;
- immediate mutable registries for one subsystem and immutable generations for another without a reason;
- native tool plugins that bypass capability policy;
- UI plugins with a separate lifecycle from other modules;
- custom memory plugins that bypass the standard service, snapshot, and event model;
- privileged user Luau and structurally different agent Luau APIs unless latency or authority genuinely requires the split.

Uniformity does not mean every domain uses one generic contribution type. Tools, providers, UI, memory, cognition, and policy must retain domain-specific typed contracts.

### 3. Prefer lifecycle ownership over cleanup conventions

Every live effect has exactly one owner. Parent disposal recursively disposes owned children. Async cleanup must have an explicit bounded policy for quiescence, timeout, authority revocation, and any permitted forced termination; the exact contract remains under review.

Review every resource:

- event listener;
- service registration;
- tool registration;
- UI contribution;
- timer;
- async task;
- provider stream;
- native subprocess;
- Wasm callback;
- capability lease;
- temporary file;
- pending approval;
- child run;
- dynamic scope.

For each, ask:

1. Who owns it?
2. When is authority revoked?
3. Can it act after generation replacement?
4. How is async cleanup awaited?
5. What happens when cleanup hangs?
6. What canonical fact records an interrupted or ambiguous outcome?

Cordis-style temporal composability means setup and later cleanup, not durable time travel or deterministic effect replay.

### 4. Typed FSMs decide; canonical events record facts

Do not turn the event bus into the control plane.

- Rust domain FSMs accept typed commands.
- A session actor serializes transitions.
- Expensive work executes outside the actor.
- Completion returns as a typed command with origin, request, generation, and idempotency identity.
- The actor rejects stale, duplicate, cancelled, or invalid *effect publication*; outcomes of already-dispatched host work are always committed as facts (origin generation + commit snapshot), classified `interrupted`/`ambiguous` when the origin generation is stale.
- Accepted transitions append normalized facts.

Reviewers should flag:

- events that command behavior without an owning FSM;
- state inferred only from event names or callback order;
- mutually exclusive options represented as nullable fields instead of ADTs;
- async work that can mutate canonical state directly;
- event appenders that bypass the session actor;
- recovery logic that invents missing results.

### 5. Immutable source of truth first; efficient projections second

The architecture intentionally uses different persistence forms for different semantics.

Canonical audit history:

- ordered typed JSONL records;
- independent Zstandard frames;
- frame checksums and local hash chaining;
- explicit schema versions;
- pure runtime upcasters;
- append-before-effect durability where required;
- one logical stream per session;
- physical one-file or segmented layout hidden behind `AppendLog<T>`.

Immutable canonical payloads:

- per-session content-addressed objects for large messages, tool representations, attachments, provider artifacts, and referenced typed payloads;
- immutable module packages;
- immutable module-state snapshots;
- immutable execution-snapshot manifests;
- immutable project/lifetime memory claim/provenance DAG objects.

Canonical current-state heads:

- module-state atomic heads are authoritative current private state;
- project/lifetime memory heads are selected through narrow canonical per-scope root-transition logs;
- current configuration source is ordinary mutable Luau, while historical behavior is pinned through immutable execution snapshots.

Disposable projections:

- SQLite domain views;
- current conversation and run views;
- graph adjacency;
- FTS5 indexes;
- dense-vector indexes (deferred with dense retrieval — R-20);
- active-memory scores;
- search caches;
- rendering caches;
- ephemeral UI state;
- telemetry.

A reviewer should be able to delete `projection.sqlite` and explain how all canonical and current-state data is recovered, including which private state is not reconstructable but remains canonical in its head file.

Never allow SQLite row IDs, offsets, cache keys, or internal graph IDs to become domain identity.

### 6. Identity names occurrences; typed references express relationships

- Session, project, branch, run, event, call, claim, module, and generation IDs are distinct branded Base58 UUIDv7 types sharing one validated binary/serialization implementation; message identity is its committing event (no separate `MessageId`).
- Immutable content and package identities use versioned digests instead.
- IDs, paths, timestamps, and append adjacency never imply causality, ancestry, or authorization.
- Session sequence and typed causal references are authoritative.
- Sessions are discovered without a canonical global catalog or global total order.
- `ProjectId` is stable identity; repository roots, Git common directories, remotes, fingerprints, and aliases are locator evidence recorded in the canonical project-registry stream.
- Backup/import preserves occurrence IDs; independent session forks receive fresh session identity and explicit provenance.

Reviewers should flag any design that derives session identity from first-message identity, sorts by Base58 text to infer order, treats paths/remotes as canonical identity, or leaks SQLite/object paths into domain references.

### 7. Keep source-of-truth files size-efficient

The design should avoid both extremes:

- one enormous session log containing every raw byte;
- millions of tiny files for trivial values.

Expected approach:

- small typed values remain inline in event frames;
- large logical payloads externalize to immutable objects above a configurable threshold;
- object identity hashes canonical logical bytes with a domain separator, not compressed representation;
- text/JSON objects may use Zstd;
- already-compressed media uses an appropriate native encoding;
- rich UI truth may be retained separately from model-visible truth when it cannot be regenerated;
- terminal rendering, wrapping, highlighting, and thumbnails remain derived;
- append-only logs do not duplicate full context, transitive ancestry, or repeated stable schemas unnecessarily;
- model/tool output streams commit bounded chunks or finalized representations, not one object per token/line;
- execution snapshots deduplicate stable runtime manifests;
- module state uses immutable snapshots and heads rather than an eternal update log;
- project/lifetime memory uses immutable DAG objects plus small ordered root-transition logs.

Reviewers must stress-test:

- a multi-year perpetual session;
- a tool emitting gigabytes;
- a plugin writing large state repeatedly;
- thousands of short tool calls;
- image-heavy sessions;
- many module generations;
- repeated context snapshots;
- branches sharing payloads;
- project memory updated across many sessions.

For each, estimate which individual files grow, whether growth is bounded or segmentable, whether the source remains independently verifiable, and whether normal queries remain fast.

The architecture should allow a future physical segmented backend without changing event semantics.

### 8. Preserve good inspection and export UX

Efficient binary/compressed storage must not turn the system into an opaque database.

Expected UX:

- `zstdcat` or an official command can produce canonical JSONL;
- events use branded human-readable IDs;
- a session inspector resolves object references and execution snapshots;
- verification reports frame/object/closure failures precisely;
- export packages contain the event stream plus required object closure;
- thin exports clearly declare missing external/package references;
- SQLite remains inspectable with ordinary tools but is never mistaken for canonical truth;
- users can see which module/policy/provider/memory roots produced an event;
- retention/redaction decisions are explainable without exposing removed secrets;
- partial audit availability is explicit rather than silently repaired from caches.

Manual file editing is not a supported semantic operation. Mid-file edits are corruption, not retractions. Out-of-band deletion removes actual data and must never be interpreted as a domain event.

### 9. UX must hide internal machinery until relevant

Expert configurability does not justify exposing every internal concept during normal use.

Default experience should not require users to understand:

- frame chains;
- object digests;
- execution snapshots;
- memory root CAS;
- service generations;
- schema upcasters;
- capability handles;
- SQLite watermarks.

Users should encounter these through clear product concepts:

- session;
- branch;
- run/child task;
- module/plugin;
- project memory;
- pending approval;
- interrupted work;
- current config versus historical config;
- retained versus redacted output;
- partial provenance.

Review the UI and CLI for:

- comprehensible defaults;
- concise failure messages with drill-down detail;
- atomic reload behavior;
- safe fallback when UI composition fails;
- discoverable module contributions and conflicts;
- inspection of pending/blocked cognition;
- clear distinction between context branching and workspace restoration;
- explicit provider/model continuity loss;
- bounded approval queues;
- easy pause/stop of perpetual cognition;
- no surprise background daemon.

### 10. Cache optimization cannot alter semantics

Context projection should maximize provider prompt-cache reuse, but semantic authority wins.

Ordering priority:

1. provider protocol validity;
2. system/developer/user authority semantics;
3. conversation chronology and tool/result constraints;
4. projection intent;
5. cache stability only among fragments proven semantically equivalent.

Reviewers should reject any cache plan that moves stable project memory or tool schemas ahead of higher-authority or chronologically required messages merely to increase hits.

Every model call should retain:

- selected event/object/claim references;
- final context-fragment digest;
- projector and module identities;
- provider/model capabilities;
- cache plan and outcome;
- explicit treatment of incompatible opaque reasoning artifacts.

### 11. Agent-authored code remains capability-scoped

Agent-authored Luau runs through Luaur inside per-generation Wasmtime instances. This is an in-process guest-fault boundary, not proof against Wasmtime, host, allocator, or kernel faults.

Authority comes only through typed host imports and capability handles. Effective authority is the intersection of:

- declared requirements;
- user/workspace policy;
- parent delegation;
- current budget.

Delegation only narrows. Changed workspace config/package hashes cannot silently retain unrelated grants.

Native tool subprocesses are not per-call OS-sandboxed initially. Users may sandbox the complete process tree. The harness must describe launch constraints honestly and never market them as kernel confinement.

### 12. Retention policy is configurable but effect-free

There is no mandatory Rust secret-classification algorithm.

Replaceable classification, redaction, and retention plugins inspect bounded candidates inside a kernel-enforced no-effect runtime. They cannot invoke network, model, tool, process, arbitrary filesystem, memory-write, or installation capabilities.

The kernel guarantees:

- policy runs before harness-controlled persistence and telemetry;
- outputs and transforms are bounded and typed;
- model-influential omitted content creates explicit non-resumable/non-replayable boundaries or rejection;
- policy package identities are pinned;
- native subprocess side effects remain outside output-retention guarantees.

Review whether the design accidentally copies raw candidates into temp files, logs, panic messages, telemetry, SQLite, or crash reports before policy.

### 13. Memory has three distinct layers

Do not collapse these:

1. **Experience** — immutable session event DAG.
2. **Activation** — disposable per-run salience projection over recent causal history, goals, pins, retrievals, and memory claims.
3. **Durable knowledge** — immutable source-backed claim/provenance DAG under project and later lifetime roots.

Project memory updates:

- child/background curator proposes private candidates;
- root agent approves/rejects/requests evidence;
- immutable DAG objects install;
- per-project root-transition actor compare-and-swaps expected old root to new root;
- canonical scope transition commits;
- originating session records the accepted transition reference;
- SQLite projects exact-entity + FTS5/BM25 + one-hop views (dense retrieval deferred — R-20).

MVP retrieval:

- exact entity lookup;
- FTS5/BM25;
- dense retrieval (deferred post-MVP — R-20);
- deterministic rank fusion;
- temporal/supersession filtering;
- deterministic one-hop expansion;
- provenance-rich selected claims;
- selected claims/final context are canonical audit, while embeddings and candidate matrices are disposable.

Reviewers should challenge contamination, correlated child evidence, stale claims, source deletion, branch/project scope, and root-transition races.

Project memory is in MVP scope. Lifetime memory is a future extension and lifetime promotion remains user-gated; review its shared invariants and compatibility constraints, not its implementation readiness.

### 14. No deterministic execution replay

The initial system supports:

- lifecycle reversibility for live owned effects;
- audit reconstruction from canonical facts and immutable objects;
- `continue_from(checkpoint)` as new execution.

It does not promise:

- deterministic rerun of old model/tool/plugin behavior;
- restoration of arbitrary Luau heaps;
- exact reconstruction of every unpinned private state mutation;
- replay of external side effects.

Unresolved intents recover conservatively as interrupted or ambiguous. Retries are new attempts with current authorization and linked provenance.

## High-level architecture under review

```text
Rust harness process
├── enforcement kernel (tier 1: mechanisms and invariants only)
│   ├── IDs/bootstrap schemas/codecs
│   ├── session actor and domain FSMs
│   ├── AppendLog and immutable object stores
│   ├── execution snapshots and state heads
│   ├── schema versioning and bootstrap upcasting
│   ├── capability/approval/resource enforcement
│   ├── Wasmtime/Luaur lifecycle supervision
│   ├── provider/tool protocol boundaries
│   ├── projection write-gating and rebuild-verification framework
│   └── terminal ownership, safe fallback UI, structural accessibility invariants
│
├── native built-in services (tier 2: Rust implementations of the typed module service contracts)
│   ├── default context-projection stages
│   ├── memory retrieval mechanics
│   ├── native render diffing
│   ├── provider gateway mechanics
│   └── domain projection operations (SQLite)
│
├── Wasm/Luau module generations (tier 3: replaceable module behavior)
│   ├── cognition provider
│   ├── scheduler policy
│   ├── context projection stages
│   ├── memory curation/retrieval
│   ├── no-effect retention/redaction policy
│   ├── tools/providers/workflows
│   └── composed semantic UI (one built-in module generation; multi-module deferred)
│
├── asynchronous execution
│   ├── model/provider requests
│   ├── native tool subprocesses
│   ├── bounded child-agent FSMs
│   └── Wasm module callbacks
│
└── disposable projections
    ├── SQLite domain views
    ├── graph/FTS indexes (vector indexes deferred with dense retrieval)
    ├── active-memory scores
    ├── UI ephemeral state
    └── telemetry
```

## Storage model under review

```text
$XDG_STATE_HOME/<harness>/
├── sessions/<SessionId>/
│   ├── events.jsonl.zst
│   ├── state/<StateKey>.head
│   └── objects/<digest>
├── memory/
│   ├── lifetime/
│   │   ├── transitions.jsonl.zst
│   │   ├── head.json
│   │   └── objects/<digest>
│   └── projects/<ProjectId>/
│       ├── transitions.jsonl.zst
│       ├── head.json
│       └── objects/<digest>
├── projects/events.jsonl.zst
├── modules/<package-digest>/...
└── projection.sqlite
```

Important qualifications:

- one physical session file is a V1 backend choice, not semantic commitment;
- every canonical append log uses the shared independent-Zstd-frame protocol;
- not all sources of truth are append logs;
- module-state heads are canonical current state with a defined integrity format (digest + schema version + checksum + last-pinned snapshot digest + sequence), written only by the session actor via temp+fsync+rename+dirsync; corrupt heads restore the newest canonically pinned snapshot with an explicit "unbacked private state lost" report;
- project/lifetime memory transition logs canonically select immutable roots;
- immutable object/package GC is disabled in the MVP;
- SQLite can be deleted and rebuilt but cannot reconstruct corrupted private module heads;
- session export must traverse execution-snapshot and payload closure.

Source warning: within `architecture.md:460-496`, only the sentences that make root selection solely a session event or omit per-scope transition logs are superseded by the locked model at `architecture.md:401-406` and `high-level-architecture.md:174-179`. Lines 481-489 of that range already match the locked model (per-scope transition logs, single-writer CAS actor, and session backlinks) and need no correction.

## Core operation traces reviewers must validate

Primary reviewers produce the assigned sequence diagrams and crash matrices; designated challengers validate them rather than duplicating them. Every reviewer marks cross-cutting traces as `Reviewed`, `Not applicable—with reason`, or `Deferred to Reviewer [X]`.

### Trace coverage

| Trace | Primary | Challenger |
|---|---|---|
| Session creation | Reviewer B | Reviewer A |
| Perpetual cognition wake | Reviewer E | Reviewer H |
| Model call | Reviewer E | Reviewer D |
| Tool effect | Reviewer D | Reviewer C |
| Module activation/reload | Reviewer C | Reviewer A |
| Dynamic child scope | Reviewer C | Reviewer H |
| Project-memory promotion | Reviewer F | Reviewer B |
| Module-state update | Reviewer B | Reviewer C |
| Composed semantic UI | Reviewer G | Reviewer C |
| Continue from checkpoint | Reviewer E | Reviewer B |
| Audit reconstruction | Reviewer B | Reviewer I |

### A. Session creation

Expected shape:

```text
create SessionId/BranchId/RunId/EventId
→ construct bootstrap execution snapshot
→ install required objects
→ append genesis event
→ acknowledge session visibility
→ project into SQLite
```

Questions:

- What exists if each step crashes?
- Can a half-created directory appear as a valid session?
- How is genesis verified without aliasing IDs?
- Can SQLite discover the session without becoming canonical?

### B. Perpetual cognition wake

```text
trigger/coalescing proposal
→ Rust validates pause, responder priority, concurrency, budget, queue/timer, and circuit-breaker bounds
→ invoke exactly one cognition-step generation
→ produce typed model/tool/memory/child/domain intents
→ commit terminal outcome: Progress | NoProgress | Waiting | Blocked | Failed | CompletedGoal
→ propose a bounded next wake
```

Questions:

- Can the responder preempt or remain responsive during background cognition?
- Can a module create a valid-but-runaway wake loop without crashing?
- Are accepted wakes and terminal outcomes paired exactly once?
- What survives process shutdown or generation replacement?
- Can reactive-only policy use the same contract?
- Which scheduler details are canonical facts versus disposable projections?

### C. Model call

```text
scheduler/responder command
→ context projection selects sources
→ freeze model-visible context and cache plan
→ append durable model intent with origin snapshot
→ dispatch provider request
→ stream ephemeral and retained candidates
→ retention/finalization
→ return completion command
→ session actor validates current state
→ append outcome with origin + commit snapshots
```

Questions:

- Which bytes must exist before dispatch?
- How are provider-opaque reasoning artifacts retained or omitted?
- What happens after success but before outcome commit?
- Does cache optimization preserve authority and chronology?
- Can a provider switch create false continuity?

### D. Tool effect

```text
proposal transforms
→ resolve/validate/normalize
→ digest exact action
→ commit intent durably
→ capability guards and approval
→ dispatch native/tool provider
→ stream through no-effect retention pipeline
→ commit immutable outcome
```

Questions:

- Can arguments change after approval?
- Can a policy module exfiltrate candidate output?
- What is ambiguous after crash?
- How are raw/model/rich outputs represented without log explosion?
- Can broad process execution bypass claimed capability distinctions?

### E. Module activation/reload

```text
load immutable package
→ validate bootstrap manifest/schema/ABI
→ resolve required service DAG
→ instantiate Wasm/Luaur generation
→ stage registrations/scopes
→ validate conflicts/capabilities
→ construct/install the resulting immutable execution-snapshot manifest
→ commit the activation transition under the pre-transition snapshot and atomically publish the new composition
→ revoke and quiesce displaced generation
→ subsequent events reference the resulting manifest
```

Resolution note (reconciliation R-01): activation is canonically recorded only when a session observes it — a typed `composition_changed` event appended through the session actor and pinned under the pre-transition snapshot. Startup and root-scope activation are non-canonical: they are rebuilt from desired state, with kernel safe mode on validation failure. No global module log exists. UI composition is a pure derivation of the composition epoch — there is no second commit point.

Questions:

- What remains live during staging?
- Which composition becomes visible at the canonical transition commit, and what snapshot explains that transition?
- Can old callbacks publish after swap?
- How are state migrations isolated and rolled back?
- What happens when disposer hangs?
- Does every subsystem use the same generation/scope model?

### F. Dynamic child scope

```text
active generation begins named transaction
→ stage tools/services/UI/hooks
→ validate dependency and conflict rules
→ publish one immutable composition update
→ own under parent scope
→ dispose atomically on mode/task end or parent replacement
```

Questions:

- Are dynamic updates replayed after restart or intentionally ephemeral?
- Can concurrent scope transactions conflict deterministically?
- Are nested scopes necessary or avoidable complexity?
- Is state separated from structural registration cleanly?

### G. Project-memory promotion

```text
private claim candidate
→ root review decision
→ construct immutable claims/edges/new root
→ install DAG objects
→ CAS expected project root in per-scope writer
→ commit transition log entry
→ append originating-session backlink
→ update head convenience pointer
→ project SQLite graph/FTS indexes (vector indexes deferred with dense retrieval)
```

Questions:

- What happens if the scope commit succeeds and session backlink fails?
- How does reconciliation avoid duplicate acceptance?
- How are stale roots rebased?
- How does source-session deletion change claim status?
- Can a child influence project memory without root approval?

### H. Module state update

```text
read current immutable snapshot
→ copy into Luaur
→ bounded update
→ validate/serialize
→ install immutable state object
→ atomically replace canonical head
→ later consequential event pins state via execution snapshot
```

Questions:

- What current state is lost if the head corrupts?
- Are old snapshots retained indefinitely in MVP?
- Can state updates race across callbacks?
- Is full-state copying acceptable for intended sizes?
- Are current-state and historical-audit guarantees stated honestly?

### I. Composed semantic UI

```text
root UI provider defines slots
→ modules stage typed component/reducer/keymap/theme contributions
→ validate cardinality/order/focus/state ownership
→ atomically publish immutable semantic tree snapshot
→ composition becomes visible at the activation transition commit (no separate UI commit point); derivation failure activates the safe fallback path
→ Rust renders and handles native interactions
→ Wasm handles bounded semantic commands/reducers
→ composition failure activates safe fallback/last-valid UI
```

Questions:

- Can a broken plugin make the application unusable?
- Is fallback accessible when keymaps/UI are replaced?
- Who owns reducer state?
- Are per-keystroke paths native?
- Is multi-module UI composition justified in MVP? (deferred per R-20; MVP ships one built-in UI module generation + fallback)

### J. Continue from checkpoint

```text
select historical causal frontier
→ quiesce current perpetual root
→ reconstruct historical conversation/active-memory inputs
→ stage current config and state migration
→ record current-vs-historical memory/config choice
→ create new BranchId
→ append branch transition
→ resume new cognition
```

Questions:

- Which state is historical and which is current?
- Are pending effects cancelled or left ambiguous?
- Can project memory leak future facts into a historical branch?
- Why is workspace restoration separate and obvious to users?

### K. Audit reconstruction

```text
delete projection.sqlite
→ enumerate canonical streams
→ verify frame chains and schema versions
→ upcast records
→ resolve object/snapshot/package closure
→ rebuild session/run/conversation/memory/project/UI projections
→ report partial or corrupt sources explicitly
```

Questions:

- Which data cannot be reconstructed and why?
- Can missing old packages prevent reading module events?
- Is upcasting executable code trusted safely?
- Does rebuild require invoking models/tools/plugins?
- Is the process streaming and bounded in memory?

## Cross-cutting review dimensions

Every reviewer should assess these even when assigned one subsystem.

### Consistency

- Same concepts have one name and one semantic meaning.
- Session, branch, run, event, message, claim, module, generation, scope, package, object, snapshot, and root are not overloaded.
- Commit points are explicit and non-contradictory.
- “Canonical,” “immutable,” “current state,” “rebuildable,” “audit,” “resume,” and “replay” are used precisely.
- Domain-specific seams reuse shared lifecycle/capability/storage primitives.

### Simplicity

- Each abstraction removes more complexity than it introduces.
- No mechanism exists only for hypothetical scale.
- No duplicated persistence or lifecycle systems.
- No generic framework where a narrow typed service is sufficient.
- MVP complexity is acknowledged and milestone ordering isolates risk.
- Propose concrete simplifications where supported. For each, state the removed capability, preserved invariants, and evidence that the change reduces total complexity. “No justified simplification found” is an acceptable evidenced conclusion.

### Small core

- Rust contains mechanisms and invariants, not arbitrary defaults.
- Kernel APIs remain small enough to audit and fuzz.
- Plugin replacement does not require kernel changes for ordinary features.
- Bootstrap schemas are narrow and stable.
- Unsafe/runtime code is isolated from canonical writers and authority storage as much as the one-process decision permits.

### Plugin uniformity

- Built-ins, user config, workspace config, and agent modules use the same package/generation/service/scope contracts.
- Trust changes capabilities and runtime restrictions, not contribution semantics unnecessarily.
- UI, memory, tools, providers, and policy all participate in lifecycle ownership.
- Domain registries expose explicit conflict behavior.

### Persistence efficiency

- Individual source-of-truth files do not become unmanageably large without a segmentation path.
- Large bytes are externalized, deduplicated within appropriate privacy boundaries, and referenced immutably.
- Tiny values do not create pathological inode counts.
- Full state or context is not redundantly copied into every event.
- Compression is applied where useful without hiding corruption or requiring full rewrites.
- Normal operation queries SQLite rather than scanning compressed logs.
- Full reconstruction remains possible and tested.

### UX

- Internal architecture is inspectable but not imposed on ordinary interaction.
- Errors explain the user-visible consequence first, technical identity second.
- Reload, fallback, pause, approval, branch, and recovery behavior is predictable.
- Safe defaults exist as replaceable modules.
- Expert users can discover and override composition without reading Rust source.

### Security and privacy

- Model output proposes; kernel policy authorizes.
- Retrieved memory and repository content remain untrusted evidence.
- Workspace config grants bind to content and project identity.
- Agent-authored code receives no ambient host APIs.
- No-effect policy runtimes cannot invoke side effects.
- Outer-sandbox limitations are described honestly.
- Secrets do not leak through logs, object metadata, public hashes, telemetry, errors, or temp files before policy.

### Failure and concurrency

- Every asynchronous result has origin and commit provenance.
- Commit ordering is explicit.
- Memory roots serialize across sessions.
- Session actor throughput assumptions are credible.
- Crashes produce recoverable or explicit ambiguous states.
- No automatic retry repeats a consequential external effect without an idempotency/reconciliation contract.
- Dynamic registration and UI composition are atomic.

### Evolution

- Old events remain readable without rewriting source history.
- Module schemas can be decoded without activating unsafe old behavior.
- Provider/model capability differences do not collapse into a lowest-common-denominator API.
- Physical storage can segment or repack without changing logical identity.
- Public ABI commitments are deferred until evidence supports them.

## Suggested independent reviewer assignments

Run these as separate fresh-context reviews. Reviewers should not see one another’s conclusions until reconciliation.

### Reviewer A — Kernel minimality and architecture consistency

Primary challenge ownership: session creation, module activation, and the integrated kernel boundary.

Focus:

- Rust/module boundary;
- duplicate abstractions;
- terminology;
- bootstrap cycle;
- FSM/event separation;
- execution snapshots;
- whether the kernel remains small.

Must return:

- proposed minimal kernel API surface;
- list of behavior currently misplaced in Rust;
- list of invariants currently delegated too far into modules;
- at least three simplifications.

### Reviewer B — Persistence, crash consistency, and size efficiency

Primary trace ownership: session creation, module-state update, and audit reconstruction. Challenge project-memory promotion and continue-from persistence semantics.

Focus:

- Zstd append frames;
- one logical stream and future segmentation;
- object stores;
- module-state heads;
- memory transition logs/DAG roots;
- execution-snapshot closure;
- export/rebuild;
- crash points;
- individual-file growth and many-small-files behavior.

Must return:

- commit-point table;
- crash matrix;
- 1-year and 5-year storage-growth thought experiment;
- largest likely individual files;
- missing-object/corruption policy review;
- recommended benchmarks.

### Reviewer C — Plugin/lifecycle/service architecture

Primary trace ownership: module activation/reload and dynamic child scope. Challenge tool and UI lifecycle semantics.

Focus:

- unified module generation model;
- explicit package selection;
- service dependencies;
- transactional dynamic scopes;
- reload and disposal;
- stale generation behavior;
- custom event schemas;
- consistency across UI/tools/providers/memory/policy.

Must return:

- lifecycle state machine;
- ownership table;
- cycle/conflict analysis;
- places where plugin systems diverge;
- whether dynamic child scopes are worth MVP complexity.

### Reviewer D — Security, capabilities, retention, and Wasm boundary

Primary trace ownership: tool effect. Challenge model-call handling of sensitive data and policy boundaries.

Focus:

- one-process Wasmtime/Luaur threat model;
- capability attenuation;
- workspace config approval;
- broad native process tool;
- no-effect classification/retention runtime;
- pre-persistence leakage;
- approval exactness;
- secrets and telemetry.

Must return:

- trust-boundary diagram;
- authority-flow analysis;
- bypass list;
- honest security claims the product may make;
- claims it must not make.

### Reviewer E — Cognition, context, and model/provider architecture

Primary trace ownership: perpetual cognition wake, model call, and continue-from checkpoint. Challenge tool and audit-reconstruction traces where they affect model context.

Focus:

- perpetual wake supervisor;
- generic cognition-step seam;
- responder separation;
- child agents;
- typed staged context projection;
- cache semantics;
- provider-native capabilities/opaque reasoning;
- current-config resume;
- context discontinuity.

Must return:

- cognition and model-call sequence diagrams;
- loop/runaway failure analysis;
- cache/semantic-order review;
- smallest credible context API;
- assumptions requiring empirical spikes.

### Reviewer F — Memory architecture and retrieval quality

Primary trace ownership: project-memory promotion. Challenge context and audit-reconstruction claims involving memory.

Focus:

- experience/activation/claims split;
- project roots and compatibility constraints for a later user-gated lifetime scope;
- root approval;
- immutable claim DAG;
- transition-log concurrency;
- FTS+dense+one-hop retrieval;
- temporal validity/provenance;
- source deletion;
- contamination and correlated evidence.

Must return:

- memory schema critique;
- update and retrieval sequence diagrams;
- concurrency review;
- benchmark/evaluation plan;
- minimum graph semantics needed before implementation.

### Reviewer G — Semantic UI architecture and UX

Primary trace ownership: composed semantic UI.

Focus:

- composed semantic contributions (MVP: one built-in module generation);
- root provider/slots;
- native interaction primitives;
- reducer state;
- keymaps/themes/commands;
- fallback UI;
- audit/approval/cognition/memory UX;
- internal complexity leakage.

Must return:

- minimal semantic component vocabulary;
- slot/conflict/focus rules;
- safe fallback path;
- latency-sensitive paths;
- UX simplifications and failure messages.

### Reviewer H — MVP scope and delivery risk

Focus:

- whether broad architecture-first MVP is internally orderable;
- milestone dependencies;
- disposable spikes;
- irreversible commitments;
- test gates;
- likely critical path;
- opportunities to defer without invalidating architecture review.

Must return:

- dependency-ordered review/spec plan;
- milestone risk register;
- explicit “do not implement yet” list;
- decisions that require prototypes before approval.

### Reviewer I — Fresh-context integrated adversarial review

Focus:

- read the constitution and detailed ledger before reading this handoff; record an initial architecture model and concerns first, then use the handoff only to check coverage and discover omissions;
- search for contradictions missed by subsystem reviewers;
- challenge the primary hypothesis and broad MVP;
- determine whether architecture complexity is justified.

Must return:

- approve/revise/reject verdict;
- top five architecture risks;
- top five simplifications;
- conditions required before implementation authorization.

## Review coverage rule

Every reviewer marks each of the twenty explicit questions and each assigned trace as:

- `Answered`;
- `Not applicable` with a concrete reason; or
- `Deferred to Reviewer [X]` with the expected artifact.

Reviewer-specific deliverables augment the common template; they do not require every reviewer to reproduce every diagram. Primary reviewers own assigned artifacts, challengers validate them, and the integrated reviewer checks unresolved coverage.

## Required reviewer output format

Every review must use this structure.

```markdown
# Review: [Area]

## Verdict
Approve | Approve with required revisions | Revise and re-review | Reject direction

## Executive assessment
[Maximum 500 words. State whether the subsystem fits the constitution and why.]

## Invariants reviewed
- [Invariant] — Holds | Ambiguous | Violated

## Critical findings
### [Finding title]
- Severity: Critical | High | Medium | Low
- Documents: `path:line`
- Problem:
- Concrete failure scenario:
- Violated principle:
- Smallest correction:
- Decision to reconsider or clarification only:

## Consistency findings
[Conflicting terminology, duplicate mechanisms, mismatched commit points, divergent plugin semantics.]

## Contract artifacts reviewed
[State machines, sequence diagrams, wire/storage schemas, version boundaries, and trace coverage status.]

## Alternatives and migration
[Rejected alternatives, compatibility impact, upcasting/migration strategy, and whether a locked decision is reopened.]

## Validation and budgets
[Test/fault-injection plan plus measurable latency, throughput, storage, memory, rebuild, and cleanup budgets.]

## Unresolved risks
[Open contract question, owner, evidence needed, and decision deadline.]

## Simplifications
1. [Change]
   - Removes:
   - Preserves:
   - Cost/tradeoff:

## UX impact
[What users see in success, failure, reload, recovery, and inspection.]

## Persistence impact
[Canonical facts, immutable objects, heads, SQLite projections, file growth, reconstruction.]

## Security and authority impact
[Principals, capabilities, trust boundaries, bypasses.]

## Failure and recovery matrix
| Failure point | Durable state | Recovery behavior | Ambiguity |
|---|---|---|---|

## Required experiments
| Question | Minimal spike | Pass criterion | Must remain disposable? |
|---|---|---|---|

## Required revisions before approval
- [ ] ...

## Deferred risks
- ...

## Coverage
- Explicit questions: Answered | Not applicable—with reason | Deferred to Reviewer [X]
- Assigned traces/artifacts: Produced | Validated | Deferred—with owner

## Final recommendation
[Specific next design action; no implementation unless review gate allows it.]
```

A useful finding must include a concrete failure scenario and smallest correction. Generic advice such as “add tests,” “consider scalability,” or “improve security” is insufficient.

## Reconciliation protocol for multiple reviews

After independent reviews complete:

1. Assign immutable finding IDs such as `B-03`; preserve original reviewer text.
2. Store each finding as `{id, reviewer, citations, invariant, failure scenario, proposed correction, severity, confidence}`.
3. Cluster duplicates under a separate reconciliation ID while retaining every source finding ID.
4. Record disagreements and minority opinions explicitly, especially when reviewers disagree on invariant, severity, or correction.
5. Classify each issue:
   - constitutional contradiction;
   - subsystem-spec gap;
   - implementation detail;
   - empirical uncertainty requiring a spike;
   - accepted tradeoff;
   - speculative concern to defer.
6. Record disposition as `accept | reject | merge | defer | experiment`, with rationale, owner, affected documents, required evidence, and acceptance criterion.
7. Compare corrective options by invariant coverage, failure risk, operational burden, and total mechanism count; do not prefer addition or removal by default.
8. Maintain a coverage matrix for principles, operations, non-goals, unresolved contracts, and review-gate artifacts.
9. Update the high-level constitution only for changed load-bearing principles.
10. Update the detailed ledger for all decisions, rejected alternatives, and unresolved risks.
11. Re-review from a bounded change packet containing prior finding IDs, exact document diffs, disposition rationale, and unresolved questions.
12. Run the integrated adversarial review last; it must test accepted corrections and rejected minority findings.
13. Do not aggregate verdicts by vote. Base the final verdict on unresolved load-bearing risks and unmet acceptance criteria.
14. Produce explicit implementation authorization or another revision cycle.

No issue is resolved merely because one reviewer mentions a mitigation. The correction must preserve consistency across:

- kernel boundary;
- module model;
- capability model;
- persistence and commit points;
- audit reconstruction;
- UX;
- MVP milestones.

## Decision rubric

### Approve

Use only when:

- no critical/high contradiction remains;
- commit points and recovery are explicit;
- kernel/module boundary is stable;
- persistence does not rely on SQLite as hidden truth;
- source-of-truth files have credible size/segmentation plans;
- plugin lifecycle is uniform;
- capability bypasses are addressed;
- UX has safe defaults/fallbacks;
- required empirical assumptions have bounded disposable spikes.

### Approve with required revisions

Use when corrections are local, do not change load-bearing architecture, and can be verified in the spec without another full design cycle.

### Revise and re-review

Use when:

- a commit point changes;
- canonical/current/projection ownership changes;
- a new global coordinator is required;
- plugin lifecycle diverges;
- the Rust kernel expands materially;
- an MVP subsystem should be removed or replaced;
- failure semantics are not reconstructable.

### Reject direction

Use when the subsystem cannot satisfy the constitution without replacing its central abstraction.

## Explicit questions every reviewer must answer

1. Is the Rust kernel still small enough to audit, fuzz, and reason about?
2. Does every replaceable behavior use the same module-generation/service/scope model?
3. Does any subsystem smuggle product behavior into Rust?
4. Does any module control an invariant it could bypass?
5. Is each live effect owned and fully disposed?
6. Is each consequential transition serialized through an owning FSM/actor?
7. What is the exact commit point?
8. Which bytes are canonical facts, immutable objects, canonical heads, external receipts, or projections?
9. Can SQLite be deleted without semantic loss beyond explicitly non-reconstructable private heads?
10. Which individual source-of-truth files grow indefinitely, and can the backend segment them without changing semantics?
11. Does large-output externalization avoid both giant logs and tiny-file explosion?
12. Can a missing object, old module package, or corrupt frame produce a precise partial-audit result?
13. Can async outcomes be attributed to both origin and commit environments?
14. Can concurrent sessions update project memory deterministically?
15. Can agent-authored code gain ambient authority or exfiltrate policy inputs?
16. Does cache optimization preserve semantic authority and chronology?
17. Is the default UX understandable without exposing storage/runtime machinery?
18. Is composed semantic UI (one built-in module generation) safe enough to belong in the MVP? (multi-module composition deferred — R-20)
19. Does the subsystem help test the primary hypothesis?
20. What is the simplest design that preserves its required value?

## Review deliverables

The full review cycle should produce the artifacts below. Each artifact must name an owner, contributing reviews, dependencies, acceptance criterion, and final disposition.

- reviewed architecture constitution;
- subsystem design specs;
- shared terminology/glossary;
- kernel API inventory;
- module/service/registry inventory;
- canonical event and object schemas;
- state-machine and sequence diagrams;
- storage growth and crash matrices;
- threat and authority model;
- persistence/reconstruction model;
- MVP dependency graph;
- disposable research-spike plan;
- accepted tradeoff register;
- explicit non-goals;
- final approve/revise/reject record;
- explicit implementation authorization.

Until those exist and the integrated review approves them, no production implementation or dependency commitment should begin.

## Copyable reviewer prompt

Use this prompt when assigning the document to an independent agent:

```text
Perform an adversarial architecture review using:

1. high-level-architecture.md
2. architecture.md
3. design-review-handoff.md

If assigned the fresh-context integrated review, read documents 1 and 2 first, record your independent model and concerns, and only then read document 3 to check coverage.

Your assigned review area is: [AREA].

Do not implement code. Do not assume locked decisions are correct. Optimize for consistency, simplicity, a small Rust enforcement kernel, one uniform module/plugin lifecycle, good expert UX, size-efficient immutable sources of truth, and disposable efficient projections such as SQLite.

Trace concrete operations through identity, authority, state, persistence, failure, recovery, and user-visible behavior. Identify contradictions, hidden mutable truth, file-growth hazards, duplicate abstractions, plugin-model divergence, and speculative complexity. Compare corrections objectively by invariant coverage and total complexity rather than presuming removal or addition is best.

Use the exact required reviewer output format from the handoff. Every important finding must include severity, file:line references, a concrete failure scenario, the violated principle, and the smallest correction. State whether each finding reopens a locked decision or only requires clarification.

Return an approve/revise/reject verdict. No code changes.
```
