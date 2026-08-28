# Agent Harness Design Review — Reconciliation Record

Status: reconciliation complete; **final verdict: Revise and re-review** (bounded change packet required before any implementation authorization)
Date: 2026-08-28
Inputs: `high-level-architecture.md` (constitution), `architecture.md` (ledger), `design-review-handoff.md` (handoff)

Nine independent fresh-context reviews were run per the handoff assignments (A–I), with strong models, read-only, no shared conclusions until reconciliation. Original reviewer finding IDs are preserved. Duplicates are clustered under `R-xx` reconciliation IDs. Per protocol step 13, verdicts are not aggregated by vote; the final verdict follows the integrated reviewer's assessment plus unresolved load-bearing risks.

## Verdict summary

| Reviewer | Area | Verdict |
|---|---|---|
| A | Kernel minimality and architecture consistency | Approve with required revisions |
| B | Persistence, crash consistency, size efficiency | Approve with required revisions |
| C | Plugin/lifecycle/service architecture | Approve with required revisions |
| D | Security, capabilities, retention, Wasm boundary | Approve with required revisions |
| E | Cognition, context, model/provider | Approve with required revisions |
| F | Memory architecture and retrieval quality | Approve with required revisions |
| G | Semantic UI architecture and UX | Approve with required revisions |
| H | MVP scope and delivery risk | Approve with required revisions |
| I | Fresh-context integrated adversarial | **Revise and re-review** |

**Final verdict: Revise and re-review.** No reviewer found a central-abstraction contradiction; the architecture's core (small kernel boundary, unified module-generation lifecycle, AppendLog/object/heads storage split, three-layer memory, attenuation-only capabilities) is endorsed by all nine. The failures are unfinished contracts (commit points, durability, grant establishment, reconciliation semantics) and over-broad MVP scope, plus one constitutional clarification (P9 pinning granularity) and three wording amendments. None of the corrections requires replacing an abstraction; all are spec-verifiable in a bounded change packet. Implementation remains unauthorized until the packet re-review passes.

## Clustered findings

Class key: CC = constitutional contradiction/clarification · SG = subsystem-spec gap · ID = implementation detail · EU = empirical uncertainty (spike) · AT = accepted tradeoff · SD = speculative deferral.
Disposition key: accept · reject · merge · defer · experiment.

### R-01 — Module activation commit point and canonical home
Sources: A-05 (High), C-01 (High), C-11 (Medium), G consistency note.
Class: SG. Disposition: **accept (merged)**.
Both A and C independently converge on the same rule: activation is canonically recorded only when a session observes it — in-session reloads/scope publishes append one typed `composition_changed` event through the session actor, pinned under the pre-transition snapshot; out-of-session (startup) activation is non-canonical, rebuilt from desired state (config), with kernel safe mode on validation failure (see R-06 for safe mode). No global module log (`ledger:506` stands). UI composition is a pure derivation of the composition epoch — no second commit point (C-11, G-11). Canonical order: install objects → install snapshot manifest → commit event → CAS epoch → drain old generation. EpochId := digest of the manifest's composition section (A-S1 accepted; "active epoch" is not a second concept).
Owners: ledger decision record + trace E annotation. Deadline: before module-substrate spec approval.

### R-02 — Stale completions: effect publication vs outcome recording
Sources: C-03 (High), cross-confirmed by D and E crash matrices.
Class: SG (fidelity: `handoff:206` overbroad vs `constitution:116`, `constitution:154`). Disposition: **accept**.
Amend handoff: "the actor rejects stale, duplicate, cancelled, or invalid *effect publication*; outcomes of already-dispatched host work are always committed as facts (origin generation + commit snapshot), classified `interrupted`/`ambiguous` when the origin generation is stale."

### R-03 — Session-existence predicate; orphan directories
Sources: A-03 (Medium), B-06 (Medium), I + H noted the same gap.
Class: SG. Disposition: **accept (merged)**.
A session exists iff its stream contains ≥1 verifiable committed genesis frame (torn tail with ≥1 complete record qualifies); zero-record directories are orphans — invisible to discovery, reported by the inspector, deletable.

### R-04 — Retention: kernel-owned replay bit, fail-closed semantics, classification window
Sources: A-04 (High), D-07 (Medium), E-08 (Medium, continuation gate), I deferred risk.
Class: SG + fidelity. Disposition: **accept (merged)**.
Kernel owns the conservative replay-relevance default (candidates whose role can enter model context are replay-relevant unless the kernel-validated tool manifest declares otherwise; policy may only narrow retention upward; Drop on a replay-relevant candidate forces an explicit non-resumable boundary or `RejectExecution`). Policy runtime trap/timeout/limit = fail-closed — nothing unclassified commits; two-phase classification must span record boundaries. Outcome events carry `ReplayEligibility`; `continue_from` over non-replayable frontiers rejected by default with explicit `ContinuityBoundary` override.

