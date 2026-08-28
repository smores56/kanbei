# Agent Harness High-Level Architecture

Status: design constitution
Last updated: 2026-08-28
Revised: 2026-08-28 — applied design-review reconciliation packet; see `review-reconciliation.md`.
Detailed record: `architecture.md`

## Purpose

Build a greenfield, local-first agent harness for expert developers that combines:

- a small, strongly typed Rust enforcement kernel;
- perpetual Headlong-style cognition;
- Cordis-style reversible runtime composition;
- Maki-style strong primitives exposed through Luau;
- capability-scoped agent-authored extensions;
- configurable context, cognition, memory, tools, providers, and semantic UI;
- durable, inspectable audit history without deterministic effect replay.

The primary differentiator is trustworthy live extensibility: users and agents can change behavior at runtime without restarting, while ownership, capabilities, transactions, snapshots, and fault boundaries constrain mistakes.

## Architecture principles

### 1. Rust defines the valid space; Luau defines behavior within it

Rust owns integrity, authority, durability, type validation, resource limits, lifecycle enforcement, and a small bootstrap meta-schema for packages, ABIs, services, capabilities, event envelopes, execution snapshots, module schema descriptors, and memory claim, edge, and root-manifest objects. Luau composes and replaces policies and product behavior through typed primitives. Configuration cannot bypass kernel invariants or redefine the format that loads and validates modules.

### 2. Facts are durable; projections are disposable

Canonical history records normalized domain facts and immutable referenced payloads. SQLite, active-memory scores, search indexes, UI state, caches, and current graph views are rebuildable projections.

Do not persist UI gestures when the resulting domain fact is sufficient.

### 3. Lifecycle reversibility is not execution replay

Cordis-style composition means every live registration, task, timer, listener, child scope, service, and handle has one owner and is cleaned up when that owner unloads.

Audit reconstruction rebuilds state from canonical facts without invoking effects and never executes module code; reconstruction validity covers only effect-free projections, and effect-requiring projections (for example dense-vector rebuilds) are excluded from reconstruction validity. Historical `continue_from` creates new execution; `fork` and `adopt` are post-MVP, deferred rather than first-class for the MVP. The harness does not initially replay old model/tool effects deterministically.

### 4. Typed FSMs make decisions; events record facts

Domain state machines enforce valid transitions. Events are outputs of validated transitions, not an untyped event-bus control architecture. Expensive work executes asynchronously. Each canonical log has exactly one serialized writer; session transitions serialize through the session actor, per-scope memory transitions through the per-scope memory CAS writer, and project-registry changes through the project-registry writer.

### 5. Identity and relationships are separate

Distinct branded Base58 UUIDv7 newtypes identify sessions, projects, branches, runs, events, messages, calls, claims, modules, and generations. A shared validated UUIDv7 representation guarantees consistent marshalling. `SessionId` names the storage/lifecycle container, `BranchId` names a causal conversation future, and `RunId` names one bounded supervised execution.

Causality, genesis, fork, promotion, and ancestry are explicit typed references. IDs never encode or imply ancestry. Session sequence, not UUID/Base58 order, is authoritative.

Immutable byte/package identity uses versioned content digests, not UUIDs.

### 6. One logical session stream owns causal order

Each session has one logical ordered canonical event stream. Root and child runs share it through distinct IDs and causal edges. Conversation is a non-destructive tree projection over the broader event DAG.

Physical storage may evolve from one file to immutable segments without changing stream semantics.

### 7. Canonical append logs share one format

Every canonical append-only log uses the same Rust `AppendLog<T>` protocol:

- complete JSONL records;
- independent Zstandard frames;
- frame checksums;
- local hash chaining;
- explicit schema versions;
- pure runtime upcasters;
- configurable durability profiles;
- torn-tail recovery;
- no ordinary rewriting of prior records.

Not every durable structure must be an append log. Immutable state snapshots and memory DAGs use content-addressed objects and atomic heads.

