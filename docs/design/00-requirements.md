# Kanbei requirements baseline

Status: resolved through research, brainstorming, and human grilling. This is the input to the technical specification, not an approved implementation design.

## Product target

Kanbei is a self-hosted, single-user, always-thinking ledger agent. It runs in one NIC-less microVM on the owner's host, owns the RTX 3090, and performs all inference offline with llama.cpp and the owner's own weights. There is no managed LLM API and no general network path; the world reaches Kanbei only through six fixed channels (Inbox read-only, Vault read-only, Todo read-write size-capped, Chat over vsock, CSR over vsock, Search over vsock). Canonical state, orchestration, interface, and the enforcement boundary stay on the owner's infrastructure.

Kanbei's code enters the VM only through a human-merged `smores56/kanbei` commit pinned in nix-config's `flake.lock` and built by Nix into the VM image. Agents may open pull requests; no development worktree is ever mounted into the VM.

The canonical acceptance journey is life admin: ingest the owner's mail, calendar, finance, browser, photo metadata, phone data, and project changes; reconcile them into one inspectable ledger; advise on what needs attention; and maintain a human-owned todo list that Kanbei may read and write within a size cap. World-state control, memory reconciliation, and self-modification must all be demonstrated at comparable depth on that journey: one declarative change goes live, and one executable change waits for human approval before it activates.