### R-05 — Context projection kernel guards: authority filter, sensitivity non-escalation, chronology, suppression ban
Sources: E-03 (High), E-14 (Low), A-06 (Medium), E-05 (Medium).
Class: SG (violates attenuation-only authority if unassigned). Disposition: **accept (merged)**.
One kernel-owned mandatory authority filter applied to fragment sources before any replaceable stage (run's trajectory/memory read capability); replaceable stages may only narrow; validator rejects any fragment whose source references fail the run's read capability. Kernel structural check: a fragment carrying canonical conversation events beyond the frozen prefix cannot claim prefix-eligible stability. Kernel check: output fragment sensitivity ≥ max input sensitivity (no declassification without a typed transform). Lowering must include all currently selected evidence (cache must never suppress, defer, or stale-select); promotion-driven invalidation accepted and recorded. Two kernel guards + three replaceable seams (Select, Budget+Compress, Lower) is the smallest credible context API (E).

### R-06 — Custom schemas/upcasters: kernel-interpretable or opaque; no module code in reconstruction
Sources: A-08, B-08, C-08 (all Medium), F4 (High), D deferred note. Five-way independent convergence.
Class: SG + constitutional clarification. Disposition: **accept (merged)**.
Kernel validates and stores every event envelope invariant regardless of module schema; module-custom payloads are retained verbatim as canonical schema-versioned JSON, decoding as opaque-but-inspectable records; typed interpretation is a projection layered on only when the package is installed; upcasters are pure and either kernel-compiled Rust or declarative descriptors interpreted by the kernel (never module-executed code on the audit path); reconstruction never loads or executes old module packages; packages referenced by an event's schema descriptor are pinned by that event's snapshot closure (GC-exempt); missing package ⇒ precise partial availability and the rebuild continues.

### R-07 — Module-state heads: persistence contract, integrity, generation binding, fail-closed migration
Sources: B-01 (High), F2 (High), A-10 (Medium), C-07 (Medium).
Class: SG. Disposition: **accept (merged)**.
Heads are canonical current state with no defined contract today. Specify: location/format (`sessions/<SessionId>/state/<StateKey>.head`: digest + schema version + checksum + last-pinned snapshot digest + sequence); write protocol temp+fsync+rename+dirsync performed only by the session actor (or kernel state-store actor); snapshot objects in the per-session object store; kernel generation-token check on every head CAS (displaced generations' updates rejected and recorded as rejected stale effects); MVP session-scoped state only (stated); state-schema migration fail-closed — incompatible version ⇒ activation rejected atomically, old head untouched, explicit user `module reset-state` records reinitialization; corrupt head ⇒ refuse silent substitution, restore newest canonically pinned snapshot, emit explicit "unbacked private state lost" fact. UI durable drafts may only ride this mechanism explicitly (G-01: UI reducer state is always ephemeral).

### R-08 — Snapshot manifest pinning granularity and churn
Sources: B-04 (High), F3 (High), A-07 + E-12 (Medium, version fields), E-09 (Medium, related volume).
Class: CC (P9 wording) + SG. Disposition: **accept**.
Constitutional clarification of P9: pin manifests at state-changing transitions, run/branch genesis, and authority/policy changes; pure events reference the last-pinned manifest (constitution:81's "every canonical event" is over-broad relative to the ledger's own dedup intent). Manifests materialize only at event commit, never per private update; manifests remain per-event-*changing* content-addressed objects (inlining rejected — destroys dedup). Add kernel/bootstrap-schema version, module-ABI version, event-envelope schema version, and engine/toolchain digests to manifest contents (A-07, E-12); cross-engine re-derivation marked *unverifiable*, never failed. Pack-file object backend recorded as the deferred segmented-backend option (B-04).

### R-09 — Scheduler canonical event surface
Sources: E-09 (Medium), A-S2. Minor framing difference, compatible (see Disagreements).
Class: SG. Disposition: **accept (merged)**.
Canonical = wake-acceptance decision (coalesced triggers as digest lists inside it), denials/circuit-breaks with responsible constraint, run start, terminal outcome. Raw observed triggers and expected-utility scores are policy-private module state or ephemeral. Drop `lane` (undefined, shadows one-logical-stream) and trigger/correlation-ID envelope fields in favor of typed causal references (A-S2). Wake = Run: every accepted wake creates exactly one `RunId` with kind discriminator (`CognitionStep | ResponderTurn | Child`) and trigger provenance; responder priority defined as actor precedence, stream-boundary cancellation (`Failed(UserCancelled)`), no intent rollback (E-10). Child spawn routes through the tool intent pipeline (E-11).

### R-10 — Durability contract: dirsync protocol, profile-independent effect fsync
Sources: B-03 (High), B-05 (High).
Class: SG. Disposition: **accept (merged)**.
Install protocol everywhere (objects, heads, session creation): temp write, fsync temp, rename, fsync parent directory; a referencing event may commit only after the referenced object's rename is dirsync-durable. fsync-before-consequential-effect applies under every durability profile (profiles vary only non-effect-adjacent cadence). Invariant made explicit: intent and outcome never share a frame; committed-intent-without-outcome is the sufficient condition for interrupted/ambiguous classification.

### R-11 — Memory promotion: idempotency, reconciliation, durability pin, kernel-verified provenance, project binding
Sources: F1 (High), B-09 (Medium), M-05 (High), M-06, M-12 (Medium), M-15 (High), M-17.
Class: SG. Disposition: **accept (merged)**.
Transitions carry an idempotency key (originating session/event + decision digest); the CAS actor rejects a second transition with the same key. Memory transitions always commit with fsync before ack (profile-independent); session backlink appended only after durable transition ack. Recovery: scan referenced scope logs for accepted-but-unbacked transitions, append backlink idempotently by `TransitionId`; rebase = rebuild delta over the winner's root, bounded retries (≤3), else explicit `promotion deferred` fact. Kernel transition actor verifies the origin reference is a typed root-approval event matching the claim digest and current generation. Session→project binding is an explicit typed field of the genesis fact; ambiguity is user-visible (prompt or explicit refusal of memory-write tools) and recorded; propose/transition actors reject claims when origin binding is ambiguous. Branch records carry `MemoryFollowPolicy { FollowHead, PinnedAt(TransitionId) }`, default `PinnedAt(checkpoint's pinned root)` for `continue_from` (E-04, B-10, M-17 converge); re-following the head is an explicit recorded transition.

### R-12 — Memory schema and retrieval pipeline
Sources: M-01 (High), M-02 (High), M-03 (High), M-04 (High), M-07, M-08, M-13, F simplifications 1–5.
Class: SG + constitutional amendment. Disposition: **accept (merged)**.
M-01: add memory claim/edge/root-manifest objects to the bootstrap meta-schema (constitution:25 amendment) — kernel validates structure (schema version, digest, acyclicity, refs-to-committed objects); edge vocabulary stays schema data. M-02: `ClaimId` (UUIDv7) names the claim occurrence; object is content-addressed over canonical serialization; edges reference `ClaimId`s; retrieval dedups by content digest over claim content + kind (not provenance). M-03: entities are derived projection keys extracted deterministically at SQLite-projection time, carried by `applies_to` edges; one-hop = claims sharing an entity key, validity-filtered; no canonical entity nodes. M-04: validity/supersession filter moves before rank fusion. M-07: drop curator-asserted `confidence`; `validation_status` derived from canonical events only; lineage-union dedup before ranking. M-08: validity ends only via supersession/retraction edges (no silent `valid_until` expiry). M-13: edge vocabulary 9→6 (`derived_from`, `supports`, `contradicts`, `supersedes`, `promoted_from`, `applies_to`); merge = supersession with two predecessors; `derived_from` name collision disambiguated (claim-DAG edge renamed, e.g. `evidence_for`). M-09: delta root manifests. Activation logs are disposable SQLite rows, never session-stream events (F-S5; coordinate with R-09).

### R-13 — Capability grant establishment and consent gate
Sources: D-01 (Critical), D-04 (High), D-09 (Medium, narrow scope).
Class: SG + constitutional wording (§10). Disposition: **accept**.
Consequential capabilities can only be granted by a user-authored/user-approved policy decision bound to (origin trust class, ProjectId, package content digest, capability set, purpose), recorded as a canonical approval fact; digest changes re-prompt; default-deny for workspace- and agent-origin modules; built-ins and explicitly user-installed user-level modules may auto-grant; policy-type modules (retention, scheduler, projection) require the same consent on replacement. Policy templates are keyed by trust class (constitutional §10 sentence; keeps the intersection at four inputs). D-09 (memory as durable injection channel): accept narrow — claims carry an integrity/provenance class surfaced in retrieval and the projection-stage invariant of M-14 forbids claim-sourced system/developer authority fragments; semantic redesign (user gates for instruction-like claims) stays with Reviewer F as a deferred risk.

### R-14 — Caller principal attribution
Sources: D-02 (High).
Class: SG. Disposition: **accept**.
Every invocation carries the initiating principal (session/run/generation). Effective authority for a tool effect = caller ∩ tool-provider module ∩ policy ∩ budget. Approvals attribute to the human-consent principal. Closes the module-registered-tool confused-deputy path.

### R-15 — Privacy invariant widening: provider egress, crash reports, diagnostics
Sources: D-03 (High), D-05 (Medium-High), E-14 context-side.
Class: CC (constitutional test 9 wording) + SG. Disposition: **accept**.
Amend constitution test 9: sinks = storage, SQLite, telemetry, temp files, crash reports, diagnostics, **and provider egress**. Kernel invariant: diagnostics carry digests/lengths/counts, never raw candidate/provider bytes (enforce via a `SensitiveBytes` type with no `Display`/`Debug`). Each model call records a canonical egress entry (provider identity, sensitivity classes egressed, origin snapshot); policy templates may restrict egress per provider/sensitivity class. Constitution §17 subject corrected to "model and tool effects emit typed output candidates" (D fidelity finding: stale wording).

### R-16 — Dispatch-time re-verification and in-flight revocation
Sources: D-11 (High), C-10 (Medium), H-05 related.
Class: SG. Disposition: **accept (merged)**.
At dispatch: re-derive the digest from the committed intent, re-run guards, verify policy/grant versions unchanged. Approval attaches to the committed intent; at execution the kernel re-checks the *current* composition's capability intersection; revoked ⇒ intent resolves `interrupted` with user-visible reason; re-approval is a new intent. Approval scoping/expiry contract (D-12; resolves `ledger:569`): digest binds tool ModuleId+generation, action type, canonicalized arguments, cwd/env fingerprint for process tools; scope (run/session/project) and expiry explicit; standing approvals without scope are prohibited.

### R-17 — Circuit breakers: inputs, floors, canonical trip
Sources: E-02 (High), H-05 gate (High).
Class: SG. Disposition: **accept (merged)**.
Kernel-owned breakers keyed on canonical counters: consecutive `Failed`; consecutive `NoProgress`/`Waiting` without new causal events; N identical action digests within a window; spend per wall-clock window. Kernel enforces minimum floors; policy tunes only above floors. Trip appends canonical `BreakerTripped` (responsible counter) and pauses cognition until explicit user resume. Acceptance gates added (H-05): breaker trips within budget; responder latency under background cognition within H budget; bounded approval queue with explicit overflow/eviction semantics; budget exhaustion ⇒ explicit `Blocked` terminal outcome.

### R-18 — Cognition-step execution model
Sources: E-01 (High), E-10 (Medium, in R-09), E-06 (Medium), E-07 (Medium).
Class: SG. Disposition: **accept (merged)**.
`step(context, trigger)` is a bounded orchestration body over a closed set of typed host commands (`model_call`, `tool_intent`, `memory_query`, `memory_propose`, `child_spawn`, `schedule_wake`), each committing through its owning FSM; kernel checks wake deadline/budget at each host-command boundary; deadline expiry ⇒ `RunOutcome=Failed(Deadline)`; committed intents remain canonical facts; context is a frozen immutable projection. Compaction operates only on causal-closed prefixes; compaction selection is a canonical event; FSM rejects new events whose causal parents fall inside a compacted range. Provider manifests declare opaque-artifact transferability (default none); kernel validator strips untransferable artifacts at lowering; outcome events carry `ReasoningContinuity`, mandatory `Broken` on first call after a provider change.

### R-19 — Kernel boundary: three tiers, single Wasm Luaur substrate, registry mechanisms
Sources: A-01 (High), A-02 (Medium), A-11 (Medium), D-S3, G-08 (Low), C consistency #4.
Class: SG + fidelity. Disposition: **accept (merged)**.
Name three tiers: (1) enforcement kernel (mechanisms/invariants only); (2) native built-in services — Rust implementations of the same typed module service contracts (retrieval mechanics, render diffing, provider gateway mechanics, projection ops behind the kernel write-gate/rebuild-verification framework); (3) Wasm/Luau module generations. Reconcile `handoff:113` vs `constitution:216-221` accordingly. MVP: all Luau (config included) runs in Wasm-hosted Luaur with one host ABI; no native-Luau tier (defer); retention/classification runs in the same Wasm hosting path with an empty import set (D-S3). Registries for structural contribution types are kernel mechanisms with fixed typed conflict rules per contribution type; modules contribute typed entries, never resolution logic; one kernel staging/validation/publish protocol shared by all domain registries (C). Principal/project resolution is kernel-orchestrated authority machinery, never a module service key (A-09; with R-11's binding). Accessibility validation narrowed to kernel-enforced structural invariants (focus reachability, labels, modal escape); richer a11y policy is module work (G-08). Constitutional wording clarification: "each canonical log has exactly one serialized writer; session transitions serialize through the session actor" (A fidelity item 2 — the three canonical writer families).

### R-20 — MVP scope: deferral packet and hypothesis-first ordering
Sources: H-03, H-09, H-10, F5 (all High/Medium), I simplifications 1/2/4/5, G conditional.
Class: SD. Disposition: **accept (merged)** — reopens MVP scope lines `ledger:521,522,524` as deferrals only.
Deferrals: (1) distributed multi-module UI composition — M5 reduced to kernel terminal/fallback boundary + one built-in UI authored as a module generation through the same contribution contract + composition-failure fallback; slots/reducers/atomic cross-module composition post-MVP, gated on Reviewer G's slot/focus/fallback spec and a latency budget (H-03; I-S4; G's contract governs whatever ships). (2) Dense retrieval — ship exact-entity + FTS5/BM25 + one-hop; dense enters as one more stage when Reviewer F's benchmark plan justifies it (H-09, I-S5; F's plan remains the entry criterion — see Disagreements). (3) OTel correlation and storage reporting (H-09), with a manual usage check before M8 to compensate deferred GC growth. (4) No-effect retention policy runtime — ship a Rust built-in default (store-all or simple pattern redaction) with the module seam defined; ordering stays a kernel invariant; canonical policy facts when the runtime lands (I-S1). (5) Upcaster framework machinery beyond the M1 version field + versioned-record registry + one exercised fixture (H-02 merged with I-S2 — see Disagreements). (6) Module-authored UI reducers/timers (G-S1/S2). Add staged hypothesis probes after milestones 3 and 4 (F5): longitudinal continuity/cost log after M3; memory usefulness probes after M4. Pre-register the M8 instrument before M3 begins (H-10): unattended outcome rates, interrupted-recovery success, cost ceiling, expert-task battery. M7 split: upcast fixture + test harness to M1; export/closure verification to M6/M8 prep; remainder deferred (H-02, H-08, H-09). Milestone count drops to seven.

### R-21 — Acceptance gate must be falsifiable: budgets and per-milestone matrix
Sources: H-04 (High), H-05 (High, merged into R-17), H-06 (High), H-08 (Medium).
Class: SG. Disposition: **accept (merged)**.
Add the numeric budget table (provisional values below, ratified at kernel review from spike data). Publish a per-milestone acceptance matrix mapping each of the eleven `ledger:529-542` bullets to the earliest milestone where it is testable; M1's gate covers kernel-local crash points only. Crash-injection harness and property-test framework become M1 deliverables; map the 15 consistency tests (`constitution:289-307`) to milestones; M7 reworded to "broaden coverage."
Provisional budgets: interactive input ACK p99 ≤ 50 ms with ≥1 background wake/s; event-commit ACK p99 ≤ 10 ms at ≥100 events/s; projection rebuild ≥10k events/s streaming; Wasm callback p99 ≤ 1 ms, cold start ≤ 100 ms; AppendLog write amplification ≤ 2× raw JSONL, O(1) per-frame verify, torn-tail recovery O(tail); breaker trip ≤ 1 s; snapshot closure O(active) not O(history), dedup ≥ 90%; rebuild of 5M-event stream ≤ 15 min < 512 MB RSS; export/closure ≤ 2× read time.

### R-22 — Pre-M1 irreversible decisions must be ratified, not drifted
Sources: H-07 (Medium), I revision list.
Class: SG. Disposition: **accept**.
Kernel review ratifies (a) Base58 width and (b) bootstrap descriptor schema v1 as explicit preconditions of M1 approval; Wasm host ABI stays explicitly unstable/internal until the hosting spike produces data. Object file names carry digest-algorithm versions (`<alg>:<digest>`); per-object size quota required (closes part of `ledger:572`).

### R-23 — Frame format and stream layout specifics
Sources: F7 (Medium), F8 (Low), B consistency notes, I revision list.
Class: SG. Disposition: **accept**.
Specify frame = [one typed metadata record, event records…] with metadata excluded from session sequence (or chain digests moved into the following frame's header) so `zstdcat` yields clean JSONL. Remove "frame identities" from the stable caller contract (`ledger:302` — physical artifact; segmentation invalidates them). Add V1 segment-rollover triggers (frame-count/byte thresholds) to the AppendLog spec. Watermarks commit in the same SQLite transaction as the rows they cover; rebuild ignores watermarks. Reword `ledger:353`: "no global session catalog or session append lock; the rare project-registry stream has one writer by design." Discovery no longer projects undefined "headers" — the genesis frame is the header (R-03).

### R-24 — Fork authority floor and Wasm hardening parameters
Sources: D-08 (Medium), D-10 (Medium), C-04 related.
Class: SG. Disposition: **accept (merged)**.
Fork floor: read-only capabilities plus memory-propose; consequential effects require the standard approval path; the attenuated grant is a canonical fact. Require configured store limits (memory, table, instance count) per generation, epoch deadlines plus host-side timeout wrappers around every host import, and per-generation wall-clock budget. Drain policy (C-04, confirmed): quiesce cancellable effects → bounded deadline (default 5 s) → force-terminate → commit `cleanup_forced` canonical fact → finish disposal; no per-resource variants in MVP.

### R-25 — Service dependent policy and key namespace
Sources: C-05 (Medium), C-06 (Medium), H risk register.
Class: SG. Disposition: **accept**.
Two typed rules replace the four-policy matrix: service contracts carry a version; dependents declare a required version; on provider replacement, version-compatible dependents rebind atomically, version-incompatible dependents restart in the same transaction. Delete reject and pinned (pinned prohibited — it creates owner-less live effects). Keys are `ScopePath/Name`, namespaced by owning module; publication requires the key free or an explicit `replace` intent validated against capability/precedence; dependencies may point only to same-scope or ancestor-scope services; parent→child dependencies rejected in MVP.

### R-26 — Dynamic scopes trimmed
Sources: C-09 (Medium), H "do not implement" list.
Class: SD. Disposition: **accept**.
MVP scopes: children of root only (single level), always ephemeral, created with an explicit owner lease (generation or run), name-unique within parent, staged via OCC. Add "durable desired scopes and nested scopes" to MVP non-goals (`ledger:216` stays as post-MVP design).

### R-27 — UI contract set
Sources: G-01…G-11 (G-01/02/03/04 High; rest Medium/Low), H-03 (deferral), I-S4.
Class: SG. Disposition: **accept (merged with R-20 UI deferral)**.
Required for whatever UI ships: UI reducer state always ephemeral (durable drafts only via module-state heads); three fault classes (composition validation → last-valid/core UI with staleness banner; runtime component fault → component-level placeholder, module degraded; kernel render fault → kernel fallback UI); kernel-assigned input provenance on every `UiEvent` (`User`, `Module(gen)`) and module-emitted intents subject to the standard capability intersection; kernel reserved interaction floor (pause cognition, open inspector/fallback, dismiss modal, quit — unshadowable, composition-time validated; trims `ledger:255`); versioned root slot schema with revalidation-outcome rule on root swap (stale contribution renders slot-level error placeholder, never blocks composition, never silently); native-primitive partition of the per-keystroke path with latency budgets; fallback/last-valid UI renders approval/cognition/run panels from live canonical facts; keymap same-layer conflicts reject unless explicit override, with a binding-inspection surface; terminology fixes (kernel fallback vs last-valid; semantic tree is an in-memory derived value, never persisted; kernel-fallback lifecycle exception recorded; "composed semantic UI" not "distributed"). Module UI timers: none in MVP (G-11/S2).

### R-28 — Trust-boundary honesty and MVP simplifications in security
Sources: D simplifications 1–2, D bypass list, D must-not-claim list.
Class: SG + AT. Disposition: **accept**.
Drop `ExternalReceipt` from the MVP retention decision set (reintroduce when an external store exists). Demote native launch executable/argument/cwd/env *constraint policies* to recorded hygiene metadata in MVP (keeps the operationally load-bearing controls: timeout, output limits, FD closure, tree cancellation, default-deny env from D-06). Kernel-held credentials via OS keychain, injected at call time only; never in canonical records/snapshots/objects. Product may make only the claims on D's "may make" list; must not make the claims on D's "must not make" list (notably: no sandbox claim beyond guest-fault containment, no per-call tool sandboxing, no prompt-injection prevention, no secrets-cannot-leak, no tamper-proof history, no "data never leaves the machine").

### R-29 — Accepted tradeoffs register (record, do not fix)
Class: AT. Disposition: **record**.
- Per-session object stores prevent cross-session payload dedup — privacy boundary; digest equality across sessions would leak content correlation.
- GC disabled in MVP — unbounded `modules/`/`objects/` growth; acceptable only with the R-20 manual usage check before M8; failed-CAS memory objects and orphan objects are expected orphans until coordinated GC.
- Hash chain does not prove suffix removal without an external trusted head (`ledger:306`) — tamper window documented where integrity claims are made.
- Unpinned private-state history is genuinely lost on prune/head-corruption — permitted by P20, stated honestly (R-07).
- Acknowledged-but-vanished commits under the fast durability profile — documented, never discovered (B/H matrix).
- Dense retrieval deferral leaves lexical-gap queries unserved until re-entry.

## Disagreements and their resolution

1. **Upcaster framework timing** — H-02 (M1 ships version field + registry + one exercised fixture) vs I-S2 (defer all machinery to the first real schema change). Resolution: merge — version tags and the versioned-record registry exist from day one (cheap, prevents H-02's permanent dual-schema audit); the generic upcaster framework machinery is deferred (I-S2); one internal fixture exercises the registry at M1. M7 reworded to "exercise custom schemas and partial-availability reporting."
2. **Dense retrieval in MVP** — F treats it as in-scope with a benchmark plan; H-09 and I-S5 defer it. Resolution: defer behind F's benchmark plan; F's evaluation harness remains the re-entry criterion. F's schema/concurrency/benchmark deliverables stand unchanged.
3. **Distributed UI in MVP** — G verdicts it approvable-in-MVP conditional on its revisions; H-03 and I-S4 defer multi-module composition. Resolution: defer multi-module composition (hypothesis value is the tiebreaker per `handoff:90`); G's contract set (R-27) governs the reduced M5 (built-in UI as module generation + fallback). G's slot/focus/fallback spec remains the post-MVP gate.
4. **Scheduler events** — A-S2 makes triggers ordinary causal events; E-09 makes observed triggers policy-private. Resolution: E-09 governs canonicality (decisions canonical, observations private); A-S2's envelope-field removal (lane, trigger/correlation IDs) accepted as the mechanism.
5. **Ledger one-pager truncation** — A claimed `ledger:586-603` truncated; I explicitly verified the lines are complete. Resolution: I's verification stands; A's claim retracted (both fresh reads; I checked this specific item).
6. **`handoff:550` staleness warning** — B, I, and H independently found the warning overbroad: `ledger:460-496` already contains the locked transition-log/CAS/backlink model. Resolution: narrow the warning to genuinely superseded sentences; do not "double-correct."

## Constitutional changes required (all wording-level; no principle replaced)

1. P9 (constitution:81): pinning granularity — state-changing transitions, run/branch genesis, authority/policy changes; pure events inherit the last-pinned manifest (R-08).
2. Test 9 (constitution:299): sink list extended to provider egress, crash reports, diagnostics (R-15).
3. §10 (constitution:97-102): one sentence keying policy templates by trust class (R-13).
4. §17 (constitution:157-158): subject corrected to "model and tool effects emit typed output candidates" (R-15).
5. §25 bootstrap meta-schema list: add memory claim/edge/root-manifest objects (R-12/M-01).
6. §37 (P3): add the audit-reconstruction qualifier that effect-requiring projections (dense vectors) are excluded from reconstruction validity (M-11); mark `fork`/`adopt` post-MVP (I).
7. Structure diagram (constitution:186-222) and kernel responsibility list (`handoff:100-114`): adopt the three-tier naming; reconcile the projection-framework placement (R-19).
8. Writer wording: "each canonical log has exactly one serialized writer; session transitions serialize through the session actor" (R-19).

## Editorial corrections

- Handoff has two sections numbered "7" (`handoff:276`, `handoff:314`) — renumber (found independently by A, B, I, H).
- Narrow/drop the `handoff:550` staleness warning (see Disagreements 6).
- Annotate the superseded memory layout in `ledger:460-499` in place (H).
- Glossary artifact: distinct names for the four "snapshot" kinds (ExecutionSnapshot, StateSnapshot, UiComposition, MemoryRoot — I); four "context" meanings (ProjectionOutput, StepContext, CompressedSegment, ProviderRequest — E); "composed semantic UI" not "distributed"; desired-state (mutable config source) vs immutable generation (C).
- Name the ledger acceptance list (`ledger:529-542`) as the normative gate-criteria source in the constitution (H).
- "Automatic project promotion" non-goal wording: define as promotion without a recorded root-approval decision fact (F).
- Reviewer A's ledger-truncation claim retracted (see Disagreements 5).

## Required revision packet (dependency-ordered; the re-review input)

1. **Doc reconciliation** (blocking): apply constitutional changes and editorial corrections; annotate superseded passages; fix numbering; apply deferrals in all three scope statements; republish milestones with the per-milestone acceptance matrix (R-21) and budget table.
2. **Kernel/persistence spec**: AppendLog framing + R-23; durability contract R-10; object/head install protocol; module-state head contract R-07; manifest schema v1 with version fields (R-08); discovery predicate R-03; commit-point table from B's artifact.
3. **Module-substrate spec**: activation canonicality R-01; lifecycle FSM + drain policy R-24; dependent policy + key namespace R-25; state migration fail-closed (R-07); scopes trim R-26; safe mode (C-02, folded into R-01); custom schema rules R-06.
4. **Capability-and-consent addendum**: grant establishment R-13; caller principal R-14; dispatch re-verification + revocation + approval scoping R-16; privacy invariant R-15; credential custody R-28; fork floor + Wasm limits R-24; retention R-04 + ExternalReceipt removal + launch-constraint demotion R-28.
5. **Cognition/context spec**: step execution model R-18; breakers R-17; wake=run + scheduler surface R-09; projection guards R-05; branch memory policy R-11; compaction/opaque-reasoning/replay-eligibility (R-18, R-04).
6. **Memory spec**: schema ADTs, identity rule, edges, entities, pipeline order R-12; promotion/reconciliation R-11; M-14 projection invariant.
7. **UI contract** (reduced M5): R-27 items.
8. **MVP plan**: deferral packet R-20; M8 instrument (H-10); do-not-implement list (H): distributed multi-module UI; broad native process-execution tool until the approval digest + bounded queue are ratified; custom schemas beyond the fixture; OTel/storage reporting; dense retrieval; public Wasm Component ABI; automatic GC; lifetime memory; any ABI/format freeze before ratification.

## Spike register (deduplicated; all disposable per `constitution:335`)

| # | Spike | Sources | Gate |
|---|---|---|---|
| S1 | Wasm-hosted Luaur hosting: cold start, hot callback, async host-call round trip, fuel/epoch interruption, store limits, config-compile latency, trap+respawn | F6, H-S1, A, D-10, E | M2 precondition + fallback tree |
| S2 | Session-actor throughput under mixed command/outcome + wake chain + chunk commits | H-S2, A, I | Kernel review |
| S3 | AppendLog framing: append latency vs JSONL, chain verify, torn-tail drill, zstd frame-size sweep, kill -9 durability profiles, dirsync cost (APFS/ext4) | B, H-S3, I | Kernel review (format freeze) |
| S4 | Snapshot-closure growth + manifest materialization + prune: object counts, dedup ratio, 1M-file object store scale | B-04, F3, H-S4, A | Kernel review |
| S5 | Rebuild throughput/memory: 1–10 GB streams, 5M-event session | B, I | Kernel review |
| S6 | Version-pinned reconstruction across a kernel-upgrade fixture | A-07, B | Kernel review |
| S7 | Context pipeline latency: staged Select/Budget/Lower over 128k-token context in per-generation Wasmtime | E | M3 |
| S8 | Provider cache-hit stability across wakes/promotions + scheduler event volume after R-09 | E, F | M3/M4 |
| S9 | Opaque-reasoning artifact round-trip byte-exactness (2 providers) | E | M6 |
| S10 | Tokenizer estimate accuracy vs provider-enforced limits | E | M4 |
| S11 | Retrieval quality: FTS-only vs +dense ablations on synthetic coding histories; sqlite-vec at 100k claims (only if dense re-entry pursued) | F, I | M4 gate |
| S12 | Promotion-churn cache coalescing + embedding-model migration cost + root-review burden | F | M4 |
| S13 | Hostile-guest suite: fuel/memory/wall-clock bounds, host-call parking, instance churn | D-10, E | M2 |
| S14 | Streaming no-effect classification latency + cross-chunk secret corpus + keyed-receipt digest resistance | D | M2 |
| S15 | OCC structural transactions under reload/scope-storm concurrency; safe-mode startup; stale-generation outcome recording; drain deadline forced termination | C | M2 |
| S16 | Native render diff throughput + composition swap cost | G | M5 (reduced) |
| S17 | Luau/Wasmtime cross-version determinism for re-derivation (informs E-12) | E | M1 manifest freeze |

Path-critical: S1, S2 (must precede module/kernel reviews). All others gate their owning reviews.

## Coverage matrix

**Constitution principles (1–20 per handoff numbering):**

| Principle | Status | Cluster(s) |
|---|---|---|
| 1 Small strong Rust core | Ambiguous → holds with R-19 three-tier fix | R-19 |
| 2 One plugin structure | Holds; runtime-class split documented | R-19, R-27 |
| 3 Lifecycle ownership | Ambiguous until drain contract | R-24, R-25 |
| 4 FSMs decide, events record | Ambiguous for activation → resolved | R-01, R-09, R-18 |
| 5 Immutable truth + projections | Holds; head contract added | R-07, R-10, R-23 |
| 6 Identity | Holds; ClaimId rule pinned | R-12, R-22 |
| 7 Size efficiency | Violated-in-MVP → corrected (prune, granularity) | R-08, R-12 |
| 7(bis) Inspection UX | Holds; frame layout specified | R-23 |
| 8 UX hides machinery | Holds; safe mode + failure messages added | R-01, R-27 |
| 9 Cache cannot alter semantics | Ambiguous → guards added | R-05 |
| 10 Capability-scoped agent code | Violated-in-effect (grant establishment, caller, visibility) → corrected | R-13, R-14, R-05, R-16 |
| 11 Effect-free retention | Ambiguous → kernel replay bit + fail-closed | R-04 |
| 12 Memory three layers | Holds; schema/pipeline corrected | R-11, R-12 |
| 13 No deterministic replay | Holds | — |
| 14 Perpetual cognition bounded | Ambiguous → breakers + step model | R-17, R-18 |

**Traces:** Session creation (B primary, A challenger — Produced/Validated) · Perpetual wake (E primary, H challenger — Produced/Validated) · Model call (E/D — Produced/Validated) · Tool effect (D primary, C challenger — Produced/Validated) · Module activation (C primary, A challenger — Produced/Validated) · Dynamic child scope (C — Produced) · Project-memory promotion (F primary, B challenger — Produced/Validated) · Module-state update (B primary, C challenger — Produced/Validated) · Distributed UI (G primary, C challenger — Produced/Validated) · Continue from checkpoint (E primary, B challenger — Produced/Validated) · Audit reconstruction (B primary, I challenger — Produced/Validated). All 11 traces covered; none deferred unowned.

**20 explicit questions:** all Answered or validly Not applicable/Deferred across the nine reviews; no question left unowned. Question 18 (UI in MVP) resolves to "No as specified — deferred per R-20." Question 20: no further justified simplification found beyond the accepted packet (multiple reviewers converged on this independently).

**Review-gate artifacts:** reviewed constitution (packet pending) · subsystem specs (packet items 2–7) · glossary (editorial) · kernel API inventory (A's 12-group surface — accepted as the working kernel contract) · module/service/registry inventory (C) · canonical event/object schemas (packet) · state machines and sequence diagrams (C/E/F/G produced; committed to ledger) · storage growth and crash matrices (B/H/D/E/F produced) · threat and authority model (D produced) · persistence/reconstruction model (B produced) · MVP dependency graph (H produced) · spike plan (this register) · accepted tradeoff register (R-29) · explicit non-goals (R-20/R-26 additions) · final verdict record (this document) · implementation authorization (**withheld**).

## Final disposition

**Revise and re-review.** The bounded change packet (section above) goes to the design lead; on completion, re-review runs from the packet diff per `handoff:1194` — reviewers receive prior finding IDs, exact document diffs, disposition rationale, and unresolved questions. S1 and S2 spikes may start immediately (disposable). No production code, scaffolding, or dependency commitment until the packet re-review and the integrated re-review grant explicit implementation authorization.

**Applied: 2026-08-28.** The accepted packet above was applied to all three documents (constitution, ledger, handoff); pre-edit originals are preserved in `~/dev/.agent-harness-backup-20260828/`. The re-review input is the diff of the three documents against that backup. Next step per this record: bounded re-review of the change packet, with S1 (Wasm-hosted Luaur) and S2 (session-actor throughput) spikes started in parallel.

Per-reviewer full texts (A–I, including H's recovered tail) were produced in-session; this record is the durable artifact. Clusters R-01–R-29 preserve all 70+ source finding IDs.

## Second review pass (bounded re-review) — 2026-08-28

Second pass ran per `handoff:1194` from the packet diff (current docs vs `~/dev/.agent-harness-backup-20260828/`), via five lane reviewers + one integrated adversarial reviewer. Verdict on the first application: **Revise and re-review** — direction sound, but the handoff was largely missed and several ledger corrections were incomplete or self-contradictory.

Gaps found (all now closed):

- G1 (high): handoff never received the three-tier kernel boundary; diagram + kernel-responsibility list stayed single-tier; still listed `message` as a branded ID. → Applied R-19 three tiers, glossary, and `message` identity fix to handoff.
- G2 (high): handoff scope statements still listed dense retrieval, distributed multi-module UI, and the upcaster framework as in-scope; ledger one-pager "Not doing initially" omitted the upcaster-machinery deferral. → Applied deferrals to handoff scope + one-pager.
- G3 (high): R-27 UI contract absent from all docs. → Added fault classes, `UiEvent` provenance, module-intent capability intersection, and kernel-reserved interactions to ledger UI section.
- G4 (high): R-24 Wasm hardening parameters (store limits, epoch deadlines + host-import timeout wrappers, per-generation wall-clock budget) missing. → Added to ledger one-process containment.
- G5 (medium): R-21 per-milestone acceptance matrix described but never published. → Published bullet→milestone and 15-test→milestone matrices.
- G6 (high): retrieval pipeline self-contradictory (dense step live, pre-fusion filter listed after fusion); `evidence_for` rename not propagated into the edge code block. → Reordered pipeline, removed live dense step, renamed code block to `MVP claim-DAG edges` with `evidence_for`.
- G7 (medium): milestone count drift (constitution 7 vs ledger one-pager "eight"; dangling `M8`). → Corrected to seven milestones and M7 dogfooding gate.
- G8 (medium): glossary editorial correction (four snapshot kinds, four context meanings, "composed" terminology) never applied. → Added `## Shared terminology (glossary)` to handoff.
- G9 (low): R-09 trailing clause "child spawn routes through the tool intent pipeline" dropped. → Added to ledger cognition section.
- Also closed a new R-12 finding: `Retracted` status was unreachable under the six-edge vocabulary; clarified retraction = `supersedes` without a successor.

Rejected minority findings 2 (dense retrieval in MVP) and 3 (multi-module UI in MVP) had been reintroduced in the handoff only; both now corrected. The other four rejected findings remain absent.

Status after second pass: packet reapplied; a third (verification-only) re-review of the re-application diff is the next gate before implementation authorization. S1/S2 spikes remain clear to start.

## Third review pass (verification-only) — 2026-08-28

Verification pass over the re-application diff. All nine second-pass gaps (G1–G9) confirmed closed, plus the two reintroduced rejected findings:

- G1 (three-tier boundary + message identity): three-tier naming present in constitution, ledger, and handoff (responsibility list + diagram). `message` removed from branded-ID lists; handoff identity line reads "message identity is its committing event (no separate `MessageId`)".
- G2 (deferrals): handoff scope statements now carry dense-retrieval / distributed-UI / upcaster / embeddings / vector-index deferrals; ledger one-pager "Not doing initially" includes the upcaster-machinery deferral.
- G3 (R-27 UI contract): three fault classes, `UiEvent` provenance, module-intent capability intersection, and kernel-reserved interactions present in ledger UI section.
- G4 (R-24 Wasm hardening): store limits, epoch deadlines + host-import timeout wrappers, and per-generation wall-clock budget present.
- G5 (R-21 matrix): bullet→milestone and 15-test→milestone matrices published.
- G6 (R-12 pipeline + edge): pipeline reordered (filter/dedup before fuse), live dense step removed, `MVP claim-DAG edges` uses `evidence_for`; no `5a` remnant, no `Run dense semantic search` anywhere.
- G7 (milestone drift): zero occurrences of "eight" in the three docs; seven-milestone wording consistent.
- G8 (glossary): `## Shared terminology (glossary)` present with four snapshot kinds and four context meanings.
- G9 (R-09 clause): "child spawn routes through the tool intent pipeline" present.
- Rejected findings 2/3: no in-MVP dense-retrieval or multi-module-UI framing remains in the handoff (only the infra non-goal "propose distributed systems" and the glossary's contrastive mention of "distributed" remain, both legitimate).

Verification verdict: **implementation authorization on the architecture (documents only) is granted.** The authorization covers document-level design; no production code, scaffolding, or dependency commitment begins until S1 (Wasm-hosted Luaur hosting) and S2 (session-actor throughput) spikes report back and ratify the provisional budget table and hosting fallback tree.