### 8. Large payloads are immutable objects

The session event stream owns ordering, causality, type, trust, audience, and references. Small payloads may remain inline. Large messages, tool representations, attachments, provider artifacts, and typed state snapshots use per-session immutable content-addressed objects.

Object installation precedes event commit. Missing required objects are explicit corruption or unavailability. Separate canonical message/tool/state sidecar logs are avoided.

### 9. Every event is independently interpretable

Every canonical event references a pre-event commit-snapshot digest. Manifests are pinned at state-changing transitions, run/branch genesis, and authority/policy changes; pure events reference the last-pinned manifest, so every event still resolves to the exact snapshot under which it was produced. Async intents also establish an origin snapshot; outcomes reference both the origin that produced work and the current snapshot that accepted or classified it. Immutable snapshots are acyclic content-addressed manifests that pin:

- module packages, generations, and dynamic scopes;
- module-state heads;
- project and lifetime memory roots;
- tool-registry snapshot;
- context-projection pipeline;
- cognition provider and scheduler policy;
- retention policy;
- provider/model capabilities;
- capability and policy versions.

State-changing events describe transitions; subsequent events use the resulting snapshot.

### 10. Capability authority is explicit and attenuation-only

Effective module authority is the intersection of:

- declared requirements;
- user/workspace policy templates;
- parent delegation;
- current run/session budgets.

Models and modules cannot widen their own authority. Security guards are monotonic. Consequential capabilities can only be granted by a user-authored or user-approved policy decision bound to (origin trust class, ProjectId, package content digest, capability set, purpose), recorded as a canonical approval fact; a package digest change re-prompts. Policy templates are keyed by origin trust class. Workspace- and agent-origin modules are default-deny; built-ins and explicitly user-installed user-level modules may auto-grant. The user may sandbox the whole process tree for machine-level containment; the harness still mediates module-to-module and module-to-host authority.

### 11. Agent-authored code is fault-contained in Wasm

Agent-authored Luau runs through Luaur in per-generation Wasmtime instances off the main thread. Wasm stores, linear memory, traps, fuel, deadlines, and cancellation provide the intended in-process fault boundary.

Canonical state stays in Rust-owned stores. Luau heaps, closures, coroutines, userdata, and capability proxies are disposable.

### 12. Structural mutation is dynamic, transactional, and owned

All built-ins, config, tools, providers, policies, UI, and agent extensions are immutable module generations using one lifecycle.

Modules may register dynamically through named child scopes, but coherent structural changes stage and publish atomically. Parent disposal recursively disposes children. Stale generations cannot publish late effects.

### 13. Domain seams own their semantics

Do not invent one universal action, hook, or plugin contribution protocol.

Tools, providers, cognition, scheduling, memory, retention, UI, state, and services each expose typed contracts and domain-specific composition/conflict rules. One narrow cognition-step service runs per wake; tool and memory operations still use their own FSMs.

### 14. Perpetual cognition is configurable but bounded

Luau policy chooses trigger priority, coalescing, backoff, expected utility, cognitive-step selection, and proposed wake timing.

Rust enforces pause/shutdown, cancellation, deadlines, responder priority, concurrency, budgets, queue/timer limits, stale-generation rejection, and circuit breakers.

Normal child agents are bounded. Persistent children require explicit authority, lifecycle, and budget.

### 15. Context is a typed, cache-aware projection

Context assembly is a configurable staged pipeline ending in a Rust-validated provider context. Semantic authority, role, chronology, and provider protocol determine ordering. Cache stability optimizes only among fragments proven semantically equivalent, placing stable legal content first where that cannot change meaning.

Projection fragments declare semantic order, stability, dependencies, sensitivity, and cache eligibility. Summaries are explicit model effects whose frozen outputs become projection inputs. Provider-native opaque reasoning remains opaque.

### 16. Tool intent becomes immutable before execution

Tool processing is:

