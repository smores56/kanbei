# M4 Report — Context and memory (cache-aware projection, experience/activation/claim split, project identity, immutable memory DAG, root-transition CAS, root approval, exact-entity + FTS5/BM25 retrieval, memory tools, child runs)

Date: 2026-08-30. Milestone gate: **GREEN** — 338 tests (56 suites) / 28 s warm, clippy
`-D warnings` clean on the full workspace, crash matrix over the memory-root transition
and head-update points green. Memory usefulness probes pre-registered
(docs/memory-probes.md).

## Deliverables

New crates (17 → 20):

- **kanbei-memory** — the durable claim/provenance DAG (architecture.md §Memory,
  R-11/R-12): immutable content-addressed `Claim`/`ClaimEdge` objects (six-edge
  vocabulary `evidence_for | supports | contradicts | supersedes | promoted_from |
  applies_to`; `supersedes` without successor = retraction), `RootManifest` deltas with
  an explicit parent edge (R-12/M-09, current set = projection-time fold), the narrow
  per-scope `transitions.jsonl.zst` + atomic `head.json` + `objects/` layout under XDG
  state, and the single-writer **CAS actor** (`MemoryRootActor`): expected-old-root
  verification, idempotency keys (origin session/event + decision digest, R-11), origin
  verification (typed root-approval event kind + decision digest, R-11/M-12),
  refs-to-committed + acyclicity validation (R-12/M-01), head repair from the log,
  torn-tail recovery, failed-CAS orphans, and `ProjectRegistry` (ProjectId locator,
  canonical `projects.jsonl`). Validation status is derived from canonical data only
  (Proposed/Approved/Active/Superseded/Retracted, R-12/M-07); wall-clock fields are
  display/heuristic only (R-12/M-08).
- **kanbei-context** — the cache-aware typed projection pipeline (architecture.md
  §History and context projection, R-05): fragments with stability classes
  (Static/ScopeStable/SessionStable/TurnVolatile), semantic ordering, content/dependency
  hashes, sensitivity, and cache eligibility; the kernel-owned mandatory authority
  filter before any replaceable stage (E-03); the stage seam (config may replace stages
  where types permit); built-in Trajectory/Cognitive/Evidence/Memory/Compression/Budget
  stages; the kernel-owned final validator (authority re-check E-03, sensitivity
  non-escalation E-14, chronology A-06, opaque-artifact rejection E-07, token limits);
  longest-legal-stable-prefix lowering with the suppression ban (E-05) and
  `CachePlan::StablePrefix`; `ReasoningContinuity` (E-07) and `CompactionSelection`
  (E-06) types.
- **kanbei-retrieval** — SQLite adjacency + FTS5/BM25 (architecture.md §Memory,
  steps 1-9): deterministic exact-entity extractors (paths/symbols/commits/errors/
  tickets), external-content FTS5 with bm25() ranking and a documented LIKE fallback,
  validity/supersession filtering with contradiction annotation (R-12/M-04),
  authority ordering + lineage-union dedup (R-12/M-02/M-07), entity-boost fusion,
  bounded one-hop expansion (R-12/M-03), evidence rerank; disposable
  `activation_log` + deterministic versioned salience projector
  (`salience-v1`: causal recency, repeated use, unresolved goals, pins, bounded graph
  reachability — R-12/F-S5).

Session integration (kanbei-session):

- Memory wiring at open: lifetime + project actors, `project_bound` canonical event,
  index build, **backlink recovery** (accepted-but-unbacked transitions appended
  idempotently by TransitionId — R-11), compacted-range recovery.
- **Memory tools** (memory.query / memory.propose) activated through the tool FSM
  (B-05): proposals are canonical `memory_proposal` events; approval-gated root
  approval (`memory_root_approved` origin event) drives the CAS transition; rebase ≤3
  with `promotion_deferred` + `memory_orphans_expected` facts on exhaustion; backlinks
  committed after acceptance. Supersede support (edge + retraction in a second
  transition, acyclicity requires the successor to pre-exist). Child runs query with
  attenuated scopes (project-only reads, no lifetime — architecture.md §Memory 463-464).
- **Child runs** (R-09): `child.spawn` routes through the tool FSM; the session spawns
  a bounded `RunKind::Child` run (kernel-clamped budgets, no nesting), a fresh provider
  from a config-supplied factory, canonical run_start/run_outcome records, ChildDone
  wake observation, and parent children-budget enforcement.
- **Cache-aware projection** in the default render (`project_context`): full pipeline
  (harness contract → schemas → memory → conversation prefix → active memory/evidence
  → trigger) → validator → lowering; `model_call` records projection digest + cache
  plan/outcome (Hit / Miss / Invalidated{memory root changed}) + `ReasoningContinuity`
  (mandatory Broken on provider change, E-07); execution manifests pin exact
  project/lifetime memory roots (schema 4: `project_memory_root`).
- Compaction FSM rule (E-06): the session rejects events whose payload re-declares a
  fragment covered by a `compaction_selected` range.

## Gate

- **Crash matrix** (M4 acceptance bullet "memory-root transition / head update"): 6
  points — Before/AfterMemoryProposal (session) + Before/AfterTransition,
  Before/AfterHeadUpdate (actor) — all SIGABRT; `verify_m4_recovery` reopens, asserts
  head repair, transition/backlink consistency (exactly one backlink per transition,
  idempotent across repeated reopens), B-05 intent classification, and index rebuild.
- **Project-memory root CAS handles concurrent sessions deterministically**: two
  sessions sharing a scope — stale expected root → CasFailed → rebase over the winner
  → both claims folded; final fold identical regardless of commit order.
- Consistency tests: 3, 5, 6, 7, 11, 12, 15 exercised (15 re-verified untouched by the
  memory substrate); 9 tests in gate_m4.rs.
- Workspace: 338 passed (56 suites), 27.8 s warm; clippy `-D warnings` clean on all
  crates (incl. the wasm guest path; the spike's 5 pre-existing lints under rustc 1.98
  fixed minimally — no behavior change).

## Scope notes / deferrals

- Dense retrieval (R-20), generic spreading activation, always-on PageRank, and
  community summaries are deferred per architecture (retrieval benchmarks gate them).
- Lifetime promotion stays user-gated through the approval path; branch
  `MemoryFollowPolicy` (R-11/E-04) lands with `continue_from` (M6); compaction
  selection events exist canonically but no summarizer produces them yet (M5/M6).
- Automatic DAG-object GC remains disabled (M7 concern).
- Child providers are harness-supplied (config `child_provider` factory); the parent
  run's provider remains caller-supplied per M3.
