# Kanbei requirements baseline

Status: resolved through research, brainstorming, and human grilling. This is the input to the technical specification, not an approved implementation design.

## Product target

Kanbei is a self-hosted, single-user, single-always-on-host agentic assistant. Its first vertical slice covers two selected projects plus Kanbei itself. Managed LLM and search APIs are allowed; the canonical state, orchestration, interface, and broker remain on the user's infrastructure.

The canonical acceptance journey is research to feature push: ingest a project, explore intent, approve a structural goal where required, execute nested deltas, reconcile external commits, add a useful tool/UI projection through self-modification, and preserve enough evidence for internal-state and presentation replay. World-state control, memory reconciliation, and self-modification must all be demonstrated at comparable depth.

The target repository label for later SDLC tracking is `smores56/kanbei`. Its code has not been inspected and must not be inspected under the current constraint. The only personal codebase inspected during research was `nix-config`.

## Architecture and authority

Use a truth-first canonical ledger with convergence, attention, Git, and security as supporting planes. Do not build a universal truth engine.

Human intent, approvals, amendments, vetoes, and curation inputs are canonical authority records. Observations are canonical records of captured source content, not automatically true statements about the world. Semantic memory is a rebuildable projection of canonical inputs and curation. Human promotion appends a linked human-attested assertion rather than certifying objective truth. Effect and activation receipts record attempted and observed effects; replay never repeats external effects.

Use six kernel contracts:

- Record: ECS-like append-only components.
- Effect: scoped runtime activation, disposal, and compensation boundaries.
- Projection: deterministic or explicitly asynchronous derived state with coverage watermarks.
- Capability: mediated filesystem, process, network, model, and secret operations.
- Loop: durable agent execution, subscriptions, and branches.
- View: renderer-neutral catalogs, artifacts, and curation gestures.

Records are canonical at rest. Loops are actor-like at runtime. Effects own resources, not authority. Projections own derived state. Capabilities own external authority. Views own presentation and are not canonical domain state.

## Record and state semantics

Use open, namespaced, independently versioned components. Only record identity, canonical recording time, and integrity fields are mandatory. Authority, actor, subject, parent/cause links, desired/observed role, lifecycle, validity, payload references, and domain semantics are optional typed components. Unknown components must survive round trips. Projections declare the component versions they understand.

Use one global recording sequence assigned by a single sequencer and explicit causal links for meaningful partial order. Do not require a global causal total order or distributed clocks.

Desired and observed state use separate optional linked components and different authority paths. No entity must have both. Link them with generation/coverage cursors where useful. Avoid mandatory Kubernetes-shaped objects.

Recording time is the only required semantic clock. Valid, observed, published, derived, and expiry times are optional typed components. Corrections and retractions append records; they do not mutate prior records.

Use a hash chain over canonical record bytes and predecessors, with checkpoint digests included in backups. Signatures are optional future work.

## Semantic memory and reconciliation

The current semantic graph is derived, versioned, and rebuildable. Agents may append identity merge, split, alias, contradiction, and other reconciliation proposals. Projections may apply them automatically only non-destructively: source identities remain stable and every operation is reversible by projection version.

A scoped resolver may prefer a working claim for action, but contested status and alternatives remain visible. Optional agent self-reported confidence is explicitly uncalibrated metadata and cannot affect authority or automatic promotion.

A retraction immediately excludes dependent projections at read time by marking them suspect, then triggers an asynchronous rebuild. Prior claims and derivation history remain inspectable. Reconciliation is incremental for affected subgraphs, with scheduled deeper audits and non-destructive evidence-preserving compaction.

Untrusted content creates observations and candidate claims, never authority records. Enforce memory scope at read time. Suspected poisoning quarantines affected subgraphs and triggers rebuilds.

Use arbitrary versioned disclosure projection DAGs with coverage, provenance, and staleness metadata. Do not impose fixed world/feature/task levels or conflate disclosure depth with work depth.

## Intent, approval, and work

Preserve exact human intent as an immutable anchor. Agents attach versioned interpretations citing source spans and assumptions. Exploration and low-stakes work may proceed speculatively; consequential branches require an accepted interpretation unless human-owned autonomy policy says otherwise.

Agents infer whether human chat is advisory, directive, or corrective. Original messages are canonical; inferred authority is derived and cannot bypass autonomy-tier approval.

Use human-owned declarative autonomy tiers: automatic, review-after, approval-required, or forbidden. Policy may consider operation, reversibility, blast radius, and effect/data class. Agents may propose classifications but cannot grant their own tier.