```text
proposal
→ cooperative transforms
→ resolution/schema validation
→ normalization and action digest
→ canonical intent commit
→ capability guards and exact approval
→ execution
→ bounded result transforms
→ immutable outcome
```

No hook rewrites arguments after intent commit. Missing outcomes recover as explicit interrupted or ambiguous facts, never automatic effect retries.

### 17. Retention is configurable before persistence

Model and tool effects emit typed output candidates. Replaceable classification, redaction, and retention plugins choose store, transform, drop, external receipt, or reject before durable storage. They run in a kernel-enforced no-effect policy runtime with bounded candidate access and no network, model, tool, process, arbitrary filesystem, or memory-write authority.

Rust enforces policy-before-harness-persistence, replay honesty, and hard resource limits, but no mandatory secret classifier is hard-coded. Model-influential content must be retained exactly or the boundary is explicitly non-resumable/non-replayable. Native tool side effects remain governed only by the user's outer sandbox and launch policy.

### 18. Memory separates experience, activation, and durable knowledge

Memory has three distinct layers:

1. Immutable session experience DAG.
2. Per-run active-memory projection for current cognitive salience.
3. Immutable content-addressed claim/provenance DAG for durable knowledge.

Project and lifetime memory use XDG-owned DAG roots. Claims are source-backed, scoped, temporal, capability-filtered, and never instruction authority. Corrections, contradictions, supersession, retraction, and promotion preserve history through new objects/edges.

Child/background curators propose private claims. The root agent approves project promotion. Lifetime promotion is user-gated initially.

### 19. Memory and private state use immutable snapshots, not eternal mutation logs

Module-private durable state is an immutable typed snapshot plus an atomic head that is canonical current state. Project/lifetime memory is an immutable claim DAG plus an atomic root selected through a narrow per-scope canonical root-transition log. Consequential session events pin exact state and memory roots.

Memory heads are repairable from scope transition logs. Private module-state heads are not fully reconstructable unless their snapshots were pinned by canonical events. Automatic canonical-object/package GC is deferred in the MVP; later GC requires coordinated root epochs, writer pins, quarantine, and last-reference grace.

### 20. Current state may be compact; audit boundaries must remain exact

Private updates that never influence canonical behavior need not remain forever. Every canonical event pins the exact execution snapshot under which it was produced, preserving audit reconstruction without recording every transient state mutation.

## High-level structure

```text
Rust harness process
├── enforcement kernel (tier 1: mechanisms and invariants only)
│   ├── branded IDs and typed domain schemas
│   ├── session actor and typed FSM validation
│   ├── AppendLog and content-addressed object stores
│   ├── execution snapshots and immutable state heads
│   ├── schema upcasting and audit reconstruction
│   ├── capability, approval, retention, and resource enforcement
│   ├── Wasmtime/Luaur runtime and lifecycle supervision
│   ├── provider/tool protocol safety boundaries
│   ├── projection write-gating and rebuild-verification framework
│   └── terminal ownership, fallback UI, and structural accessibility invariants
│
├── native built-in services (tier 2: Rust implementations of the typed module service contracts)
│   ├── default context-projection stages
│   ├── memory retrieval and embedding mechanics
│   ├── native render diffing
│   ├── provider gateway mechanics
│   ├── terminal renderer
│   └── domain projection operations (SQLite)
│
├── Wasm/Luau module generations (tier 3: replaceable module behavior)
│   ├── cognition-step provider
│   ├── scheduler policy
│   ├── typed context-projection stages
│   ├── memory curation and retrieval
│   ├── retention/redaction policy
│   ├── tools and provider manifests
│   ├── prompt composition and workflows
│   └── semantic UI, reducers, commands, keymaps, and themes
│
├── asynchronous execution
│   ├── model/provider calls
│   ├── native tool subprocesses
│   ├── child agents
│   ├── embeddings/indexing
│   └── Wasm-hosted module callbacks
│
└── disposable projections
    ├── SQLite current/domain views
    ├── memory graph adjacency, FTS, and vectors
    ├── active-memory scores
    ├── search indexes
    └── ephemeral UI state
```