The target repository is `smores56/kanbei`. This document set is the rebase of the pre-split requirements baseline onto the deployed split design (SDLC #509); where the two disagree, this rebase governs.

## Architecture and authority

Use a truth-first canonical ledger with convergence, attention, Git, and security as supporting planes. Do not build a universal truth engine.

Human intent, approvals, amendments, vetoes, and curation inputs are canonical authority records. Observations are canonical records of captured source content, not automatically true statements about the world. Semantic memory is a rebuildable projection of canonical inputs and curation. Human promotion appends a linked human-attested assertion rather than certifying objective truth. Effect and activation receipts record attempted and observed effects; replay never repeats external effects.

Use six kernel contracts:

- Record: ECS-like append-only components.
- Effect: scoped runtime activation, disposal, and compensation boundaries.
- Projection: deterministic or explicitly asynchronous derived state with coverage watermarks.
- Capability: mediated channel, model, filesystem, process, and secret operations.
- Loop: durable agent execution, subscriptions, and branches.
- View: renderer-neutral catalogs, artifacts, and curation gestures.

Records are canonical at rest. Loops are actor-like at runtime. Effects own resources, not authority. Projections own derived state. Capabilities own external authority. Views own presentation and are not canonical domain state.

Authority is layered by plane. The host (kernel, root-only VM artifacts, network plumbing) and the human sit outside Kanbei's trust boundary. Inside the VM the frozen trust root holds the canonical append path and sequencer, the six-channel host surface and its code, the Monty host-call boundary and sandbox supervisor, the global ceiling governor, receipt verification, the activation/rollback supervisor, the audit sink, the startup verifier, and the minimal consent surface. Everything else — agents, declarative views, and executable candidates — is mutable and untrusted.

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

Agents have full discretion to reassess and override failed approval constraints with a recorded rationale. Approval constraints therefore express delegated intent, not a hard security boundary. The VM boundary, the channel policy, and global resource ceilings are separate hard human-owned boundaries.

Nested work has one ownership parent and an arbitrary causal DAG. Completion requires evidence against acceptance criteria and required blockers; autonomy tier determines automatic, review-after, or approval-required promotion.

External changes are recorded immediately and reconciled autonomously without clobbering speculative history. Surface conflicts that change interpretation, acceptance, or prior decisions.

## Agent loops and durability

Use persistent self-triggering agent loops with event and timer wakeups, idle backoff, and policy-controlled concurrency. Do not require literal continuous model calls. GPU-lane occupancy, ingest volume, CAS growth, chat rate, and growth-bearing search budget ceilings are hard; per-loop budgets are soft and support rationale-bearing overrides. Concrete limits are mandatory operator configuration informed by measurement.

Use ledger-backed canonical delegation, handoffs, decisions, and subscriptions. Direct streams are ephemeral optimizations whose outcomes are appended.

Implement durability natively in the ledger with idempotent inboxes, effect/result journals, checkpoints, branch heads, timers, and recovery. Do not add Temporal, Restate, or another second workflow authority.

External effects use at-least-once attempts, idempotency keys and deduplication where supported, and explicit `outcome_unknown` after ambiguous interruption. Never blindly claim exactly-once delivery or retry an unknown non-idempotent effect.

Record exact available model requests, responses, output events, parameters, and exposed reasoning artifacts. Hidden provider chain-of-thought is not promised.

## Replay and undo

Provide deterministic rebuild of operational projections and mutable plugin-internal state from canonical inputs and recorded effect results. Provide presentation replay of UI timelines. Do not replay external effects or promise bit-identical LLM regeneration.

Maintain an undo forest of agent execution and state branches. Undo creates a new branch from a recorded checkpoint and preserves exact available LLM I/O and all descendants. External compensation is a separate new effect.

Use dependency-aware branch sets for cross-agent undo. Compute affected descendants across the causal DAG and allow coordinated branches without silently assuming a consistent global cut.

## Self-modification

Declarative and executable changes follow different paths.

Declarative changes — context filters, queries, budgets, summaries, disclosure rules, view catalogs, themes, keybinds, and settings — activate live at the next turn boundary, with the running turn pinned to the version it started with. They are versioned documents validated for schema, budget bounds, authorized sources, unknown component versions, total disclosure rules, no presentation of suspect or retracted records as current, and explicit fallback.

Executable changes — arbitrary tools, plugins, and executable projection code — never activate without a human. They are staged as immutable candidates carrying a dependency lock, a capability and dependency diff, a sandboxed dynamic conformance run under Monty with external capabilities denied, a state-migration dry run on shadow state, and a health plan. The candidate then waits in the decision queue for an approval receipt binding the candidate digest, capability diff, scope, and expiry. Approval flips one scope pointer atomically, runs health checks, and retains the previous candidate for rollback. An agent cannot approve its own candidate, and a candidate cannot remove the approval affordance.

The frozen trust root contains the canonical append/integrity path and single sequencer, the six-channel host surface and its code (Inbox reader, Vault reader, Todo writer, Chat server, CSR handler, Search client), the Monty host-call boundary and the sandbox supervisor, the global resource ceilings and their governor, receipt verification (including approval/tier/coverage checks), the activation/rollback supervisor, the audit sink, the startup verifier that records the kernel generation, and a minimal read-only consent surface with its gesture endpoint. The consent surface renders canonical evidence, scope, and hashes independently of agent-authored views. Agents may propose but cannot live-edit these components; they change only through a human-merged commit built into a new image.

Use a bespoke TypeScript plugin kernel, borrowing Cordis and DeepSeek Harness patterns without depending on either. Do not let their APIs define Kanbei's domain model. Agent-authored code runs under Monty, a Python-subset interpreter with no ambient filesystem, network, or subprocess access; host calls are explicit asynchronous stubs. Monty is a language-level boundary inside the VM and is never described as the security boundary: the VM and its channel set are.

All canonical writes pass through the frozen append API. Mutable agents and plugins cannot open or mutate canonical SQLite directly. Mutable plugin state uses separate scoped stores under `/var/lib/kanbei/`.

Frozen-root changes require human-approved Nix generations built from the pinned commit. Roll back code only when data remains readable; otherwise roll forward or branch from a verified checkpoint. Do not run destructive automatic down migrations.

## Security and external effects

Assume hostile prompt injection with arbitrary agent and tool code, hostile dependencies, exfiltration attempts, and confused-deputy behavior in every ingest batch. The host and the human remain trusted; inside the VM, the agent process is the adversary.

The VM has no NIC, no user-mode networking, and no DNS. Its only device surface is the six channels. Kanbei therefore cannot exfiltrate except through the Search channel (capped, filtered, logged, and an accepted residual), the Chat channel (to the owner), the Todo channel (size-capped), or a VM escape.

Reads are limited to the channel mounts: Inbox and Vault read-only, Todo read-write within its cap, plus per-agent scratch and agent-mutable state inside the VM. The frozen trust root and canonical storage are outside the mutable write boundary.

The Kanbei VM holds no provider credentials at all. The model is local and the only TinyFish key lives in the Search VM. Channel policy is the boundary; there is no credential-injection path, no broker, and no general-purpose HTTP client inside the VM. Credentialed work happens in the per-source ingest VMs, which hold only their own source's credentials, parse that source's hostile input, and emit age-encrypted batches to a host writer that the VM cannot write.

Inter-agent isolation inside the VM is for crash and resource containment, not a security boundary. Monty bounds one program's time and memory; the VM contains the process tree.

The capability set an agent can invoke is closed and derived from the channel manifest. A sandbox that starts with a weaker posture than declared is a start failure, not a runtime downgrade.

## Tools and providers

Expose one programmable execution surface with search/list/describe discovery for versioned functions and documentation. Agents compose host calls in code and receive structured results. Direct per-tool schemas are not the default context cost.

Agent-authored code runs under Monty, fixed by decision: the same runtime for every agent, with host calls generated as typed stubs and no ambient capability. The control plane is TypeScript on Deno, fixed by decision; Deno permissions are an inner policy layer and the VM boundary is mandatory.

Model routing is a single local provider behind a provider-neutral request/response contract: llama.cpp on the 3090 serving the owner's model, with human-owned task policies mapping task classes to lanes. Preserve exact post-redaction request/response evidence and record reasoning availability; there is no remote model, no fallback chain, and no provider credential.

Search uses the provider-neutral search and fetch contract with TinyFish as the only adapter, called from the Search VM over the Search channel. A hit is a locator, not a capture; raw provider envelopes are evidence. Reported limits are applied as backpressure before the call.

## Interface and multimodal state

Serve a static, human-owned web shell built by Nix from the pinned commit. Its CSP limits `connect-src` to the Kanbei API, `img-src` to `'self'` and `data:`, and `form-action` to `'self'`; there is no inline script, no remote font, and no remote media. Kanbei returns JSON only, and model text and declarative views render with no remote resources and no auto-linking, so a prompt-injected Kanbei cannot use the owner's browser as an exfiltration channel.

The shell carries coordinated graph, delta timeline, and global decision views. Agents may supply declarative view catalogs, themes, and contextual components; they cannot add code, new origins, remote references, or remove an anchor or decision affordance. Authoritative approval displays use the separate frozen consent surface, which the shell may deep-link to but cannot modify or supply evidence to. Do not build a TUI in the vertical slice; keep projections and catalogs renderer-neutral.

Keep canonical events and graph semantics internal. Adapt runs to AG-UI, render ordinary A2UI catalogs, add custom graph components, and isolate arbitrary artifacts inside the CSP-locked shell.

Human graph edits are curation gestures that append merge, split, link, scope, correction, retraction, and promotion records. Agents receive the same versioned structured view specifications as renderers. Human interactions become records.

Content-address arbitrary files. Provide provenance, previews, and derived OCR/text/metadata for text, code, images, screenshots, and PDFs. Store audio/video without requiring full semantic indexing initially. Canonical raw objects remain distinct from derived transcripts, OCR, embeddings, and thumbnails.

## Persistence, freshness, and backup

Store all mutable Kanbei state on the in-guest LUKS volume under `/var/lib/kanbei/`. The kernel, static UI, model weights, and fixtures are image content: Nix-built from the pinned commit, read-only at runtime, and never writable by agent code. No secret material is stored in the VM.

Use SQLite for the canonical ledger and rebuildable SQL/FTS/graph indexes. Use one sequenced writer, JSON component payloads, and transactional metadata. Mutable plugins use separate scoped state.

Use a local OCI-layout whole-object CAS behind a replaceable `BlobStore` contract. Publish bytes before manifest and ledger visibility. Enforce per-object and total admission limits, backpressure, and safe orphan reclamation. Defer chunk-level dedup. CAS growth is a hard ceiling and a freshness signal, not an unbounded consequence of ingest.

Operational projections — work DAG, approvals, raw observation metadata, active configuration, source heartbeats, and ledger status — provide strong read-your-writes and commit atomically with the ledger. Cheap indexes may join this class after measurement. Semantic projections expose exact coverage and pending work rather than pretending to be current.

Every source carries a freshness heartbeat: the last batch sequence, batch time, and observed coverage. The UI labels a source stale past its window, and the host raises an alert when a heartbeat expires. Staleness is visible, never silent.

Retain the ledger indefinitely by default. Logical retraction is normal; physical erasure is an exceptional epoch-producing operation covering derived artifacts and backups as far as possible. Leave automated retention policies open for later.

Back up the guest as an encrypted image. The disk is LUKS sealed to the VM's swtpm vTPM with an offline recovery key shown once; the host snapshots the frozen image ciphertext nightly and restic copies it to `/var/backup` and R2. Restore reimages the VM, unlocks with the vTPM or the recovery key, verifies the ledger checkpoint and every CAS reference, and rebuilds derived indexes. An online SQLite snapshot is taken before the image snapshot so the image contains a consistent ledger.

`kanbei.sammohr.dev` is a Cloudflare private hostname route reachable only from devices running the WARP client. Gateway TLS decryption stays off and the Cloudflare root CA is never installed; TLS terminates in the VM over the Chat channel. A CAA record with `accounturi` limits who can issue certificates for the domain.

## Validation and deferred decisions

Use zero-tolerance gates for secret exposure, canonical ledger bypass, silent external clobber, effects outside hard policy, missing required provenance, known-retracted claims presented as current, approval-binding violations, trust-root mutation, channel leaks, remote requests from the UI shell, Todo cap violations, and stale heartbeats presented as current. Measure reconciliation quality, life-admin task success, lane latency, ingest freshness, and UI usefulness; set concrete quality thresholds after baseline spikes.

A channel leak is any path that moves bytes outside the six channels: a NIC, a user-mode network, a DNS lookup, a host filesystem mount outside the declared classes, a virtiofs share not in the manifest, or an undeclared vsock port. It is a release-blocking defect, not a residual.

Defer these decisions to explicit spikes:

- Concrete resource ceilings: GPU lane occupancy, ingest volume, CAS growth, Todo bytes, chat rate, and search budget.
- Freshness windows per source and the alert threshold.
- DLP patterns and the judge's block rate for the Search channel.
- What the vTPM seal binds to under microvm.nix direct kernel boot.
- Exact Firecracker vsock and read-only secret-block-device behavior under microvm.nix.
- A2UI/AG-UI exact pins and multi-decision interrupt grouping.

Placement, isolation, the agent-facing language, the TypeScript runtime, and the deployment plane are no longer open: Monty under a NIC-less VM with a Deno control plane, recorded in `08-implementation-plan.md`.