Present one global decision queue grouped by intent and deadline, deduplicating shared evidence and supporting individual or batch rulings. Approval binds to an exact interpretation/plan hash, subject, authorized scope, constraints, and expiry. Compliant child deltas may proceed without fresh approval.

Agents have full discretion to reassess and override failed approval constraints with a recorded rationale. Approval constraints therefore express delegated intent, not a hard security boundary. Sandbox, broker, and global resource policies are separate hard human-owned boundaries.

Nested work has one ownership parent and an arbitrary causal DAG. Completion requires evidence against acceptance criteria and required blockers; autonomy tier determines automatic, review-after, or approval-required promotion.

External changes are recorded immediately and reconciled autonomously without clobbering speculative history. Surface conflicts that change interpretation, acceptance, or prior decisions.

## Agent loops and durability

Use persistent self-triggering agent loops with event and timer wakeups, idle backoff, and policy-controlled concurrency. Do not require literal continuous model calls. Global spend, token, rate, and concurrency ceilings are hard; per-loop budgets are soft and support rationale-bearing overrides. Concrete limits are mandatory operator configuration informed by cost measurements.

Use ledger-backed canonical delegation, handoffs, decisions, and subscriptions. Direct streams are ephemeral optimizations whose outcomes are appended.

Implement durability natively in the ledger with idempotent inboxes, effect/result journals, checkpoints, branch heads, timers, and recovery. Do not add Temporal, Restate, or another second workflow authority.

External effects use at-least-once attempts, idempotency keys and deduplication where supported, and explicit `outcome_unknown` after ambiguous interruption. Never blindly claim exactly-once delivery or retry an unknown non-idempotent effect.

Record exact available model requests, responses, output events, parameters, and exposed reasoning artifacts. Hidden provider chain-of-thought is not promised.

## Replay and undo

Provide deterministic rebuild of operational projections and mutable plugin-internal state from canonical inputs and recorded effect results. Provide presentation replay of UI timelines. Do not replay external effects or promise bit-identical LLM regeneration.

Maintain an undo forest of agent execution and state branches. Undo creates a new branch from a recorded checkpoint and preserves exact available LLM I/O and all descendants. External compensation is a separate new effect.

Use dependency-aware branch sets for cross-agent undo. Compute affected descendants across the causal DAG and allow coordinated branches without silently assuming a consistent global cut.

## Self-modification

Initially allow live mutation of everything outside the frozen trust root, including arbitrary tools and plugins, as an experiment to be tightened if practice demonstrates unacceptable risk. Declarative context projections, UI, keybinds, themes, settings, tool wiring, and executable plugins all use versioned candidates.

The frozen trust root contains the canonical append/integrity path, sandbox supervisor, secret broker, global resource ceilings, receipt verification (including approval/tier/coverage checks), activation/rollback supervisor, audit sink, and a minimal read-only consent surface with its gesture endpoint. The consent surface renders canonical evidence, scope, and hashes independently of the mutable world UI. Agents may propose but cannot live-edit these components.

Use a bespoke TypeScript plugin kernel, borrowing Cordis and DeepSeek Harness patterns without depending on either. Do not let their APIs define Kanbei's domain model.

Activate mutable changes by staging each agent/plugin dependency scope, validating it, producing capability and dependency diffs, atomically flipping a scope pointer, running health checks, and retaining previous candidates for rollback. External effects are not rolled back by pointer flips.

Agents may edit declarative context filters, queries, budgets, summaries, and disclosure rules with next-turn activation. Executable projection code follows the same scoped candidate mechanism as other mutable code.

All canonical writes pass through the frozen append API. Mutable agents and plugins cannot open or mutate canonical SQLite directly. Mutable plugin state uses separate scoped stores.

Frozen-kernel changes require human-approved Nix generations. Roll back code only when data remains readable; otherwise roll forward or branch from a verified checkpoint. Do not run destructive automatic down migrations.

## Security and external effects

Assume hostile prompt injection with arbitrary agent/tool code, hostile dependencies, exfiltration attempts, sandbox escape attempts, and confused-deputy attacks. The host and human remain trusted.

Allow read/write access only to selected project roots, scratch space, and agent-mutable Kanbei paths. Secrets and the frozen trust root are outside the mutable write boundary. Canonical storage is not agent-writable.

Unrestricted outbound network is accepted. Consequently, the design does not protect project-data confidentiality from malicious code; injected code can exfiltrate anything readable. This is an accepted residual that must be explicit.

Inter-agent isolation is for crash and resource containment, not a required security boundary. Prefer lightweight process, WASM, or microVM mechanisms only when operationally cheap. A container per agent is excluded.