The boundary is three-tiered. Tier 1, the enforcement kernel, contains mechanisms and invariants only and never depends on tier 2 to enforce an invariant; the safe fallback UI stays in tier 1. Tier 2, native built-in services, are Rust implementations of the same typed module service contracts — default projection stages, retrieval and embedding mechanics, render diffing, provider gateway mechanics, terminal renderer — replaceable by Wasm modules under the existing scoped-key rules. Tier 3 is Wasm/Luau module generations. For SQLite projections the kernel owns the write-gating and rebuild-verification framework, domain projection operations are tier-2 native built-in services, and SQLite data itself remains a disposable projection. Kernel accessibility enforcement covers structural invariants only — focus reachability, interactive nodes have labels, and modal escape exists; richer accessibility policy is module work.

## Storage shape

```text
$XDG_STATE_HOME/<harness>/
├── sessions/<SessionId>/
│   ├── events.jsonl.zst
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

The one-file session layout is a V1 backend choice. The logical event stream may later use immutable physical segments.

## Runtime flow

```text
input or scheduled trigger
→ session actor validates domain command
→ event pins pre-event execution snapshot
→ required objects install atomically
→ canonical event commits
→ SQLite/UI projections update
→ async work launches if authorized
→ completion returns as typed command
→ session actor rejects stale/duplicate/invalid completion or commits outcome
```

Cognition flow:

```text
scheduler policy proposal
→ Rust scheduling bounds
→ one cognition-step service call
→ domain-specific model/tool/memory/child intents
→ terminal cognition outcome
→ next scheduling proposal
```

Memory flow:

```text
session experience
→ deterministic entity/provenance extraction
→ configurable curator candidates
→ root project-memory decision / user lifetime decision
→ immutable memory DAG objects
→ canonical memory-root transition event
→ atomic convenience head update
→ disposable graph/FTS/vector projection
→ capability-filtered retrieval
→ active-memory/context projection
```

## Consistency tests for every design change

A proposed feature must answer all of these:

1. **Owner:** Which generation/scope owns every live effect, and how is it disposed?
2. **Authority:** Which explicit capability permits it, and can delegation only narrow?
3. **Canonical fact:** What normalized domain event records the consequential transition?
4. **Snapshot:** Which pre-event execution snapshot explains the behavior?
5. **Payload:** Is data inline, an immutable object, disposable projection, or external receipt?
6. **Crash:** What remains after failure between intent, object install, event commit, dispatch, and outcome?
7. **Recovery:** Can audit reconstruction proceed without executing effects?
8. **History:** Does branching create a new future rather than rewrite old facts?
9. **Privacy:** Does forbidden data reach storage, SQLite, telemetry, temp files, crash reports, diagnostics, or provider egress before retention policy?
10. **Replay honesty:** If model-influential bytes are dropped, is resumability explicitly lost or execution rejected?
11. **Causality:** Are relationships explicit rather than inferred from IDs, paths, timestamps, or append adjacency?
12. **Projection:** Can SQLite and other indexes be deleted and rebuilt from surviving canonical facts/objects?
13. **Hot path:** Does Luau/Wasm stay off terminal-cell rendering and protocol parsing hot paths?
14. **Evolution:** Can old wire facts be upcast without rewriting canonical history?
15. **Scope:** Is this mechanism required now, or is it speculative platform complexity?

If a design cannot answer these clearly, it does not fit the architecture.

## Explicit non-goals for the initial system

- Deterministic execution/effect replay.
- Hosted multi-tenant isolation.
- Per-tool OS sandboxing; users may sandbox the whole process tree.
- A universal cognitive-action or plugin-contribution meta-protocol.
- Automatic package dependency solving.
- Raw Bash as the internal capability protocol.
- Persistent Luau/Wasm heap snapshots.
- Canonical UI gesture history.
- Community-summary or unrestricted spreading-activation memory by default.
- Global session catalog or global session ordering.

## Mandatory design-review gate

No implementation begins until formal architecture review approves the design. Review is subsystem-based rather than one broad approval:

1. kernel, identity, storage, object closure, and crash consistency;
2. module ABI, Wasm/Luaur boundary, services, capabilities, and lifecycle;
3. session/event FSMs, async provenance, tools/providers, and recovery;
4. context projection, cache semantics, memory DAG, and retrieval;
5. semantic UI composition and native interaction boundary;
6. checkpoint, continue-from, and resume semantics;
7. integrated threat, failure, concurrency, performance, and operability review;
8. final architecture decision record and explicit implementation authorization.

Each review must include invariants, state machines/sequence diagrams, wire/storage schemas, failure-injection matrix, security analysis, rejected alternatives, migration strategy, test plan, performance budgets, unresolved risks, and an approve/revise/reject decision. At least one adversarial review uses fresh context. No production scaffolding or dependency commitment occurs before approval; disposable research spikes require explicit classification and cannot silently become implementation.

## Architecture-first MVP milestones

After design approval, the MVP remains intentionally broad because its first acceptance gate is architectural confidence, not speed to demo. Implement it as invariant-gated vertical milestones:

1. **Durable kernel** — branded IDs, bootstrap schemas, session actor, typed events, Zstd AppendLog, object store, execution snapshots, SQLite rebuild, crash injection. M1 also delivers the schema-version field, the versioned-record registry with one exercised upcast fixture, the crash-injection harness, and the property-test framework.
2. **Live module substrate** — Wasmtime/Luaur, explicit packages, service DAG, capabilities, generation replacement, transactional scopes. Ship the Rust built-in default retention policy; the replaceable no-effect retention policy runtime is deferred with the module seam defined, and ordering stays a kernel invariant.
3. **Agent spine** — one provider, responder, perpetual cognition, bounded scheduler, typed tools, async provenance, interrupted/ambiguous recovery. The pre-registered dogfooding instrument (unattended outcome rates, interrupted-recovery success, cost ceiling, expert-task battery) must exist before M3 begins; a longitudinal continuity/cost log probe follows M3.
4. **Context and memory** — cache-aware projection, experience/activation/claim split, project identity, immutable memory DAG, root-transition CAS, root approval. Retrieval ships exact-entity + FTS5/BM25 + one-hop expansion; dense retrieval is deferred until the memory benchmark plan justifies it as one more stage. Memory usefulness probes follow M4.
5. **Semantic workbench** — kernel-owned terminal/fallback boundary, one built-in UI authored as an immutable module generation through the standard contribution contract, and composition-failure fallback. Distributed multi-module slots/reducers/atomic composition are deferred post-MVP, gated on a slot/focus/fallback spec and a latency budget.
6. **Historical correction** — checkpoints and `continue_from`, current-config staged resume, explicit provider-reasoning discontinuity; export/closure verification is prepared here and for the dogfooding evaluation.
7. **Dogfooding gate** — first broaden exercised coverage of custom event schemas/upcasters and concurrency/fault/property tests across delivered subsystems (OTel correlation and storage reporting are deferred), then evaluate with the pre-registered instrument: coherence, memory usefulness, extension ergonomics, unattended behavior, cost, and whether perpetual cognition earns its complexity. The instrument and thresholds are ratified at the cognition review before M3 begins.

A milestone does not pass until its consistency tests and crash/failure invariants pass. Later milestone interfaces may be sketched earlier, but behavior must not bypass unfinished kernel boundaries. Numeric performance budgets and the per-milestone acceptance matrix live in the detailed ledger's acceptance section (provisional values pending spike ratification at the kernel review).

## Document discipline

- This file is the architecture constitution. Change it only when a load-bearing principle or high-level subsystem boundary changes.
- `architecture.md` is the detailed research, decisions, alternatives, and unresolved-questions ledger.
- Future specs should cite this document and explicitly identify any intended exception.