The broker maps enrolled logical hostnames to real endpoints and injects credentials without exposing them to agent code. It is not an action-authorization layer and does not constrain methods, paths, or payloads. Never forward credentials across hosts or redirects. Redact known credential values from responses and logs where possible.

Any credential may be enrolled. Documentation strongly recommends least-privilege provider credentials and states that agents can exercise their full provider-side authority. Enrollment is a one-time human action; subsequent use is autonomous within hard broker policy.

## Tools and providers

Expose one programmable execution surface with search/list/describe discovery for versioned functions and documentation. Agents compose host calls in code and receive structured results. Direct per-tool schemas are not the default context cost.

Select the agent-facing execution language through an identical-task bake-off covering Monty, TypeScript, Lua, Bash, and full CPython. Measure success, token use, latency, host-call behavior, snapshots, and operability inside the OS sandbox. Do not treat any in-process interpreter as the outer security boundary.

Use TypeScript for the main control plane. Evaluate Node and Deno in a spike; the OS sandbox remains mandatory unless the threat model changes. Deno permissions may be an inner boundary, not an assumed replacement.

Use provider-neutral model routing with human-owned task policies and exact I/O capture. Use TinyFish behind a provider-neutral search/fetch contract. Preserve raw provider requests and responses as evidence, normalizing without requiring equivalent capabilities across providers.

## Interface and multimodal state

Use a stable web shell with coordinated graph, delta timeline, and global decision views. Agents choose or generate contextual visualizations and chat inside that shell. Authoritative approval displays and gestures use the separate frozen minimal consent surface; mutable UI may deep-link to it but cannot supply its evidence or ruling payload. Do not build a TUI in the vertical slice; keep projections and catalogs renderer-neutral.

Keep canonical events and graph semantics internal. Adapt runs to AG-UI, render ordinary A2UI catalogs, add custom graph components, and isolate arbitrary plugin artifacts with an MCP Apps-compatible sandboxed boundary.

Human graph edits are curation gestures that append merge, split, link, scope, correction, retraction, and promotion records. Agents receive the same versioned structured view specifications as renderers, plus derived screenshots/accessibility trees when useful. Human interactions become records.

Content-address arbitrary files. Provide provenance, previews, and derived OCR/text/metadata for text, code, images, screenshots, and PDFs. Store audio/video without requiring full semantic indexing initially. Canonical raw objects remain distinct from derived transcripts, OCR, embeddings, and thumbnails.

## Persistence, freshness, and backup

Store all mutable Kanbei state under `~/.local/state/kanbei/`; trusted Nix configuration and secret material remain outside it. Add this path to the existing backup configuration when implementation lands.

Use SQLite for the canonical ledger and rebuildable SQL/FTS/graph indexes. Use one sequenced writer, JSON component payloads, and transactional metadata. Mutable plugins use separate scoped state.

Use a local OCI-layout whole-object CAS behind a replaceable `BlobStore` contract. Publish bytes before manifest and ledger visibility. Enforce per-object and total admission limits, backpressure, and safe orphan reclamation. Defer chunk-level dedup.

Operational projections—work DAG, approvals, raw observation metadata, active configuration, and ledger status—provide strong read-your-writes and commit atomically with the ledger. Cheap indexes may join this class after measurement. Semantic projections expose exact coverage and pending work rather than pretending to be current.

Retain the ledger indefinitely by default. Logical retraction is normal; physical erasure is an exceptional epoch-producing operation covering derived artifacts and backups as far as possible. Leave automated retention policies open for later.

Back up using an online SQLite point-in-time snapshot immediately before the normal filesystem backup. Copy that snapshot and immutable CAS files while GC is paused; normal ledger writes continue. Extra new blobs are harmless. Restore verifies every reference and rebuilds derived indexes.

Bind the UI to loopback and expose it through the existing Cloudflare Tunnel and Access conventions.

## Validation and deferred decisions

Use zero-tolerance gates for secret exposure, canonical ledger bypass, silent external clobber, effects outside hard policy, missing required provenance, and known-retracted claims presented as current. Measure reconciliation quality, runtime task success, cost, latency, and UI usefulness; set concrete quality thresholds after baseline spikes.

Defer these decisions to explicit spikes:

- Agent-facing execution language.
- Node versus Deno.
- Pure user-space deployment versus a dedicated-UID broker/sandbox helper.
- Lightweight inter-agent isolation mechanism.
- Concrete resource ceilings and quality thresholds.

The deployment spike must test user namespaces, same-UID process/ptrace isolation, Landlock, systemd user-manager directives, broker identity, resource controls, and recovery. A pure user-space model is desirable but cannot be assumed to protect broker secrets from same-UID malicious code.
