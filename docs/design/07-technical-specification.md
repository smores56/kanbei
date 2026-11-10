# Kanbei Technical Specification

Status: consolidated design specification derived from the requirements baseline, the state/memory, agent-execution, plugin-runtime, security/deployment, and interface/ingestion drafts, the research synthesis, and the architecture recommendation. It is the implementation contract to be refined by the deferred spikes named below.

Normative language: **MUST**, **MUST NOT**, **SHOULD**, **SHOULD NOT**, and **MAY** are used as defined by RFC 2119. A **spike** is an explicit experiment whose result selects an option; this document does not silently choose among spike options.

---

## 1. Summary

Kanbei is a self-hosted, single-user, single-always-on-host agentic assistant. Its first vertical slice covers two selected projects plus Kanbei itself. Managed LLM and search APIs are allowed; canonical state, orchestration, interface, and broker remain on the user's infrastructure. The canonical journey is research-to-feature push: ingest a project, explore intent, approve a structural goal where required, execute nested deltas, reconcile external commits, add a useful tool or UI projection through self-modification, and preserve enough evidence for internal-state and presentation replay.

Kanbei uses a truth-first canonical ledger with convergence, attention, Git, and security as supporting planes. It does not build a universal truth engine. Human intent, approvals, amendments, vetoes, curation, and promotion are canonical authority records. Observations are canonical records of what a source appeared to contain, not automatically true statements about the world. Semantic memory is a rebuildable projection of canonical inputs and curation. Effect and activation receipts record attempted and observed effects; replay never repeats external effects.

The architecture is organized around six kernel contracts: **Record**, **Effect**, **Projection**, **Capability**, **Loop**, and **View**. Records are canonical at rest, loops are actor-like at runtime, effects own resources but not authority, projections own derived state, capabilities own external authority, and views own presentation without owning canonical domain state.

The design deliberately preserves these human decisions:

- Open, namespaced, independently versioned ECS-like components, with only record identity, recording time, and integrity mandatory.
- One global recording sequence plus explicit causal links; no global causal total order and no distributed clocks.
- Semantic memory is derived, versioned, rebuildable, and non-authoritative.
- Agents have discretion to override failed approval **constraints** with a recorded rationale, while sandbox, broker, and global resource ceilings remain hard human-owned boundaries.
- Arbitrary tools and plugins may be activated live outside the frozen trust root as an experiment, to be tightened if practice shows unacceptable risk.
- Outbound network is unrestricted; the resulting exfiltration residual is accepted and explicit.
- Any credential may be enrolled, with documented least-privilege guidance; agents can exercise the credential's full provider-side authority.
- Durability is native to the ledger; there is no second workflow authority such as Temporal or Restate.
- Canonical storage is local SQLite; blobs use a local OCI-layout whole-object content-addressed store.

### 1.1 Terms

| Term | Meaning |
|---|---|
| **Canonical record** | An immutable, hash-chained ledger entry carrying a global sequence number and typed components. Its payload types carry no mutable lifecycle field; lifecycle is an operational projection over appended lifecycle records. |
| **Ledger** | The append-only canonical record store plus its single sequencer, integrity metadata, epochs, and checkpoints. |
| **Epoch** | A hash-chain lineage. Normal operation has one epoch; physical erasure starts a new epoch and closes the prior lineage. |
| **Sequence (`seq`)** | Global recording order assigned only by the ledger sequencer. It orders recording, not causation. |
| **Component** | A typed, versioned payload attached to a record under a namespaced component type. |
| **Projection** | Derived state computed from canonical records. Operational projections are synchronous and read-your-writes; semantic projections are asynchronous and expose coverage. |
| **Operational projection** | Work DAG, decision queue, observation metadata, active configuration pointer, ledger status, retraction closure, quarantine scope, and similar read-critical state committed atomically with the ledger. |
| **Semantic projection** | Entity graph, claims, aliases, contradictions, disclosure nodes, embeddings, and other derived interpretation state. It is rebuildable and never canonical authority. |
| **Effect** | An attempted external action with a durable attempt/result journal and a canonical receipt. Effects own resources, not authority. |
| **Receipt** | A canonical record of an attempted or observed effect, activation, rollback, approval, or denial. |
| **Capability** | A mediated grant for filesystem, process, network, model, secret, canonical-append, plugin-state, tool, view, or loop operations. |
| **Loop** | A durable, self-triggering agent execution entity with wakeups, subscriptions, budgets, checkpoints, and branches. |
| **View** | A renderer-neutral catalog, artifact, or presentation projection. Views own presentation, never canonical domain state. |
| **Intent anchor** | The immutable human statement of what the human wants, including exact source spans. |
| **Interpretation** | A versioned, source-linked agent or human reading of one or more intent anchors, with assumptions, scope, plan, acceptance criteria, and a content hash. |
| **Autonomy tier** | Human-owned policy classification: `automatic`, `review_after`, `approval_required`, or `forbidden`. |
| **Approval receipt** | A human-authority record binding a ruling to an intent anchor, interpretation hash, plan hash, subject, server-issued evidence-set hash, scope, effect class, constraints, and expiry. |
| **Domain scope** | The canonical `DomainScope` value describing project, path, effect-class, and data-class extent, with the containment relation defined in §4.2.1. "Scope" without qualification means `DomainScope`. |
| **Isolation scope** | The canonical `IsolationScope` identifier for a process/state partition (`agent:<id>`, `plugin:<id>@<instance>`, `project:<id>`). It is not a domain scope and has no containment relation. |
| **Lifecycle projection** | An operational projection that exposes the current state of a canonical subject (work item, interpretation, approval request, receipt, divergence, override, loop, wakeup, timer, enrollment) from appended lifecycle records. It is derived and rebuildable; no canonical payload carries `state`. |
| **Append intent** | A typed request to the frozen append API: `append`, `link`, `transition`, `retract`, or `quarantine`. Each carries an authenticated actor and cause/parent references. |
| **Effect receipt** | The single canonical envelope for an attempted or observed external effect. `Effect`, `EffectReceipt`, and `EffectOutcome` shapes in this document are views over it (§4.7.5). |
| **Evidence envelope** | An immutable validation artifact binding a test outcome to the canonical head, fixtures, environment, seeds, and model recordings (§7.0). |
| **Constraint override** | An agent record that relaxes a failed approval constraint inside a declared constraint lattice, without changing binding hashes, scope, effect class, tier, or hard boundaries. |
| **Divergence** | A recorded difference between an external change and a work basis, classified for autonomous reconciliation or human decision. |
| **Trust root** | The frozen, Nix-built components agents cannot live-edit: canonical append/integrity path and sequencer, sandbox supervisor, secret broker, global resource ceilings, receipt verification, activation/rollback supervisor, and audit sink. **Receipt verification** means the whole frozen admission-verification path: canonical effect-receipt verification, approval-binding verification (including server-issued evidence hashes), autonomy-tier resolution, and receipt-coverage evaluation. This is a clarification of the frozen role, not an expansion of the trust root. |
| **Source / observation / coverage** | A configured external resource; a canonical capture or status record about that source; and a receipt describing what a scan covered and what it did not. |
| **Blob / CAS** | Immutable content-addressed bytes stored in the local OCI-layout content-addressed store and referenced by digest. |
| **Working claim / contested / suspect / quarantine** | A resolver-selected claim for action; a claim with visible alternatives; a projection row excluded from current reads because an input was retracted; and a security-scoped exclusion of an affected subgraph after suspected poisoning. |
| **Human / agent / plugin / system principal** | An authenticated human; an agent loop or tool actor; mutable plugin code; or a frozen kernel component. |
| **Spike** | An explicit experiment; its result selects a deferred option. This document specifies provisional behavior until the spike reports. |

---

## 2. Problem

One person needs an inspectable, continuously reconciled model of what is true, what should change, and what agents are doing, while allowing the harness to reshape itself within explicit external-effect boundaries.

The system must ingest heterogeneous project and web state, preserve exact human intent and evidence, let agents explore and execute nested work, reconcile external commits without clobbering speculative history, maintain useful derived memory without pretending that memory is truth, and allow live self-modification without letting agents manufacture consent or edit the enforcement boundary.

The operating environment is hostile to naive agent designs:

- Prompt-injected agents may run arbitrary code, invoke hostile dependencies, attempt exfiltration, attempt sandbox escape, and act as confused deputies.
- Provider APIs and the network are semi-trusted and reached with real credentials.
- Agents may write files, spawn processes, open sockets, and mutate their own context, UI, tools, and plugins.
- Human attention is scarce; every approval mechanism must avoid fatigue without weakening hard boundaries.
- External projects change independently of Kanbei, including force-pushes and history rewrites.
- Semantic reconciliation is error-prone; entity merges and claims must remain inspectable and reversible.

The host kernel, the frozen trust root, the human, and the audit sink are trusted. The agent process itself is the adversary.

---

## 3. Goals / Non-goals

### 3.1 Goals

1. **Canonical evidence with narrow authority.** Preserve human intent, observations, proposals, effects, and derivations so the system can answer why it believes something, who decided it, and what changed.
2. **Rebuildable semantic memory.** Provide useful derived entities, claims, conflicts, and disclosure projections while keeping every source identity stable and every reconciliation operation reversible by projection version.
3. **Intent-to-work execution.** Turn exact human intent into versioned interpretations, scoped approvals, nested work, evidence-gated completion, and autonomous reconciliation of external changes.
4. **Durable agent loops.** Run persistent, event- and timer-driven loops with idle backoff, policy concurrency, hard global ceilings, soft per-loop budgets, journaled model I/O, and deterministic recovery.
5. **Safe-enough external effects.** Use at-least-once attempts, idempotency keys, durable journals, `outcome_unknown`, and explicit compensation rather than claiming exactly-once delivery.
6. **Live self-modification outside the trust root.** Allow agents to stage and activate declarative context, UI, keybinds, themes, settings, tool wiring, arbitrary tools, and executable plugins through versioned candidates, capability diffs, atomic scope flips, health checks, and rollback.
7. **Hard security boundaries.** Keep credentials out of agent address spaces, canonical writes behind the frozen append API, writable roots constrained, and global ceilings human-owned.
8. **Stable presentation.** Provide a stable web shell with coordinated graph, delta timeline, and global decision views; keep projections renderer-neutral; adapt to AG-UI, A2UI, and MCP Apps without making them domain authorities.
9. **Operational durability.** Store canonical state in local SQLite, blobs in a local OCI-layout CAS, back up consistently, restore verifiably, and retain the ledger by default.
10. **Measured quality.** Gate on zero-tolerance failures and measure reconciliation quality, task success, cost, latency, and UI usefulness after baseline spikes.

### 3.2 Non-goals

- Multi-user authority, multi-host canon, offline sync, or tenancy.
- Universal domain modeling or arbitrary source onboarding in the first slice. The source adapter set is closed: Git, HTTP resource, web search, and local filesystem.
- A universal theorem prover, adversarial court, category-theoretic delta algebra, attention market, or CRDT-based truth model.
- Full deterministic LLM regeneration or replay of external side effects.
- TUI parity in the vertical slice. Projections and catalogs SHOULD remain renderer-neutral where practical.
- Autonomous irreversible external effects such as payments, public posting, or force-push.
- Agent editability of the approval verifier, activation gate, broker policy, sandbox supervisor, global ceilings, audit sink, receipt verifier, or replay evaluator.
- Exactly-once external delivery.
- Guaranteed confidentiality of project data against malicious readable code, given unrestricted outbound network.
- Chunk-level blob deduplication, mandatory signatures, fixed retention schedules, or concrete numeric ceilings in this document. These are deferred or optional as stated below.

---

## 4. Proposed design

### 4.1 Overview

Kanbei is a set of cooperating planes over one canonical ledger:

```text
Human browser
    |
    | loopback + Cloudflare Tunnel/Access
    v
kanbei-ui (mutable presentation)  <--- View contracts ---  plugins
    |
    v
kanbei-core / frozen append API / query / projection host
    |
    +-- ledger.sqlite  (canonical records, operational projections)
    +-- projections/<name>.sqlite  (semantic projections, rebuildable)
    +-- cas/  (OCI-layout whole-object CAS)
    |
    +-- loop scheduler / wakeups / timers / checkpoints
    |       |
    |       v
    |   agent sandbox (untrusted code; OS sandbox)
    |       |
    |       +-- capability broker / secret broker / provider adapters
    |
    +-- activation supervisor / scope pointers / candidate store
    +-- audit sink
    +-- watchdog (external)
    +-- backup (external)
```

The ledger is the evidence plane. Convergence is a projection and controller concern: desired and observed state, generation cursors, divergence, and reconciliation work are derived from records. Attention is a projection and interaction concern: one global decision queue groups requests by intent and deadline, binds rulings to exact hashes, and supports individual or batch rulings. Git is an adapter and review surface, not the world database. Security is a cross-cutting boundary implemented by the sandbox supervisor, broker, capability checks, ceilings, and frozen append path.

#### 4.1.1 Six kernel contracts

| Contract | Owns | Canonical status | Primary rule |
|---|---|---|---|
| **Record** | Append-only typed components, identity, sequence, integrity | Canonical at rest | Every canonical byte passes through the frozen append API. |
| **Effect** | Scoped runtime activation, disposal, compensation | Receipts canonical; external action not replayable | Effects own resources and produce receipts, never authority. |
| **Projection** | Deterministic or explicitly asynchronous derived state | Derived; operational projections synchronous, semantic pending | Coverage and provenance are explicit; no projection pretends to be current. |
| **Capability** | Mediated filesystem, process, network, model, secret, and append operations | Grants, not truth | Capabilities are checked before external calls; broker and supervisor enforce. |
| **Loop** | Durable agent execution, subscriptions, branches | Actor-like at runtime; durable records canonical | Loops are the only contract that schedules work. |
| **View** | Renderer-neutral catalogs, artifacts, curation gestures | Presentation only | Views append gestures; they never write domain state directly. |

#### 4.1.2 Trust root and mutable surface

The **frozen trust root** MUST contain:

- the canonical append/integrity path and single sequencer;
- the sandbox supervisor;
- the secret broker;
- global resource ceilings and the ceiling governor;
- receipt verification, defined as the whole admission-verification path: effect-receipt verification, approval-binding and evidence-hash verification, autonomy-tier resolution, and receipt-coverage evaluation (`covers`), together with the frozen startup verifier that records `kernel_generation`;
- the activation/rollback supervisor;
- the audit sink;
- the minimal read-only consent surface and its human-gesture endpoint.

Receipt verification is frozen as one role. The approval-binding, tier-resolution, and coverage functions are internal to it and are reachable only through the frozen append/admission path; no mutable candidate may supply, replace, or observe a mutable copy of them.

The **minimal consent surface** is a frozen, read-only renderer plus its gesture endpoint. It MUST be origin-isolated from the mutable world UI: it serves from a separate host and origin behind the same verified human identity, issues a host-only `SameSite=Strict` session cookie, and shares no DOM, script context, or browser storage with `kanbei-ui`. It renders approval evidence, completion subject, granted tier, max effect class, reversibility, subject, scope, constraints, expiry, interpretation/plan/evidence hashes, and a `consentDigest` directly from canonical projections; it accepts no template, markup, evidence payload, styling, or session cookie from the mutable world UI. A mutable UI may deep-link to it by `requestId` only. At each render the surface mints a cryptographically random single-use short-lived consent token, stores only its hash bound to the human session, `requestId`, the full displayed-manifest digest, and expiry, and the mutable UI cannot read, mint, or recompute that token. This implements `req. 94` and `req. 132`; the consent surface is the only display authority for rulings (§4.5.5).

Mutable agents, plugins, tools, context projections, views, settings, themes, keybinds, declarative wiring, and arbitrary executable plugins live outside that set. Agents MAY create candidates for all mutable surface and MAY activate them through the candidate protocol. Agents MUST NOT live-edit the trust root. Trust-root changes require a human-approved Nix generation.

#### 4.1.3 Contract reconciliation rules

Earlier drafts used different names for the same concepts. This specification uses one vocabulary:

| Conflict | Resolution |
|---|---|
| Draft authority names such as `human`, `derived`, `effect`, `activation`, `proposal` versus the record draft's closed class list. | The canonical classes are `human_authority`, `delegated_authority`, `observation`, `candidate_claim`, `derived_proposal`, `effect_receipt`, `activation_receipt`, `security_action`, `coordination`, `lifecycle`, and `kernel_generation`. Plugin drafts map onto these; plugins cannot mint `human_authority`, `activation_receipt`, `effect_receipt`, `security_action`, `kernel_generation`, or stable-domain components. |
| Record drafts using `id`, `recordedAt`, `integrity`, and `authority` fields versus the canonical envelope. | The canonical envelope is defined in §4.2.1. A plugin `RecordDraft` is a request shape that the frozen append API validates and materializes into the canonical envelope. |
| `SeqTime` and `Hash` aliases in agent drafts. | `SeqTime` is an alias for `UtcUs`; `Hash` is an alias for `Digest`; both denote canonical recording time and SHA-256 digest unless a domain explicitly says otherwise. |
| Agent drafts listing a short work-state set versus the execution draft's detailed state machine. | The detailed work state machine in §4.4.3 is normative; the short list was an abbreviation. |
| Approval requests described as "mixed authority" versus receipts being human authority. | Requests are non-authority coordination records that any authorized principal may create. Only a human principal may append an approval receipt. |
| `net.direct` described as audit-only while the broker is a credential path. | Outbound network is unrestricted and audit-only for mediated operations; the broker is not an egress choke point. Both statements are true and are specified together in §4.10. |
| A2UI version claims and AG-UI interrupt semantics. | Interface adapters pin exact versions and declare them; AG-UI runs use one interrupt group at a time; exact pins and multi-decision grouping are spikes. See §4.11. |

The design does not silently reverse the unusual human decisions listed in §1. Where a draft implied a stronger guarantee than the requirements allow—such as exactly-once delivery, complete exfiltration prevention, or agent-editability of the approval verifier—this specification states the weaker guarantee and the residual.

---

### 4.2 Canonical records and ledger

#### 4.2.1 Record envelope

```ts
type RecordId = `rec_${string}`;      // opaque, globally unique, never reused
type EffectId = `eff_${string}`;      // effect identity minted by the journal at admission, not a RecordId
type EntityId = `ent_${string}`;      // stable subject identity
type EpochId = `ep_${string}`;
type Digest = `sha256:${string}`;     // text form: "sha256:" + 64 lowercase hex chars
type Sha256 = Digest;
type Iso = string;                    // RFC 3339 timestamp at an external/provider boundary
type UtcUs = number;                  // integer microseconds since Unix epoch, UTC
type Instant = UtcUs;                 // alias; every frozen-path timestamp is UtcUs
type Duration = number;               // integer microseconds; never fractional seconds
type DurationUs = Duration;
type Money = number;                  // integer USD micros (10^-6 USD); never float dollars
type UsdMicros = Money;
type HumanId = `hum_${string}`;
type AgentId = `agent_${string}`;
type PluginId = `plugin_${string}`;
type SystemPrincipalId = `sys_${string}`;
type Principal = HumanId | AgentId | PluginId | SystemPrincipalId;
type ComponentType = `kb.${string}/${string}@${number}.${number}.${number}`;

interface DomainScope {
  projectIds: string[];               // required; empty means "no project"
  paths?: string[];                   // absent means unconstrained (universal)
  effectClasses?: string[];           // interpreted in the policy-owned order (§4.5.4)
  dataClasses?: string[];             // absent means unconstrained (universal)
}

type IsolationScope = string;         // e.g. "agent:<id>", "plugin:<id>@<instance>", "project:<id>"

interface BlobManifest {
  contentDigest: Digest;              // SHA-256 over the exact immutable bytes
  size: number;
  mediaType: string;
}

interface BlobRef {
  contentDigest: Digest;
  manifestDigest: Digest;             // SHA-256 over canonical BlobManifest bytes
  mediaType?: string;
  size?: number;
}

type ContentRef = BlobRef;

interface Criterion {
  id: string;
  statement: string;
  subjectHash?: Digest;
  evidenceKinds?: string[];            // allowed EvidenceKind values; absent means any verified kind
}

type AcceptanceCriterion = Criterion; // alias; one type, one name in code

type ConstraintKind =
  | "path_prefix" | "path_allowlist" | "numeric_range" | "enum_allowlist"
  | "effect_class_max" | "data_class_allowlist" | "forbidden_operation";

interface ConstraintParams {
  path_prefix: { path: string; mode: "allow" | "deny" };
  path_allowlist: { paths: string[] };
  numeric_range: { field: string; min?: number; max?: number };
  enum_allowlist: { field: string; values: string[] };
  effect_class_max: { value: string };
  data_class_allowlist: { values: string[] };
  forbidden_operation: { operation: string };
}

interface Constraint<K extends ConstraintKind = ConstraintKind> {
  id: string;
  kind: K;
  params: ConstraintParams[K];         // discriminated by kind; registered in §4.5.5
}

interface ProbeResult {
  probeId: string;
  name: string;
  outcome: "healthy" | "unhealthy" | "unknown" | "unsupported";
  observedAtUs: UtcUs;
  evidence?: RecordId[];
  detail?: string;
}

interface ConditionView {
  conditionId: string;
  statement: string;
  state: "met" | "unmet" | "unknown";
  probeId?: string;
  basisSeq: number;
}

interface DeltaEnvelope {
  operations: string[];
  paths: string[];
  parameterRanges?: Record<string, { min?: number; max?: number }>;
  childCriteria?: string[];
}

interface CurationGesture {
  kind: "merge" | "split" | "link" | "scope" | "correction" | "retraction" | "promotion";
  targetIds: RecordId[];
  payload?: unknown;
}

interface ComponentEnvelope {
  type: ComponentType;
  payload: unknown;                   // JCS-encodable JSON
}

interface RecordEnvelope {
  recordId: RecordId;
  seq: number;                         // sequencer-assigned
  epochId: EpochId;
  recordedAtUs: UtcUs;                 // sequencer-assigned, non-decreasing within epoch
  prevHash: Digest;
  components: ReadonlyArray<ComponentEnvelope>; // sorted by type
  recordHash: Digest;                  // excluded from hashed bytes
}
```

**Digest, time, and money encoding.** A `Digest` always serializes as `sha256:` followed by 64 lowercase hexadecimal characters. SQLite `BLOB` columns store the 32 raw bytes of the same digest; conversion is lossless and MUST NOT use base64 or uppercase hex. `UtcUs` and `Duration` are integers in the inclusive range `[Number.MIN_SAFE_INTEGER, Number.MAX_SAFE_INTEGER]`; fractional, `NaN`, and infinite values are rejected. `Money` is an integer count of USD micros; no rounding occurs in canonical paths, and provider decimal values are multiplied by 1,000,000 and rounded half-up exactly once at the boundary. `Iso` is used only at external boundaries and is converted to `UtcUs` before canonicalization.

**Domain-scope containment.** `A ⊆ B` (`A` contained in `B`) is evaluated per field:

- `projectIds`: `A.projectIds ⊆ B.projectIds` as sets.
- `paths`: an absent value means universal (all paths). If `B.paths` is absent, the field is contained. If `B.paths` is present, `A.paths` MUST be present and every `a ∈ A.paths` MUST equal some `b ∈ B.paths` or be a descendant of `b` at a `/` boundary.
- `effectClasses`: an absent value means universal. If `B.effectClasses` is absent, contained. Otherwise `A.effectClasses` MUST be present and every `a` MUST be `≤` some `b` in the policy-owned effect-class order of §4.5.4.
- `dataClasses`: same set rule as `paths` without prefix semantics.

`IsolationScope` is a partition identifier, never a `DomainScope`, and has no containment relation. Capability grants, manifests, candidate pointers, and append allowlists use `IsolationScope`; approvals, interpretations, work, claims, and disclosure use `DomainScope`.

The mandatory envelope fields are only identity, canonical recording time, epoch, predecessor hash, components, and integrity hash. The append API MAY require particular components for particular namespaces or authorities as conditional policy checks, but those requirements are not envelope requirements. Unknown optional components survive; a record whose required component major version no operational projection understands is rejected at append (§4.2.2).

Only one semantic clock is mandatory: `recordedAtUs`. Valid time, observed time, published time, derivation time, and expiry time are optional typed components. File `mtime` MUST NOT be used as capture time.

#### 4.2.2 Component namespaces and versioning

- `kb.core/*` is frozen kernel vocabulary: identity, authority, retraction, link, time, and lifecycle.
- `kb.state/*`, `kb.work/*`, `kb.memory/*`, `kb.observation/*`, `kb.effect/*`, `kb.security/*`, and `kb.audit/*` are stable Kanbei domains.
- A stable-domain namespace never confers authority. The frozen append API authorizes each stable-domain write by the triple (authority class, appender role, component type) in the matrix below, so an agent or plugin write is accepted only for a listed triple even though the component lives in a stable namespace.
- Plugins use `kb.plugin.<pluginId>/*` for private state and MUST NOT redefine `kb.core/*`, any stable domain, or a component type that a projection selects by type.
- Corrections and retractions append new records; they MUST NOT mutate prior canonical records.

| Component family | Permitted authority classes | Permitted appenders |
|---|---|---|
| `kb.core/*` | class-appropriate (§4.2.4) | Frozen append path only; agents and plugins may request validated `link` and `transition` intents within their authority. |
| `kb.state/desired@*` | `human_authority`, `delegated_authority` | Human, or the frozen append path acting on an accepted plan or an active autonomy policy that delegates desired state (`req. 60`, `req. 66`). |
| `kb.state/observed@*` | `observation`, `effect_receipt`, `delegated_authority` | Agent, plugin, or system within capability scope; frozen effect journal. |
| `kb.work/*` | `lifecycle`, `coordination`, `candidate_claim`, `delegated_authority` | Frozen append path for transitions; agents and humans for coordination and candidates; accepted plans for delegated work. |
| `kb.work/consent_display@*` | `coordination` | Frozen consent renderer only (§4.1.2); agents, plugins, and the mutable UI cannot append it. |
| `kb.memory/claim*`, `kb.memory/proposal*`, `kb.memory/derivation*` | `observation`, `candidate_claim`, `derived_proposal` | Agent, plugin, or system within scope. |
| `kb.memory/promotion@*`, `kb.memory/attestation@*` | `human_authority` | Human only. |
| `kb.observation/*` | `observation` | Agent, plugin, or system within capability scope. |
| `kb.effect/*`, `kb.activation/*` | `effect_receipt`, `activation_receipt` | Frozen effect journal, broker, supervisor, or activation supervisor only. |
| `kb.security/*`, `kb.audit/*` | `security_action`, `coordination` | Frozen security component, human, or frozen audit path. |

An agent or plugin therefore MAY append stable-domain observations, candidate claims, derived reconciliation proposals, and policy-approved desired state when the row permits it. It MUST NOT append promotions, attestations, effect or activation receipts, security actions, kernel generations, or any stable-domain component whose permitted classes exclude its authenticated class.

Version rules:

| Change | Rule | Reader behavior |
|---|---|---|
| Major | Incompatible semantics | A record whose component is *required* to interpret that record by an operational projection, and whose major version no operational projection understands, MUST be rejected at append with `UNSUPPORTED_COMPONENT`. A record whose unknown-major component is not required by any operational projection MAY still be appended as opaque evidence. |
| Minor | Backward-compatible additive fields | Older readers MUST ignore unknown fields while preserving the raw payload. |
| Patch | Editorial or implementation fix | No schema or semantic change. |

Each projection declares the component types and version ranges it understands. A projection encountering an unknown optional component MAY ignore it; it MUST NOT silently drop a component it requires. A projection that encounters a required major version it does not understand reports `unsupported`, refuses to advance its coverage watermark, and raises a decision item; it MUST NOT freeze a shared operational projection while the append path continues to commit records that projection is required to maintain. Unknown components MUST survive append, export, backup, restore, and projection reads. Upcasting is a pure function inside a projection; canonical records are never rewritten.

The append API materializes and validates `kb.core/*` identity, authority, link, and time components from authenticated context and validated request fields. A caller MUST NOT spoof another principal, authority class, subject identity, or predecessor link.

#### 4.2.3 Canonical bytes and integrity

Canonicalization MUST be deterministic and independent of SQLite storage:

1. Validate envelope structure, `recordId` syntax, epoch, component type grammar, and duplicate component types.
2. Reject non-JSON values, non-finite numbers, and timestamps outside the supported integer range.
3. Canonicalize each component payload with JSON Canonicalization Scheme (JCS), RFC 8785: UTF-8, object keys sorted by UTF-16 code-unit order, no insignificant whitespace, ES6 number serialization (no trailing zeros, `-0` serialized as `0`), and strings escaped exactly as JCS requires.
4. Sort components by `type` using the same UTF-16 code-unit order; reject duplicate `type` in one record.
5. Build the canonical object containing `record_id`, `seq`, `epoch_id`, `recorded_at_us`, `prev_hash`, and `components`. Do not include `record_hash`. Integers serialize as JSON numbers; digests serialize in their `sha256:<hex>` text form inside JSON, and as 32 raw bytes in binary preimages.
6. Compute:

```text
record_hash = SHA-256(
  ASCII("kanbei.record.v1") || 0x00 ||
  UTF8(canonical_json)
)
```

Here and below, `ASCII(s)` is the byte string of `s`, `UTF8(s)` is its UTF-8 encoding, `0x00` is one NUL byte, and `||` is byte concatenation. `record_hash` is stored as the 32-byte digest; the `sha256:` text form is used only in JSON, logs, and APIs. The predecessor hash is inside the canonical bytes. A `hash_alg` column permits future algorithm agility. Signatures over records or checkpoints are optional future work; the chain provides integrity, not authenticity.

#### 4.2.4 Authority classes and principals

`kb.core/authority@1.0.0` has a closed `class` vocabulary:

```ts
type AuthorityClass =
  | "human_authority"        // intent, approval, amendment, veto, curation, attestation, promotion, human revocation
  | "delegated_authority"    // desired state or activation authorized by an accepted plan
  | "observation"            // captured source content or external change
  | "candidate_claim"        // agent-proposed claim or classification; never authority
  | "derived_proposal"       // merge, split, alias, contradiction, or reconciliation proposal
  | "effect_receipt"         // attempted or observed external effect
  | "activation_receipt"     // configuration activation or rollback
  | "security_action"        // quarantine or security control, or a system-authored revocation
  | "coordination"           // message, inbox/outbox item, wakeup, subscription, request
  | "lifecycle"              // append-only state transition, expiry, or promotion
  | "kernel_generation";     // recorded Nix generation at startup
```

Authority is optional. A record without a `kb.core/authority` component makes no authority claim and cannot satisfy any authority check. The append API materializes the component only when the intent supplies a class, and it derives that class from the authenticated principal for transitions and revocations.

| Class | Who may append | Meaning |
|---|---|---|
| `human_authority` | Authenticated human principal only | Human intent, approval, amendment, veto, curation, promotion, or revocation. |
| `delegated_authority` | Frozen append path, from an accepted plan or a human-delegated autonomy policy | Desired state, tool/context activation request, or work authorized by that plan; bound to the plan hash. |
| `observation` | Agent, plugin, or system within capability scope | Captured source content or an external change. It is evidence of what was captured, not a truth assertion. |
| `candidate_claim` | Agent or plugin | A proposed claim, interpretation, classification, or inference. It cannot create approval or authority. |
| `derived_proposal` | Agent or plugin | A proposed merge, split, alias, contradiction, or other reconciliation operation. |
| `effect_receipt` | Frozen effect journal, broker, or supervisor only. Agents and plugins MAY request an effect but cannot append its receipt. | An attempted or observed external effect. |
| `activation_receipt` | Frozen activation supervisor only | Activation, rollback, or health result. |
| `security_action` | Frozen security component | Quarantine, security control, or a system-authored revocation. A human-authored revocation is `human_authority`. |
| `coordination` | Any authorized agent, plugin, human, or system principal | Non-authority coordination: approval requests, messages, inbox/outbox items, wakeups, subscriptions, and timers. |
| `lifecycle` | Frozen append path, derived from a validated typed `transition` intent | State transitions for work, interpretations, requests, receipts, divergences, loops, wakeups, timers, overrides, candidates, and enrollments. Agents and plugins may request transitions allowed by their authority; they cannot write arbitrary lifecycle payloads. |
| `kernel_generation` | Frozen startup verifier | The Nix generation actually running at boot. |

Untrusted content creates observations and candidate claims, never authority records. Human promotion appends a linked human-attested assertion; it does not certify objective truth. The append API enforces principal-to-class rules; autonomy-tier decisions remain domain policy, not an envelope concern.

**Revocation rule.** A revocation is one record of shape §4.5.5 with no stored class field: its `authority_class` is `human_authority` when the actor is human and `security_action` when the actor is the frozen security component. The frozen append path updates the revoked subject's lifecycle projection in the same transaction. A revocation is never `lifecycle` class and never a mutation.

**Lifecycle rule.** Canonical payload types in §4.4–§4.6 carry no `state` field. The current state of a subject is a lifecycle projection computed from the latest `kb.core/lifecycle@1.0.0` record for that subject at a declared `basis_seq` (or `minSeq`). Lifecycle records are append-only and carry `{family, subject_id, from, to, event, actor_id, basis_seq, guard_digest, acceptance_kind?, review_deadline_us?, review_overdue?}`; `guard_digest` commits the canonical inputs the frozen append path evaluated, and `acceptance_kind` records `human` or `policy` for interpretation and promotion acceptances. Expiry and supersession are lifecycle records, never mutations.

#### 4.2.5 Global sequence, causal DAG, epochs, and checkpoints

- `seq` MUST be assigned only by the single sequencer inside `BEGIN IMMEDIATE`. It MUST NOT be derived from wall-clock time, client input, or record content.
- `recordedAtUs` is sequencer-assigned and MUST be non-decreasing with `seq` within an epoch. If the host clock moves backward, the sequencer clamps to the previous value and raises an operational `clock_skew` flag; `seq` remains authoritative for order.
- The chain is per epoch. Normal operation has one epoch; erasure starts a new one. An append intent MAY carry `expectedEpochId`; when present it MUST equal `ledger_head.epoch_id`, otherwise the append is rejected with `EPOCH_MISMATCH`. An intent without it targets the current head epoch; no intent can select a historical epoch.
- The genesis predecessor for an epoch is:

```text
prev_hash = SHA-256(
  ASCII("kanbei.epoch.genesis.v1") || 0x00 ||
  UTF8(epoch_id) || 0x00 ||
  prior_checkpoint_hash_raw      // 32 bytes, or 32 zero bytes when none
)
```

- Checkpoints are written every operator-configured `checkpoint_interval` records and immediately before backup or erasure. A checkpoint digest is:

```text
SHA-256(
  ASCII("kanbei.checkpoint.v1") || 0x00 ||
  UTF8(epoch_id) || 0x00 ||
  ASCII(decimal(up_to_seq)) || 0x00 ||
  head_hash_raw || 0x00 ||     // 32 bytes
  ASCII(decimal(record_count))
)
```

- `ledger_checkpoint` is included in every backup; the backup manifest also carries the checkpoint digest.
- Verification reads records from the last trusted checkpoint forward, recomputes canonical bytes and hashes, and compares `prev_hash`. A mismatch, missing sequence, duplicate hash, or idempotency inconsistency marks the ledger corrupt and blocks writes.
- No global causal total order is required or inferred. `seq` orders recording; `causes`, `owns`, `derives`, `retracts`, `blocks`, and `informs` links express partial order. Distributed clocks MUST NOT be required.

Causal links MUST reference existing earlier records. Because a record can reference only earlier records, the causal graph is acyclic by construction; the append API additionally rejects self-links and cycles. Derivations MUST point backward in sequence.

#### 4.2.6 SQLite logical schema

Database split:

- `ledger.sqlite` — canonical ledger plus synchronous operational projections. One writer, WAL mode, `synchronous=FULL`, foreign keys enabled.
- `projections/<name>.sqlite` — semantic projection stores, one per projection family. Each has its own writer and is rebuildable. They are not authority.
- `cas/` — OCI-layout blob store.
- `backups/` — backup manifests and catalog metadata, not canonical state.

Canonical schema:

```sql
PRAGMA journal_mode = WAL;
PRAGMA synchronous = FULL;
PRAGMA foreign_keys = ON;

CREATE TABLE ledger_epoch (
  epoch_id              TEXT PRIMARY KEY,
  genesis_seq           INTEGER NOT NULL,
  prior_epoch_id        TEXT,
  prior_checkpoint_hash BLOB,
  erasure_digest        BLOB,
  started_at_us         INTEGER NOT NULL
);

CREATE TABLE ledger_record (
  seq             INTEGER PRIMARY KEY,
  epoch_id        TEXT NOT NULL REFERENCES ledger_epoch(epoch_id),
  record_id       TEXT NOT NULL UNIQUE,
  recorded_at_us  INTEGER NOT NULL,
  prev_hash       BLOB NOT NULL,
  record_hash     BLOB NOT NULL UNIQUE,
  hash_alg        TEXT NOT NULL DEFAULT 'sha256',
  canonical_bytes BLOB NOT NULL,
  authority_class TEXT CHECK (authority_class IN (
    'human_authority','delegated_authority','observation','candidate_claim',
    'derived_proposal','effect_receipt','activation_receipt','security_action',
    'coordination','lifecycle','kernel_generation')),   -- null: no authority claim
  actor_id        TEXT NOT NULL,
  entity_id       TEXT,
  idempotency_key TEXT,
  UNIQUE(epoch_id, seq)
);

CREATE TABLE ledger_component (
  seq            INTEGER NOT NULL REFERENCES ledger_record(seq),
  component_type TEXT NOT NULL,
  major          INTEGER NOT NULL,
  minor          INTEGER NOT NULL,
  patch          INTEGER NOT NULL,
  payload        BLOB NOT NULL,       -- canonical JSON
  payload_hash   BLOB NOT NULL,
  PRIMARY KEY (seq, component_type)
);

CREATE TABLE ledger_link (
  from_seq  INTEGER NOT NULL REFERENCES ledger_record(seq),
  to_seq    INTEGER NOT NULL REFERENCES ledger_record(seq),
  link_type TEXT NOT NULL,
  ordinal   INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (from_seq, link_type, to_seq)
);

CREATE TABLE derivation_edge (
  derived_seq INTEGER NOT NULL REFERENCES ledger_record(seq),
  input_seq   INTEGER NOT NULL REFERENCES ledger_record(seq),
  role        TEXT NOT NULL,
  PRIMARY KEY (derived_seq, input_seq, role),
  CHECK (derived_seq > input_seq)
);

-- Reverse indexes make retraction closure a bounded index walk, not a scan.
CREATE INDEX ledger_link_reverse ON ledger_link(to_seq, link_type, from_seq);
CREATE INDEX derivation_edge_reverse ON derivation_edge(input_seq, role, derived_seq);

CREATE TABLE ledger_idempotency (
  actor_id       TEXT NOT NULL,
  scope_id       TEXT NOT NULL,
  key            TEXT NOT NULL,
  request_digest BLOB NOT NULL,
  record_id      TEXT NOT NULL,
  record_hash    BLOB NOT NULL,
  created_at_us  INTEGER NOT NULL,
  PRIMARY KEY (actor_id, scope_id, key)
);

CREATE TABLE ledger_head (
  singleton             INTEGER PRIMARY KEY CHECK (singleton = 1),
  epoch_id              TEXT NOT NULL,
  last_seq              INTEGER NOT NULL,
  head_hash             BLOB NOT NULL,
  last_recorded_at_us   INTEGER NOT NULL
);

CREATE TABLE ledger_checkpoint (
  epoch_id     TEXT NOT NULL,
  up_to_seq    INTEGER NOT NULL,
  head_hash    BLOB NOT NULL,
  record_count INTEGER NOT NULL,
  digest       BLOB NOT NULL,
  created_at_us INTEGER NOT NULL,
  PRIMARY KEY (epoch_id, up_to_seq)
);

CREATE TABLE ledger_erasure (
  seq             INTEGER PRIMARY KEY REFERENCES ledger_record(seq),
  erasure_digest  BLOB NOT NULL,
  erased_at_us    INTEGER NOT NULL
);

CREATE TABLE blob_manifest (
  manifest_digest TEXT PRIMARY KEY,       -- sha256:<hex> over canonical BlobManifest bytes
  content_digest  TEXT NOT NULL REFERENCES blob_object(digest),
  size            INTEGER NOT NULL,
  media_type      TEXT NOT NULL,
  created_at_us   INTEGER NOT NULL
);

CREATE TABLE blob_object (
  digest          TEXT PRIMARY KEY,      -- content digest
  size            INTEGER NOT NULL,
  media_type      TEXT NOT NULL,
  state           TEXT NOT NULL CHECK (state IN ('pending','committed','collectible','erased')),
  epoch_id        TEXT NOT NULL,
  created_at_us   INTEGER NOT NULL
);

CREATE TABLE blob_ref (
  record_seq      INTEGER NOT NULL REFERENCES ledger_record(seq),
  manifest_digest TEXT NOT NULL REFERENCES blob_manifest(manifest_digest),
  content_digest  TEXT NOT NULL REFERENCES blob_object(digest),
  role            TEXT NOT NULL,
  PRIMARY KEY (record_seq, manifest_digest, role)
);
```

Operational projection tables live beside these and are updated in the same transaction:

```sql
CREATE TABLE lifecycle_projection (
  family             TEXT NOT NULL,       -- work | interpretation | approval_request |
                                          -- approval_receipt | divergence | tier_proposal |
                                          -- tier_exception | constraint_override | loop |
                                          -- wakeup | timer | inbox | outbox | effect | candidate |
                                          -- reconciliation | branch | undo | enrollment
  subject_id         TEXT NOT NULL,       -- RecordId of the immutable payload record
  state              TEXT NOT NULL,
  actor_id           TEXT NOT NULL,
  acceptance_kind    TEXT CHECK (acceptance_kind IN ('human','policy')),
  ruling             TEXT CHECK (ruling IN ('approve','reject','defer','amend')),
  guard_digest       TEXT,
  basis_seq          INTEGER NOT NULL,
  review_deadline_us INTEGER,
  review_overdue     INTEGER NOT NULL DEFAULT 0,
  PRIMARY KEY (family, subject_id)
);

CREATE TABLE work_projection (
  work_id                  TEXT PRIMARY KEY,
  owner_parent_id          TEXT,
  desired_generation       INTEGER,
  observed_generation      INTEGER,
  generation               INTEGER NOT NULL,
  current_work_generation_id TEXT NOT NULL, -- selected immutable WorkGeneration record
  completion_subject_hash  TEXT,              -- from the current WorkGeneration; never stored on WorkItem
  acceptance_digest        TEXT,              -- from the current WorkGeneration
  basis_seq                INTEGER NOT NULL
);

CREATE TABLE work_generation_projection (
  work_id                  TEXT NOT NULL,
  generation               INTEGER NOT NULL,
  work_generation_id       TEXT NOT NULL,     -- RecordId of the immutable kb.work/work_generation
  completion_subject_hash  TEXT,
  acceptance_digest        TEXT NOT NULL,
  basis_seq                INTEGER NOT NULL,
  PRIMARY KEY (work_id, generation, work_generation_id)
);

CREATE TABLE work_review (
  work_id         TEXT NOT NULL,
  generation      INTEGER NOT NULL,
  started_at_us   INTEGER NOT NULL,
  deadline_us     INTEGER NOT NULL,
  overdue         INTEGER NOT NULL DEFAULT 0,
  promotion_id    TEXT,
  PRIMARY KEY (work_id, generation)
);

CREATE TABLE completion_projection (
  work_id                 TEXT NOT NULL,
  generation              INTEGER NOT NULL,
  work_generation_id      TEXT NOT NULL,
  completion_claim_id     TEXT NOT NULL,
  completion_subject_hash TEXT NOT NULL,
  promotion_kind          TEXT NOT NULL,
  basis_seq               INTEGER NOT NULL,
  PRIMARY KEY (work_id, generation)
);

CREATE TABLE consent_token (
  token_hash          TEXT PRIMARY KEY,       -- server-side hash only; the token itself is never stored
  request_id          TEXT NOT NULL,
  human_session_id    TEXT NOT NULL,
  consent_display_id   TEXT NOT NULL REFERENCES ledger_record(record_id), -- canonical ConsentDisplayManifest shown to the human
  manifest_digest     TEXT NOT NULL,          -- consentDigest recomputed from the persisted display manifest
  expires_at_us       INTEGER NOT NULL,
  consumed_at_us      INTEGER,
  ruling              TEXT CHECK (ruling IN ('approve','reject','defer','amend')),
  ruling_commitment   TEXT,                   -- over manifest_digest, ruling, and token_hash; written at consumption
  ruling_seq          INTEGER,
  CHECK (consumed_at_us IS NULL OR (ruling IS NOT NULL AND ruling_commitment IS NOT NULL)),
  CHECK (consumed_at_us IS NOT NULL OR (ruling IS NULL AND ruling_commitment IS NULL))
);

CREATE TABLE authority_evidence_projection (
  request_id           TEXT NOT NULL,
  evidence_id          TEXT NOT NULL,
  record_hash          TEXT NOT NULL,
  state                TEXT NOT NULL CHECK (state IN ('active','retracted')),
  authority_affecting  INTEGER NOT NULL DEFAULT 0,
  last_seq             INTEGER NOT NULL,
  basis_seq            INTEGER NOT NULL,
  PRIMARY KEY (request_id, evidence_id)
);

-- AgentLoop mutable execution data is projected here, never re-appended to the loop payload.
CREATE TABLE loop_runtime_projection (
  loop_id              TEXT PRIMARY KEY,
  state                TEXT NOT NULL,
  last_heartbeat_at_us INTEGER,
  consecutive_idle     INTEGER NOT NULL DEFAULT 0,
  branch_set_json      TEXT NOT NULL DEFAULT '[]',  -- Branch{id,head,latest checkpoint} array
  budget_state_json    TEXT NOT NULL,               -- consumed counters + active override deltas
  basis_seq            INTEGER NOT NULL
);

CREATE TABLE retraction_target (
  target_seq      INTEGER PRIMARY KEY,
  target_record_id TEXT NOT NULL,
  retraction_seq  INTEGER NOT NULL REFERENCES ledger_record(seq),
  reason          TEXT NOT NULL
);

CREATE TABLE suspect_record (
  record_seq       INTEGER PRIMARY KEY,
  record_id        TEXT NOT NULL,
  retracted_by_seq INTEGER NOT NULL REFERENCES ledger_record(seq),
  depth            INTEGER NOT NULL,
  computed_at_us   INTEGER NOT NULL
);
CREATE INDEX suspect_record_by_id ON suspect_record(record_id);

CREATE TABLE pending_closure (
  retraction_seq  INTEGER PRIMARY KEY REFERENCES ledger_record(seq),
  target_seq      INTEGER NOT NULL,
  frontier        TEXT NOT NULL,        -- JSON array of pending record_seq values
  remaining       INTEGER NOT NULL,
  updated_at_us   INTEGER NOT NULL
);

CREATE TABLE quarantine_scope (
  quarantine_id TEXT PRIMARY KEY,
  root_seq      INTEGER NOT NULL REFERENCES ledger_record(seq),
  scope_json    TEXT NOT NULL,
  status        TEXT NOT NULL CHECK (status IN ('active','lifted')),
  started_at_us INTEGER NOT NULL,
  lifted_by_seq INTEGER
);

CREATE TABLE inbox_item (
  id                   TEXT PRIMARY KEY,
  consumer_id          TEXT NOT NULL,
  message_id           TEXT NOT NULL,
  state                TEXT NOT NULL CHECK (state IN ('pending','leased','processed','failed_retryable','dead_letter','skipped_duplicate')),
  attempts             INTEGER NOT NULL DEFAULT 0,
  lease_expires_at_us  INTEGER,
  dedupe_key           TEXT NOT NULL
);
CREATE UNIQUE INDEX inbox_dedupe ON inbox_item(consumer_id, message_id);

CREATE TABLE outbox_item (
  id                   TEXT PRIMARY KEY,
  producer_id          TEXT NOT NULL,
  message_id           TEXT NOT NULL,
  idempotency_key      TEXT NOT NULL,
  state                TEXT NOT NULL CHECK (state IN ('staged','committed','dispatched','delivered','failed_retryable','dead_letter')),
  dispatch_attempts    INTEGER NOT NULL DEFAULT 0,
  target               TEXT NOT NULL
);
CREATE UNIQUE INDEX outbox_dedupe ON outbox_item(producer_id, idempotency_key);
```

`work_projection.state` is deliberately absent: work state is read from `lifecycle_projection` by `(family='work', subject_id)`. `work_projection.current_work_generation_id` selects the immutable `WorkGeneration` record; its `completion_subject_hash` is derived from the accepted interpretation hash, `generation`, and `acceptance_digest`, and `WorkItem` carries neither field. The selected `WorkGeneration.completion_subject_hash` is the only legal completion subject referent for `EvidenceRef.subject_hash`. Completion evidence is not stored on `WorkItem`: the frozen append path writes one `kb.work/completion_claim@1.0.0` payload with the `submit` transition and projects it in `completion_projection`. `work_review` records the window start and deadline; `review_overdue` carries the promote-with-flag outcome. `loop_runtime_projection` holds per-step mutable loop data (§4.7.1), so no loop payload is re-appended for heartbeats, budgets, or branch heads.

Retraction closure reads use the reverse indexes `ledger_link_reverse` and `derivation_edge_reverse`; a closure query MUST be an index walk, never a scan. The in-transaction walk is bounded by `retraction_closure_batch`, and `pending_closure` drains asynchronously in bounded batches. A read whose closure traversal exceeds `retraction_read_budget_us` returns the target and its discovered frontier as `suspect` and never returns an undiscovered dependent as current; the worker completes the closure. This is the latency contract for `ZT-06` and `V15`.

`canonical_bytes` duplicates the serialized record for direct verification. Components are separately indexed for queries; the two representations MUST agree. On startup and restore, a record whose recomputed hash does not match either representation is corruption.

#### 4.2.7 Append transaction

```ts
type BlobRefRequest = { manifestDigest: Digest; role: string };

type AppendIntent =
  | {
      kind: "append";
      actor: Principal;
      scope: IsolationScope;
      idempotencyKey: string;
      authority?: AuthorityClass;      // absent means the record claims no authority
      subject?: EntityId;
      components: ReadonlyArray<ComponentEnvelope>;
      parents?: RecordId[];            // causal links, all earlier
      cause?: RecordId;
      blobRefs?: ReadonlyArray<BlobRefRequest>;
      expectedEpochId?: EpochId;
    }
  | {
      kind: "link";
      actor: Principal;
      scope: IsolationScope;
      idempotencyKey: string;
      from: RecordId;
      link: WorkLink;
      expectedEpochId?: EpochId;
    }
  | {
      kind: "transition";
      actor: Principal;
      scope: IsolationScope;
      idempotencyKey: string;
      family: "work" | "interpretation" | "approval_request" | "approval_receipt"
            | "divergence" | "tier_proposal" | "tier_exception" | "constraint_override"
            | "loop" | "wakeup" | "timer"
            | "enrollment" | "candidate" | "reconciliation" | "branch" | "undo"
            | "effect" | "inbox" | "outbox";
      subjectId: RecordId;
      event: string;
      to: string;
      expect: { from: string };        // compare-and-set against lifecycle_projection
      fencingToken?: number;           // required for wakeup, lease, and timer families
      guard?: {                        // references are validated against canonical records
        receiptId?: RecordId;
        evidence?: RecordId[];
        completionClaimId?: RecordId;
        acceptanceKind?: "human" | "policy";
        reason?: string;
      };
      reviewDeadlineUs?: UtcUs;
      expectedEpochId?: EpochId;
    }
  | {
      kind: "retract";
      actor: Principal;
      scope: IsolationScope;
      idempotencyKey: string;
      target: RecordId;
      reason: string;
      expectedEpochId?: EpochId;
    }
  | {
      kind: "quarantine";
      actor: Principal;
      scope: IsolationScope;
      idempotencyKey: string;
      root: RecordId;
      quarantineScope: DomainScope;
      reason: string;
      expectedEpochId?: EpochId;
    };

interface AppendResult {
  seq: number;
  recordId: RecordId;
  recordHash: Digest;
  recordedAtUs: UtcUs;
}

append(intent: AppendIntent): Promise<AppendResult>;
```

`RecordId` is assigned by the sequencer (`rec_<ULID>`), never by the caller, so a caller cannot squat or predict another principal's identity. The idempotency key is scoped by `(actor, scope)`; the same key from another scope is unrelated. `expectedEpochId` is optional and is never used to assign an epoch.

The append protocol MUST be:

1. `BEGIN IMMEDIATE`; this acquires the single-writer lock.
2. Authenticate `actor` from the caller channel and validate `scope` against the caller's bindings. `actor` is required for every intent; an unauthenticated intent is rejected with `ACTOR_REQUIRED`.
3. Validate intent shape, component types, known-component schemas, the authority principal when an authority class is supplied, conditional component requirements, and blob visibility.
4. Compute `request_digest` over the canonical intent (excluding sequencer-assigned `seq`, `recordId`, `recordHash`, and `recordedAtUs`).
5. Look up `(actor, scope, idempotencyKey)`.
   - Same key and same digest: return the prior `AppendResult` without appending.
   - Same key and different digest: reject with `IDEMPOTENCY_CONFLICT`.
6. Read `ledger_head`. If `expectedEpochId` is present and differs from `head.epoch_id`, reject with `EPOCH_MISMATCH`; an intent without it targets the current head epoch and cannot select a historical epoch. Assign `seq = last_seq + 1`; assign `recorded_at_us = max(clock_now_us, last_recorded_at_us)`. Clamp backward clock jumps; `seq` remains authoritative.
7. Set `prev_hash = head_hash`.
8. Canonicalize, compute `record_hash`, and insert `ledger_record` and `ledger_component`.
9. Apply the typed intent:
   - `append`: insert `ledger_link` and `derivation_edge` rows from `parents`/`cause`; `link` inserts one `ledger_link` row.
   - `transition`: validate the tuple `(family, subjectId, event, to)` against the family table named in the lifecycle registry below; require `lifecycle_projection.state = expect.from`; evaluate the table's actor predicate and guard from canonical records; require `fencingToken` to equal the subject's current token for wakeup, lease, and timer families; then append `kb.core/lifecycle@1.0.0` with the applied `from`/`to`, `actor_id`, `guard_digest`, and `acceptance_kind`. Step 12 updates `lifecycle_projection` and increments the fencing token where the family defines one. An unknown family, event, or target, an actor mismatch, an unmet guard, or a stale `expect.from` rejects with `TRANSITION_REJECTED`, `GUARD_FAILED`, or `STATE_CONFLICT`; a stale token rejects with `FENCING_STALE`.
   - `retract`: insert `retraction_target` and `suspect_record` for the target and for the in-transaction bounded descendant batch; insert a `pending_closure` row when the closure exceeds the batch.
   - `quarantine`: insert `quarantine_scope` and invalidate dependent semantic reads.
10. Validate domain invariants:
    - no record carries both desired and observed components;
    - each work record has at most one `owns` parent;
    - causal links reference existing records and do not form a cycle;
    - derivation edges reference existing records and point backward in sequence;
    - human-authority records have a human principal;
    - `effect_receipt` and `activation_receipt` intents originate only from their frozen appenders;
    - agent principals do not create `human_authority`, `delegated_authority`, `activation_receipt`, `effect_receipt`, `security_action`, `kernel_generation`, or spoof `kb.core/*`; stable-domain components are admitted only through the (class, appender role, component type) matrix in §4.2.2;
    - a derivation input is not `suspect` or retracted at the current basis; if it is, the append is rejected with `DERIVATION_FROM_RETRACTED`;
    - every `blobRef` resolves through `blob_manifest` to a `blob_object.state = 'committed'` with matching content digest.
11. Enforce per-principal admission quotas (records/second, bytes/second, burst) before commit; a breach rejects with `QUOTA_EXCEEDED`.
12. Update operational projections in the transaction: lifecycle projection, work DAG, decision index, observation metadata, active configuration pointer, ledger status, retraction closure/quarantine, and blob references.
13. Insert the idempotency row; update `ledger_head`; optionally insert a checkpoint.
14. `COMMIT`.
15. After commit, notify semantic projection workers and read caches. Notification failure is not data loss: semantic workers poll `ledger_head.last_seq` and coverage checkpoints.

**Lifecycle registry.** Every `transition` intent MUST name a family in this registry and match the table it names. The frozen append path evaluates the table; prose elsewhere is explanatory only. A table row is the complete set of legal `from → event → to` triples for that family, and the actor column is an additional predicate.

| Family | Subject | Normative transition table | Default actor rule |
|---|---|---|---|
| `work` | `WorkItem` record | §4.4.3 | Table actor column. |
| `interpretation` | `IntentInterpretation` record | §4.5.2 | Table actor column. |
| `approval_request` | `ApprovalRequest` record | §4.5.5 | Table actor column. |
| `approval_receipt` | `ApprovalReceipt` record | §4.5.5 | Table actor column; no transition reactivates a receipt. |
| `divergence` | `Divergence` record | §4.4.5 | Table actor column. |
| `tier_proposal` | `TierProposal` record | §4.5.4 | Table actor column; agents propose, humans accept or reject. |
| `tier_exception` | `TierException` record | §4.4.3 | Table actor column; human-only creation. |
| `constraint_override` | `ConstraintOverride` or `OverrideEscalation` record | §4.5.7 | Table actor column. |
| `loop` | `AgentLoop` record | §4.7.1 | Table actor column. |
| `wakeup` | `Wakeup` record | §4.7.2 | Table actor column; CAS and fencing required. |
| `timer` | `Timer` record | §4.7.2 | Frozen scheduler or human; CAS and fencing required. |
| `inbox` / `outbox` | item record | §4.7.4 | Consumer/producer or frozen transport. |
| `effect` | effect journal record | §4.7.5 | Frozen journal/broker/supervisor. |
| `candidate` | `CandidateManifest` record | §4.8.4 | Frozen activation supervisor; human for policy-pending approval. |
| `reconciliation` | proposal record | §4.6.3 | Table actor column. |
| `branch` / `undo` | `Branch` / `UndoRequest` record | §4.7.7 | Table actor column. |
| `enrollment` | `CredentialEnrollment` record | §4.10.6 | Human, or frozen append path for injector status. |

A transition that is not listed for its family is rejected even when the target string is a valid state name. Caller-supplied `guard` data is not trusted as a predicate: the append path re-derives every guard from canonical records and commits the result as `guard_digest`.

**Bounded retraction closure.** A retraction MUST NOT walk an unbounded descendant set while holding the single-writer lock. The transaction marks the target, walks at most `retraction_closure_batch` descendants over both `derivation_edge` and `informs`/`derives` `ledger_link` rows, and records `pending_closure` for the remainder. Until the closure completes, every read over the ledger-side closure treats the target's reachable dependents as suspect by recursive query; the async worker drains `pending_closure` in bounded batches. A watchdog kill leaves the retraction record and the pending intent durable, so no dependent becomes current in the gap.

Failure behavior:

| Error | Effect |
|---|---|
| `VALIDATION_FAILED` | Transaction rolls back; no record, no sequence consumption. |
| `ACTOR_REQUIRED` / `AUTHORITY_DENIED` | Rollback; append API records a denial receipt if the audit sink is reachable. |
| `IDEMPOTENCY_CONFLICT` | Rollback; caller must use a new key. |
| `DUPLICATE_RECORD` | Rollback; a supplied record id collides (only possible for imported records). |
| `EPOCH_MISMATCH` | Rollback; `expectedEpochId` differs from the head epoch. |
| `TRANSITION_REJECTED` | Rollback; family/event/target is not an allowed row or the actor predicate fails. |
| `STATE_CONFLICT` | Rollback; `expect.from` differs from the projected state. |
| `FENCING_STALE` | Rollback; the supplied fencing token is not current. |
| `GUARD_FAILED` | Rollback; a transition guard did not hold at the evaluation basis. |
| `BLOB_NOT_VISIBLE` | Rollback; caller must publish the blob first. |
| `CAUSAL_CYCLE` / `OWNERSHIP_CONFLICT` | Rollback; rejected. |
| `DESIRED_OBSERVED_CONFLICT` | Rollback; split into two records. |
| `DERIVATION_FROM_RETRACTED` | Rollback; derivation input is suspect/retracted. |
| `RETRACTION_CONFLICT` | Rollback; the target already has a retraction. |
| `UNSUPPORTED_COMPONENT` | Rollback; required component major version is unknown to every operational projection. |
| `QUOTA_EXCEEDED` | Rollback; per-principal admission limit. |
| `CHAIN_MISMATCH` | Fatal; ledger enters read-only `CORRUPT` state; restore required. |
| `ADMISSION_REJECTED` | Rollback before blob or after temporary cleanup; no canonical record. |

Any failure leaves `ledger_head` unchanged. A committed record is visible to a subsequent read on the same SQLite database. Readers that need explicit read-your-writes pass `minSeq`; if `head.last_seq < minSeq`, they retry or fail rather than returning an older view. Operational projections commit atomically with the ledger and provide strong read-your-writes. Semantic projections expose coverage and pending work rather than pretending to be current.

---

### 4.3 Persistence, blobs, backup, and erasure

#### 4.3.1 State locations

All mutable Kanbei state MUST live under `~/.local/state/kanbei/`. Trusted Nix configuration and secret material MUST remain outside it.

```text
~/.local/state/kanbei/
  ledger.sqlite
  projections/<name>.sqlite
  cas/
  plugin-state/<scope>/<store>/
  agents/<agentId>/
  scratch/<sandboxId>/
  mutable/candidates/<scopeId>/<candidateId>/
  mutable/scopes/<scopeId>/current -> ../candidates/<scopeId>/<candidateId>
  backups/
```

The active mutable-code pointer is core-owned and read-only to agents. Scratch is deleted on sandbox termination. The canonical database, trust-root control files, broker configuration, secret material, and other agents' state are never mounted into an agent sandbox.

`ledger.sqlite` uses one sequenced writer, JSON component payloads, and transactional metadata. Semantic projection stores hold rebuildable SQL, FTS, and graph indexes; they are not authority. Operational projections include the work DAG, approvals, raw observation metadata, active configuration pointer, ledger status, retraction closure, quarantine scope, and blob references, and commit atomically with the ledger. Cheap indexes MAY join that class after measurement.

#### 4.3.2 Blob store contract

Blobs are immutable, content-addressed, whole objects in a local OCI-layout store behind a replaceable contract:

```ts
// BlobManifest is the single definition in §4.2.1: { contentDigest, size, mediaType }.

interface BlobPutResult {
  manifest: BlobManifest;
  manifestDigest: Digest;              // SHA-256 over canonical BlobManifest bytes
}

interface BlobStore {
  put(bytes: Uint8Array, mediaType: string): Promise<BlobPutResult>;
  get(contentDigest: Digest): Promise<Uint8Array>;
  has(contentDigest: Digest): Promise<boolean>;
  pin(ref: BlobRef, recordSeq: number, role: string): Promise<void>;
  unpin(ref: BlobRef, recordSeq: number, role: string): Promise<void>;
}
```

Publication order MUST be:

1. Write and fsync bytes into the CAS under their `contentDigest`.
2. Write and fsync the `blob_manifest` row whose `manifestDigest` is the SHA-256 of canonical `{contentDigest, size, mediaType}` bytes and whose `content_digest` references those bytes.
3. Commit `blob_object.state = 'committed'`.
4. Only then append a ledger record that references `(manifestDigest, contentDigest, role)`.

A ledger record MUST NOT reference a pending, collectible, or erased object. Bytes are durable before the manifest, and the manifest is committed before ledger visibility. `BlobRef` is the only reference shape; it carries both keys. `pin`/`unpin` take the full `BlobRef`, so a pin is keyed by exactly `(record_seq, manifest_digest, role)` and matches the `blob_ref` row that keeps it alive; two manifests over identical content with different `mediaType` are distinct pins. `blob_ref` joins on `manifest_digest` with an FK to `blob_manifest`, which in turn references `blob_object(digest)`. Restore resolves a reference by manifest digest, verifies the manifest bytes, then resolves the content digest; no caller guesses which key space applies.

Admission controls MUST enforce per-object and total size limits, backpressure, and safe orphan reclamation. A content object is collectible only when no `blob_manifest` row and no live pin reference it; a manifest is collectible only when no `blob_ref` row and no live pin reference it. Orphan reclamation MUST NOT remove either while a live reference or pin exists. Chunk-level deduplication is deferred; whole-object storage is the initial contract.

#### 4.3.3 Retention and erasure

The ledger MUST be retained indefinitely by default. Logical retraction is normal and does not delete anything. Physical erasure is an exceptional, human-authorized operation that creates a new epoch and a new hash-chain lineage.

Erase behavior:

- The new epoch records `prior_epoch_id`, `prior_checkpoint_hash`, and an `erasure_digest`.
- `erasure_digest` is `SHA-256(ASCII("kanbei.erasure.v1") || 0x00 || UTF8(canonical_json(policy)))`, where `policy` contains `epoch_id`, `prior_epoch_id`, `prior_checkpoint_hash`, the sorted erased `record_id`/`seq` pairs, the sorted erased blob content digests, the backup snapshot ids covered, and `requested_at_us`. It is a commitment to *what was erased*, not a copy of the erased content.
- The prior chain's continuous verifiability cannot be preserved; this is recorded in the epoch metadata. Verification of an erased prior epoch reports `erased` and checks the erasure digest only.
- Derived artifacts and backups are covered as far as possible. Copies outside Kanbei's control cannot be guaranteed removed.

Erasure procedure:

1. Freeze the prior epoch (`ledger_head` stops advancing) and open the new epoch with the epoch record above.
2. Compute the coverage set by epoch-scoped reachability: start from the human-authorized erasure scope, walk `derivation_edge`, `ledger_link`, and `blob_ref` within the prior epoch and cross-epoch, and include derived projections, CAS objects, and backup snapshots under Kanbei control.
3. For every erased canonical row, keep `seq`, `record_id`, `record_hash`, `prev_hash`, and `hash_alg` as a tombstone, zero `canonical_bytes` and every `ledger_component.payload`, and insert a `ledger_erasure(seq, erasure_digest)` row. Foreign keys from later epochs therefore still resolve, and no later record is silently deleted.
4. Delete CAS bytes named by erased blobs and mark their `blob_object.state = 'erased'`; erase backup snapshots covered by the new epoch and record the epoch conflict on restore.
5. Rebuild derived projections from the new epoch's retained inputs and record coverage; any projection that cannot rebuild from retained inputs reports `erased` coverage without resurrecting deleted content.
6. A cross-epoch reference to an erased row MUST resolve to a tombstone, not to a dangling key or to resurrected payload. Restore of a backup containing erased data MUST refuse unless it reports the epoch conflict.

No automated retention policy is selected here. Retention, compaction, and legal-erasure obligations remain open policy questions.

#### 4.3.4 Backup and restore

Backup MUST use this protocol:

1. Request an online SQLite point-in-time snapshot immediately before the normal filesystem backup.
2. Pause CAS garbage collection.
3. Copy the snapshot and immutable CAS files. Normal ledger writes MAY continue; extra new blobs are harmless.
4. Copy mutable scope metadata, receipts, and canonical metadata.
5. Resume CAS garbage collection after the copy.
6. Write a backup manifest containing the SQLite snapshot identity, the ledger checkpoint digest, and the blob catalog.
7. On restore, verify every ledger reference, every blob digest, the checkpoint digest, and then rebuild derived indexes before serving reads.

Exclusions: scratch, sockets, live WAL files, broker memory, and all secret material. The exact backup hook mechanism is `[SPIKE: BAK-1]`; the required ordering is not deferred. The watchdog MUST alert when the last successful backup exceeds its threshold.

---

### 4.4 Desired state, observed state, and work

#### 4.4.1 Desired and observed components

```ts
interface DesiredComponent {
  entityId: EntityId;
  generation: number;              // monotonically increasing per entity
  constraints: unknown;            // domain-specific desired constraints
  acceptance: ReadonlyArray<Criterion>;
  scope: DomainScope;
}

interface ObservedComponent {
  entityId: EntityId;
  observedGeneration: number;      // desired generation this observation addresses
  probes: ReadonlyArray<ProbeResult>;
  evidence: ReadonlyArray<RecordId>;
}
```

Rules:

- `kb.state/desired@1.0.0` and `kb.state/observed@1.0.0` MUST be separate component types and MUST appear on separate records. No record carries both; an entity MAY have both across separate records.
- An entity MAY have many desired and observed records over time. Convergence is computed by the operational projection from the latest desired generation and the latest observed generation that cites it.
- `generation` increases monotonically per entity on the desired authority path. A new desired generation is a new record; it never mutates the prior one.
- `observedGeneration` is a cursor, not a claim of success. Absence of an observed condition means unknown, not healthy.
- Desired authority is `human_authority` or `delegated_authority` (an accepted plan). Observed authority is `observation`, `effect_receipt`, or `delegated_authority`. The append API enforces the stable-domain matrix and class presence; autonomy-tier policy decides whether an agent may append a given desired record.
- No mandatory Kubernetes-shaped object is introduced. These are flat optional components linked by subject, not a nested `metadata/spec/status` object.

Convergence view:

```ts
interface ConvergenceView {
  entityId: EntityId;
  desiredGeneration: number | null;
  observedGeneration: number | null;
  divergence: "converged" | "pending" | "diverging" | "unknown";
  openConditions: ReadonlyArray<ConditionView>;
  basisSeq: number;
  coverage: Coverage;
}
```

`divergence` is derived and never canonical. It is `unknown` when either side is absent or the observation cites an older desired generation.

#### 4.4.2 Work links

```ts
type WorkLink =
  | { kind: "owns"; parent: RecordId }                 // zero or one per work record
  | { kind: "causes" | "blocks" | "informs"; target: RecordId };
```

- Each work record MUST have at most one ownership parent. The ownership relation is a forest expressing containment; the causal graph is a separate arbitrary DAG.
- Causal and blocking links MUST reference existing records only. The graph is acyclic by construction; the append API additionally rejects self-links and cycles.
- Ownership conveys lifecycle scope and resource custody, not authority over the parent's approvals. A child does not inherit receipt coverage except through the coverage predicate in §4.5.5.
- Completion is a record with evidence against the acceptance digest and cleared required blockers. The operational projection exposes `completion_candidate`, but does not infer completion from the absence of blockers or from row deletion.
- `seq` never substitutes for causation. Two independent work items may interleave arbitrarily.

#### 4.4.3 Work item and states

```ts
interface WorkItem {
  id: RecordId;
  ownership_parent_id: RecordId | null; // exactly one or root
  causal_parents: RecordId[];           // 0..n
  blockers: { work_item_id: RecordId; required: boolean }[];
  anchor_id: RecordId;
  branch_id: RecordId;
  scope: DomainScope;
  max_effect_class: string;             // upper bound in the policy order; incomparable classes denied
  acceptance_criteria: Criterion[];
}

// kb.work/work_generation@1.0.0; immutable canonical payload selected by the work projection.
interface WorkGeneration {
  id: RecordId;
  work_item_id: RecordId;
  generation: number;                   // increments on rework; prior records are never rewritten
  accepted_interpretation_hash: Digest | null; // null while a declared exploration has no accepted interpretation
  plan_ref: RecordId | null;            // canonical Plan record (§4.5.2)
  plan_hash: Digest | null;             // hash of the resolved Plan, not a declaration
  acceptance_digest: Digest;
  completion_subject_hash: Digest | null; // null until an interpretation is accepted
  supersedes_generation?: number;      // prior generation on rework
  recorded_at_us: UtcUs;
}

type WorkState =
  | "draft" | "speculative" | "admitted" | "active" | "blocked"
  | "submitted" | "review_pending" | "awaiting_promotion"
  | "completed" | "failed" | "abandoned" | "superseded" | "reopened";
```

`WorkItem` is an immutable canonical payload. Its `state` is read from `lifecycle_projection(family='work')`; the autonomy tier is resolved from the effective policy at admission and at every effect, never stored on the item. `WorkItem` carries no `generation` or `completion_subject_hash`: the work projection selects the current immutable `WorkGeneration` (`current_work_generation_id`) and uses its accepted interpretation, resolved plan, acceptance digest, and completion subject. A new `WorkGeneration` is appended at admission and on adoption of an interpretation; rework appends a new record with `generation + 1`. The projection selects the newest record for the work item at its basis. Evidence lives on the selected generation's `CompletionClaim`, never on `WorkItem`.

| From | Event | Actor | Guard | To |
|---|---|---|---|---|
| `draft` | `admit` | Loop or human | Tier ≠ `forbidden`; accepted interpretation covers, or declared exploration; immutable `WorkGeneration` appended | `admitted` or `speculative` |
| `speculative` | `adopt` | Loop or human | Accepted interpretation hash covers the branch; a new `WorkGeneration` for the same generation records it | `admitted` |
| `admitted` | `start` | Loop scheduler | Required blockers ready; concurrency lease acquired | `active` |
| `active` | `block` | Loop or system | Unresolved required blocker, open divergence, or hard budget stop | `blocked` |
| `blocked` | `unblock` | Loop or system | Blockers resolved; divergence closed | `active` |
| `active` | `submit` | Loop or human | A `CompletionClaim` exists for the selected `WorkGeneration`; every criterion has verified passing evidence; required blockers `completed`; no required effect `outcome_unknown` | `submitted` |
| `submitted` | `promote_auto` | Frozen append path | Tier `automatic`; evidence valid per §4.4.4 | `completed` |
| `submitted` | `promote_review` | Frozen append path | Tier `review_after`; records `work_review.started_at_us`/`deadline_us` | `review_pending` |
| `review_pending` | `ack` | Human | Human within window | `completed` |
| `review_pending` | `object` | Human | Human | `reopened` |
| `review_pending` | `window_expire` | Frozen append path | Policy permits promote-with-flag; no open `acceptance_conflict` or `decision_conflict` | `completed` (`review_overdue=1`) |
| `review_pending` | `window_expire_hold` | Frozen append path | Open `acceptance_conflict` or `decision_conflict` at expiry | `awaiting_promotion` |
| `completed` | `object_overdue` | Human | Human within the recorded post-promotion revocation window | `reopened` |
| `submitted` | `request_promotion` | Loop or human | Tier `approval_required` or an active override escalation | `awaiting_promotion` |
| `awaiting_promotion` | `approve` | Human gesture endpoint | Active promotion receipt | `completed` |
| `awaiting_promotion` | `reject` | Human | Human | `reopened` or `failed` |
| any nonterminal | `fail` | Loop, system, or human | Terminal error recorded | `failed` |
| any nonterminal | `abandon` | Loop or human | Actor + reason | `abandoned` |
| `completed` / `reopened` | `supersede` | Human or system | New interpretation or undo basis | `superseded` |
| `reopened` | `resume` | Loop or human | Append a new `WorkGeneration` with `generation + 1`; projection selects it | `active` |

Every transition in this table appends a `kb.core/lifecycle@1.0.0` record; no transition mutates a `WorkItem`.

```ts
interface TierException {
  id: RecordId;
  class: string;                        // effect class downgraded
  scope: DomainScope;
  from_tier: "forbidden";
  to_tier: "approval_required";
  expires_at_us: UtcUs;
  author_id: HumanId;
  authority_class: "human_authority";
}
```

`forbidden` is an admission gate, not a work or completion state. A human MAY append a scoped, expiring `TierException` downgrading a `forbidden` class to `approval_required`; agents MUST NOT. An active exception is evaluated by tier resolution and is itself a lifecycle subject (`family='tier_exception'`).

| From | Event | Actor | Guard | To |
|---|---|---|---|---|
| (none) | `create` | Human | `from_tier="forbidden"`; class and scope declared; expiry set; human actor | `active` |
| `active` | `expire` | Frozen append path | `now ≥ expires_at_us` | `expired` |
| `active` | `revoke` | Human or frozen security component | A `Revocation` record for this exception | `revoked` |

#### 4.4.4 Completion evidence

```ts
type EvidenceKind =
  | "test_run" | "diff" | "observation" | "human_attestation"
  | "effect_receipt" | "replay_digest";

interface EffectCitation {
  effect_id: EffectId;                  // journal-assigned effect identity, not a RecordId
  attempt: number;
  fencing_token: number;
  request_id: string;                   // broker/supervisor-minted, never caller-supplied
}

interface EvidenceRef {
  id: RecordId;                         // cited canonical record
  criterion_id: string;
  kind: EvidenceKind;
  subject_hash: Digest;                 // MUST equal the selected WorkGeneration's completion_subject_hash
  produced_by: Principal;               // human, agent, plugin, or system
  producer_authority: AuthorityClass;   // MUST match the cited record's authenticated class
  recorded_at_us: UtcUs;
  result: "pass" | "fail" | "inconclusive"; // MUST match the cited record's verified outcome
  effect?: EffectCitation;              // required when kind = "effect_receipt"
  redaction?: "none" | "redacted_for_agent"; // exactness of the cited payload
}

// kb.work/completion_claim@1.0.0, appended with the `submit` transition.
interface CompletionClaim {
  id: RecordId;
  work_item_id: RecordId;
  work_generation_id: RecordId;         // selected WorkGeneration this claim submits
  generation: number;
  completion_subject_hash: Digest;
  evidence: EvidenceRef[];
  criterion_evidence: { criterion_id: string; evidence_id: RecordId }[];
  promotion_kind: "automatic" | "review_after" | "approval_required";
  promotion_receipt_id?: RecordId;      // required for approval_required
  review_started_us?: UtcUs;
  review_deadline_us?: UtcUs;
  review_overdue?: boolean;
  post_promotion_revoke_until_us?: UtcUs;
}
```

`acceptance_digest = SHA-256(ASCII("kanbei.acceptance.v1") || 0x00 || UTF8(canonical_json(sorted(acceptance_criteria))))`. `WorkGeneration.completion_subject_hash = SHA-256(ASCII("kanbei.completion.v1") || 0x00 || UTF8(accepted_interpretation_hash) || 0x00 || ASCII(decimal(generation)) || 0x00 || UTF8(acceptance_digest))`. It is null until an interpretation is accepted. `CompletionClaim` is one immutable `kb.work/completion_claim@1.0.0` payload per `(work_item_id, generation)`, appended by the frozen append path in the same canonical record as the `submit` work-lifecycle transition; its `evidence` array is the record representation for completion evidence, and promotion and overdue transitions read it. `object_overdue`, `window_expire`, and `promote_review` write review fields through `work_review` and the lifecycle payload, never by rewriting the claim. Rework (`reopened → resume`) appends a new `WorkItem` payload with `generation + 1` and a recomputed hash; the prior payload is retained, and all evidence for the prior generation is invalid for the new one without being deleted.

Promotion requires, per criterion, at least one `pass` in the selected `WorkGeneration`'s `CompletionClaim` whose `subject_hash` equals that `WorkGeneration.completion_subject_hash`. `inconclusive` is not `pass`. Required blockers MUST be `completed`, not merely `submitted`. Any required effect still `outcome_unknown`, `failed_terminal`, or unresolved `denied` blocks promotion.

Every evidence kind has a verification rule, and `EvidenceRef.result` MUST equal the verified outcome of the cited record; `pass` is permitted only when that outcome is successful, never merely claimed.

| Kind | Verification rule for `pass` |
|---|---|
| `test_run` | The cited record is a frozen validation-harness run or a canonical effect receipt naming the harness; the run's own outcome is `pass`; its subject hash and criterion id match. |
| `diff` | The cited record is a canonical diff artifact; its base/head digests match the work basis; any applied write has a matching `effect_receipt` with `succeeded` or `reconciled_effective`; its declared paths are inside `work.scope` and the ruling's `granted_scope`. |
| `observation` | The cited observation has required provenance and a verified capture; it may pass only a criterion whose `evidenceKinds` includes `observation`. |
| `human_attestation` | The cited record is `human_authority` authored by a human principal and bound to the same `subject_hash`. |
| `effect_receipt` | The frozen receipt verifier confirms the cited record is an `effect_receipt` appended by the frozen journal/broker/supervisor; the cited `effect_id`, `attempt`, `fencing_token`, and broker `request_id` all match; the authenticated producer class matches `producer_authority`; `subject_kind = "completion"` and `subject_hash` matches; and the receipt outcome is `succeeded` or `reconciled_effective`. |
| `replay_digest` | The cited record is a frozen replay-harness result whose recomputed digest equals the recorded `state_digest` at the same basis; a mismatch is `fail`, not `inconclusive`. |

A `failed`, `denied`, `failed_terminal`, `outcome_unknown`, or `reconciled_not_effective` receipt cited with `result="pass"` is an `EVIDENCE_OUTCOME_MISMATCH` and fails `ZT-05`. A retracted or suspect citation is rejected. A `reconciled_not_effective` effect blocks promotion for its bound criterion and permits a new effect under a new effect id and idempotency key; a `reconciled_effective` effect satisfies an `effect_receipt` criterion when its receipt verifies.

`effect_receipt` evidence is accepted only if the frozen receipt verifier confirms: the cited record is an `effect_receipt` appended by the frozen journal/broker/supervisor; the cited effect id, attempt, fencing token, and broker `requestId` all match the canonical receipt; the receipt's authenticated producer class matches `producer_authority`; and the receipt's `subject_hash` matches. `test_run` evidence is accepted only from the frozen validation harness or a canonical effect receipt naming a test execution with a verified runner identity; an agent- or plugin-authored claim of a passing test or receipt is a `candidate_claim`, never completion evidence. Promotion appends a `CompletionClaim` plus a lifecycle record; it never mutates the item row.

#### 4.4.5 External divergence

External changes MUST be recorded immediately and reconciled autonomously where non-conflicting. Kanbei MUST NOT adopt "do not push during generation" as its reconciliation model.

```ts
interface Divergence {
  id: RecordId;
  subject: { kind: "git_ref" | "file" | "external_record" | "service"; ref: string };
  base_generation: Digest;
  observed_generation: Digest;
  external_observation_id: RecordId;
  class: "non_conflicting" | "semantic_conflict" | "decision_conflict" | "acceptance_conflict";
  affected_work_items: RecordId[];
}
```

`Divergence` is an immutable payload. Its detected/triaged/reconciling/resolved/escalated/accepted_as_is state is a lifecycle projection (`family='divergence'`), and each transition appends a lifecycle record.

| From | Event | Actor | Guard | To |
|---|---|---|---|---|
| `detected` | `triage` | Loop or system | Class and affected work computed | `triaged` |
| `triaged` | `reconcile` | Loop | Class `non_conflicting` or `semantic_conflict`; no interpretation/acceptance change | `reconciling` |
| `triaged` | `escalate` | Loop or system | Class `decision_conflict` or `acceptance_conflict` | `escalated` |
| `reconciling` | `resolve` | Loop or system | Merge/observation appended; no speculative rewrite | `resolved` |
| `escalated` | `human_resolve` | Human | Ruling bound to affected receipt hashes | `resolved` |
| `escalated` | `accept_as_is` | Human | Explicit residual rationale | `accepted_as_is` |

| Class | Automatic action | Queue action |
|---|---|---|
| `non_conflicting` | Append merge/observation; create optional rebase work item | None |
| `semantic_conflict` | Create reconcile work item; block affected effects on the subject | Notify if promotion is pending |
| `decision_conflict` | Block affected work with `reason=divergence` | Escalate a decision request referencing prior receipts |
| `acceptance_conflict` | Block affected work; do not revoke acceptance | Escalate to the human with the accepted hash and the conflicting observation |

Rules:

- Speculative history MUST NOT be rewritten to match external state. Reconciliation produces new records, a new branch head, or a new base observation.
- Effect admission checks `precondition.observed_generation` against the subject's current generation. A mismatch denies the effect and emits a `Divergence`; this is the silent-external-clobber zero-tolerance gate.
- A conflict that changes interpretation, acceptance, or a prior decision MUST NOT resolve autonomously. It becomes a decision-queue item bound to the affected receipt hashes.
- External commits are not approvals. Commit authorship, messages, and merge actions are observations; they never amend intent or authorize a previously approval-required branch.

---

### 4.5 Intent, autonomy, approvals, and decision queue

#### 4.5.1 Intent anchor

```ts
interface IntentAnchor {
  id: RecordId;
  authority_class: "human_authority";
  author_id: HumanId;
  text: string;                         // exact; never normalized in place
  source: {
    kind: "chat" | "ui_gesture" | "imported_doc" | "api";
    ref: RecordId;
    spans?: { start: number; end: number; quote_hash: Digest }[];
  };
  amends?: RecordId;                    // amendment appends; does not rewrite
  veto_of?: RecordId;
  recorded_at_us: UtcUs;
  hash: Digest;
}
```

An amendment or veto is a new anchor linked to the prior one. The only mutable human-intent operation is append. A veto is effective immediately against future work in its declared scope; it never deletes prior effects or evidence and does not retroactively invalidate completed work without an explicit corrective action.

#### 4.5.2 Interpretation and acceptance

```ts
interface IntentInterpretation {
  id: RecordId;
  anchor_ids: RecordId[];
  source_spans: { anchor_id: RecordId; start: number; end: number; quote_hash: Digest }[];
  assumptions: string[];
  scope: DomainScope;
  plan_ref: RecordId;
  acceptance_criteria: Criterion[];
  delta_envelope: DeltaEnvelope;
  version: number;
  refines?: { parent_hash: Digest; kind: "detail" | "parameterization" };
  supersedes?: RecordId;
  author: AgentId | HumanId;
  authority_class: "candidate_claim" | "human_authority";
  hash: Digest;                         // covers all fields including plan and criteria
}

interface Plan {
  id: RecordId;
  interpretation_hash: Digest;
  parent_plan_hash?: Digest;            // recorded refinement parent; absent for a root plan
  delta_envelope: DeltaEnvelope;
  operations: Array<{ name: string; target: string; params_hash: Digest }>;
  hash: Digest;                         // SHA-256 over the canonical Plan bytes
}
```

`IntentInterpretation` is immutable. Its draft/proposed/accepted/rejected/superseded/expired/withdrawn state is a lifecycle projection (`family='interpretation'`), and the acceptance kind (`human` or `policy`) is recorded as `acceptance_kind` on the `accept` lifecycle record. `plan_hash` is the resolved `Plan.hash`, never a caller-declared digest: the append path resolves `plan_ref`, verifies `Plan.interpretation_hash`, and commits the resolved hash.

| From | Event | Actor | Guard | To |
|---|---|---|---|---|
| `draft` | `submit` | Author (agent or human) | ≥1 anchor span; `plan_ref` resolves to a canonical `Plan`; non-empty criteria; declared scope | `proposed` |
| `proposed` | `human_accept` | Human gesture endpoint | Human actor | `accepted` (`acceptance_kind=human`) |
| `proposed` | `policy_accept` | Frozen append path | Active autonomy rule tier is `automatic` or `review_after` for the whole scope; no effect class in scope is `forbidden` | `accepted` (`acceptance_kind=policy`) |
| `proposed` | `human_reject` | Human | Human actor | `rejected` |
| `proposed` | `supersede` | Author | New version cites the same anchors or an amendment anchor | `superseded`; new version `proposed` |
| `proposed` / `accepted` | `expire` | Frozen append path | Deadline passed | `expired` |
| `accepted` | `amend` | Author or human | New anchor + new version + new plan hash | Old `superseded`; new `proposed` |
| any nonterminal | `withdraw` | Author | Author only | `withdrawn` |

Invariants:

- Acceptance binds to the interpretation's `hash`; any edit produces a new hash and a new acceptance decision.
- A consequential branch requires an accepted interpretation whose hash covers the branch. `acceptance_kind=policy` is permitted only where human-owned policy authorizes it. Agent confidence is never an acceptance path.
- Exploration MAY proceed with `interpretation_hash = null` only when the branch is declared `branch_class=exploration`, its scope contains no consequential effect class, and the effective tier is `automatic`. Adoption of an interpretation later is an explicit `WorkItem.adopt` transition, not silent inheritance.
- A rejection binds the rejected hash. Re-queuing an identical interpretation and evidence set requires a new evidence hash or an explicit human `reopen`; otherwise the global queue suppresses it as a duplicate.

#### 4.5.3 Chat authority inference

The human message is canonical; its force is derived.

```ts
interface ChatMessage {
  id: RecordId;
  channel: "web" | "bridge" | "api";
  author_id: HumanId;
  text: string;
  attachments: BlobRef[];
  reply_to?: RecordId;                  // stamped by transport
  received_at_us: UtcUs;
  hash: Digest;
}

interface AuthorityInference {
  id: RecordId;
  message_id: RecordId;
  class: "advisory" | "directive" | "corrective" | "question" | "social";
  target_hint?: { anchor_id?: RecordId; work_item_id?: RecordId; decision_id?: RecordId };
  self_reported_confidence: number;     // uncalibrated; never authority-bearing
  rationale: string;
  model_call_ref: RecordId;
  authority_class: "candidate_claim";   // derived inference, never authority
}
```

| Inferred class | Permitted effect | Forbidden effect |
|---|---|---|
| `advisory` | Creates an intent candidate for human promotion or interpretation drafting | Any admission, approval, or tier change |
| `directive` | Creates a candidate plus a draft interpretation; triggers `policy_accept` only if active policy sets `chat_directive_acceptance=true` for the target scope and the tier is `automatic` or `review_after` | Creating a receipt; bypassing `approval_required`; accepting its own interpretation |
| `corrective` | Creates a correction proposal linked to its target; the receiving loop may voluntarily suspend itself | Changing work state, revoking a receipt, or invalidating a prior decision without human acceptance |
| `question` / `social` | No work authority | Same |

Classification is append-only. Reclassification is a new `AuthorityInference` with `supersedes`; no record upgrades its own authority class. For `approval_required` work, a literal "do X" chat message does not substitute for a ruling. The human MUST either dispose the decision-queue item or grant a standing policy before the fact.

Transport facts and judgment are separated: `reply_to` is a transport stamp; the decision to reply is a model decision recorded as an outbound `Message`.

#### 4.5.4 Autonomy policy

```ts
interface AutonomyPolicy {
  version: number;
  rules: Array<{
    id: string;
    priority: number;                  // tie-break only among equally restrictive matches
    match: {
      operation: string[];
      effect_class?: string[];
      data_class?: string[];
      reversibility?: "reversible" | "compensable" | "irreversible";
      blast_radius?: "single_artifact" | "project" | "multi_project" | "unknown";
      substrate?: "speculative" | "external";
    };
    tier: AutonomyTier;
    constraints?: Constraint[];
    review_window?: Duration;
    chat_directive_acceptance?: boolean;
    provisional_hold_on_corrective?: boolean;
  }>;
  effect_class_order: EffectClassOrder; // human-owned; no implicit string ordering
  override_limits: OverrideLimits;      // computed-count limits (§4.5.7)
  queue_limits: QueueLimits;            // request intake and deadline bounds (§4.5.6)
  effective_from_us: UtcUs;
  author_id: HumanId;
  hash: Digest;
}

type AutonomyTier = "automatic" | "review_after" | "approval_required" | "forbidden";

interface EffectClassOrder {
  classes: string[];                   // declared class names
  lessThan: Array<[string, string]>;   // strict edges a < b; transitive closure defines ≤
}
```

**Effect-class ordering is policy-owned and partial.** `effect_class_leq(a, b, order)` is true iff `a = b` or `b` is reachable from `a` through `lessThan`. `covers()` compares classes only through this relation; string comparison is forbidden. A class incomparable to the bound class has no coverage: `covers()` returns false and a fresh receipt is required. `max_effect_class` is an upper bound, not a "highest" value: every admitted effect class MUST be `≤` it. Adding or reordering a class is a human policy change, not an implementation choice; a cycle in `lessThan` rejects the policy.

**Rule resolution.** The effective tier is the most restrictive matching rule over `automatic < review_after < approval_required < forbidden`, raised to `approval_required` whenever an active override escalation (§4.5.7) covers the work, receipt, or loop. `priority` is consulted only to break ties among matching rules with the same tier; it can never let a lower tier outrank a more restrictive one. No match, or `blast_radius="unknown"`, yields `approval_required`. `forbidden` dominates all other matches.

```ts
interface TierProposal {
  id: RecordId;
  proposed_for: { operation: string[]; effect_class?: string[]; data_class?: string[] };
  from_tier: AutonomyTier;
  to_tier: AutonomyTier;
  rationale: string;
  author: AgentId;
  authority_class: "candidate_claim";
}
```

Agents MAY append `TierProposal` records; a proposal is a candidate claim and has lifecycle `proposed → accepted | rejected` (`family='tier_proposal'`). Only human acceptance writes a new `AutonomyPolicy` version.

| From | Event | Actor | Guard | To |
|---|---|---|---|---|
| (none) | `propose` | Agent | Target operation and scopes declared; no self-grant | `proposed` |
| `proposed` | `accept` | Human | Human writes a new `AutonomyPolicy` version | `accepted` |
| `proposed` | `reject` | Human | | `rejected` |

| Tier | Work admission | Effect admission | Completion promotion |
|---|---|---|---|
| `automatic` | Accepted interpretation required, except declared exploration | No receipt required; policy, budget, and precondition checks still apply | Automatic on valid evidence |
| `review_after` | Same | Allowed before review; receipt not required | Promoted with `review_outcome=pending`; human may object within the window, producing `reopened` |
| `approval_required` | Same | Requires an active receipt covering hash and scope | Requires an explicit promotion ruling |
| `forbidden` | Not admitted (`denied`, not a work state) | Denied | Not reachable |

#### 4.5.5 Approval request, receipt, and coverage

```ts
interface EvidenceSet {
  evidenceSetHash: Digest;              // SHA-256 over sorted (evidence_id, record_hash)
  evidenceIds: RecordId[];
  viewRevision: string;                 // presentation revision; not part of the evidence hash
}

interface ConsentManifest {
  requestId: RecordId;
  anchorId: RecordId;
  interpretationHash: Digest;
  planHash: Digest;
  workItemIds: RecordId[];
  workGenerationIds: RecordId[];
  completionSubjectHash: Digest;
  evidenceSetHash: Digest;
  evidenceIds: Array<{ id: RecordId; recordHash: Digest }>;
  grantedScope: DomainScope;
  grantedTier: AutonomyTier;
  maxEffectClass: string;
  constraints: Constraint[];
  reversibility: "reversible" | "compensable" | "irreversible";
  expiresAtUs: UtcUs;
  labels: Record<string, string>;       // sorted rendered labels and other display fields
  rendererVersion: string;
  viewRevision: string;
}

// kb.work/consent_display@1.0.0; canonical immutable record appended only by the frozen consent renderer (§4.1.2).
interface ConsentDisplayManifest {
  id: RecordId;
  request_id: RecordId;
  manifest: ConsentManifest;            // complete ruling-independent preimage of consentDigest
  consent_digest: Digest;               // SHA-256("kanbei.consent.v1" || canonical_json(manifest))
  consent_token_hash: Digest;           // hash of the single-use token; the token itself is never stored or exposed to the mutable UI
  rendered_at_us: UtcUs;
  authority_class: "coordination";
}

// Computed at token consumption; the render token itself is ruling-independent.
interface ConsentRulingCommitment {
  consent_digest: Digest;
  ruling: "approve" | "reject" | "defer" | "amend";
  consent_token_hash: Digest;
  ruling_commitment: Digest;            // SHA-256("kanbei.consent.ruling.v1" || consent_digest || 0x00 || ruling || 0x00 || consent_token_hash)
}

// kb.work/approval_request@1.0.0; immutable coordination payload and approval_request lifecycle subject.
interface ApprovalRequest {
  id: RecordId;
  anchor_id: RecordId;
  accepted_interpretation_hash: Digest;
  plan_hash: Digest;
  work_item_ids: RecordId[];
  work_generation_ids: RecordId[];      // exact current WorkGeneration records this request covers
  completion_subject_hash: Digest;
  evidence_set_hash: Digest;            // server-computed over the seeded authority evidence
  evidence_ids: Array<{ id: RecordId; record_hash: Digest }>;
  requested_scope: DomainScope;
  granted_scope: DomainScope;           // proposed grant the human will see and rule on
  requested_tier: AutonomyTier;
  max_effect_class: string;
  constraints: Constraint[];
  reversibility: "reversible" | "compensable" | "irreversible";
  envelope: DeltaEnvelope;
  deadline_us: UtcUs;                   // policy-clamped
  dedupe_key: Digest;                   // includes evidence_set_hash
  requester: Principal;
  requester_kind: "agent" | "plugin" | "human" | "system";
  batch_id?: string;
  supersedes?: RecordId;
  recorded_at_us: UtcUs;
  authority_class: "coordination";
}

// kb.work/authority_evidence@1.0.0; immutable coordination record.
interface AuthorityEvidenceRecord {
  id: RecordId;
  request_id: RecordId;
  work_item_ids: RecordId[];
  evidence_id: RecordId;
  record_hash: Digest;
  link_kind: "link" | "retract";
  retracts?: RecordId;                 // prior AuthorityEvidenceRecord id
  authority_affecting: boolean;        // false excludes completion evidence from the ruling basis
  author: Principal;
  recorded_at_us: UtcUs;
  authority_class: "coordination";
}
```

`consentDigest` commits the ruling-independent canonical manifest the human actually saw: `requestId`, anchor id, interpretation hash, plan hash, sorted `work_item_ids`, sorted `work_generation_ids`, completion subject hash, `granted_scope`, granted tier, max effect class, constraints, reversibility, sorted `evidence_id`/`record_hash` pairs, `evidence_set_hash`, `expires_at_us`, the frozen renderer's sorted `labels`, `rendererVersion`, and `viewRevision`. The human has not chosen a ruling when the surface renders, so no ruling is inside `consentDigest`. One render appends exactly one immutable `ConsentDisplayManifest` carrying the complete preimage and mints exactly one single-use token linked to it; that token authorizes exactly one explicit ruling gesture, and consumption records a `ConsentRulingCommitment` over the committed `consentDigest`, the chosen ruling, and the token hash. `viewRevision` is outside the evidence hash but inside `consentDigest`.

`ApprovalRequest` is an immutable coordination payload (`authority_class="coordination"`). Its `draft/queued/withdrawn/decided/expired` state is a lifecycle projection (`family='approval_request'`). At `submit`, the request's `evidence_ids` seed the request's `authority_evidence` links in the same transaction.

| From | Event | Actor | Guard | To |
|---|---|---|---|---|
| `draft` | `submit` | Requester (agent, plugin, or human) | Dedupe and queue limits pass; evidence hash server-computed; authority evidence seeded | `queued` |
| `queued` | `withdraw` | Requester | Request still open | `withdrawn` |
| `queued` | `decide` | Human gesture endpoint | Human session and origin/CSRF verified; single-use token valid and unconsumed and linked to a persisted `ConsentDisplayManifest`; ruling is `approve`, `reject`, or `amend`; `consentDigest` recomputed from that preimage, ruling commitment recomputed over `consentDigest`, ruling, and token hash, and manifest, evidence hash, work generations, scope, tier, max effect class, constraints, and expiry match; token consumption, ruling commitment, and ruling commit atomically | `decided` (ruling recorded) |
| `queued` | `defer` | Human gesture endpoint | Same checks; ruling is `defer`; ruling commitment recorded and token consumed atomically with the new policy-clamped `deadline_us` | `queued` (ruling recorded) |
| `queued` | `expire` | Frozen append path | Deadline passed with no ruling | `expired` |

**Pre-execution authority evidence.** `ApprovalRequest` seeds `authority_evidence` links from its `evidence_ids` at `submit`, and the operational projection records them as active and authority-affecting. Later evidence is added or retracted only by appending an `AuthorityEvidenceRecord`; the projection updates `authority_evidence_projection` in the same transaction. `evidence_current(work, evidenceSetHash, minSeq)` is true only when the server-computed hash over the active, authority-affecting evidence set for the request(s) bound to the work at `minSeq` equals `evidenceSetHash`; an addition or retraction changes that value and re-queues the request for a fresh ruling. Completion evidence in a `CompletionClaim` is not authority evidence and does not change `evidence_set_hash` unless an `AuthorityEvidenceRecord` explicitly marks it `authority_affecting=true`. Constraint overrides are not authority evidence: they never change `evidence_set_hash` and are evaluated only by `constraints_satisfied`; conversely, an authority-evidence link or retraction never relaxes a constraint.

**Approval display is frozen and independent of the mutable UI.** The minimal consent surface (§4.1.2) is the only renderer of approval evidence, scope, constraints, and hashes. A ruling gesture MUST carry `requestId`, `interpretation_hash`, `plan_hash`, `evidence_set_hash`, the explicit `ruling`, the single-use `consentToken`, and the `consentDigest` minted by that surface for the exact manifest it rendered. Because `consentDigest` is ruling-independent, one rendered page and its one token admit any single one of `approve`, `reject`, `defer`, or `amend`; the endpoint resolves the token's persisted `ConsentDisplayManifest`, recomputes `consentDigest` from that preimage, and records the `ConsentRulingCommitment` in the transaction that consumes the token. The gesture endpoint validates the Access/human identity, origin/CSRF state, token hash against the human session, request, full manifest digest, and expiry, consumes the token atomically with the ruling, and rejects with `APPROVAL_DISPLAY_MISMATCH` when the gesture digest, manifest, or ruling differs from the frozen surface's rendering; a reused, expired, or unknown token is rejected with `APPROVAL_TOKEN_INVALID`. The gesture endpoint recomputes `consentDigest` from canonical projections and rejects with `APPROVAL_DISPLAY_MISMATCH` when the gesture digest differs; it rejects with `APPROVAL_EVIDENCE_MISMATCH` when the evidence hash differs. A mutable UI may deep-link to the surface by `requestId`; it cannot supply evidence, labels, scope text, or a digest.

**Approval evidence is server-authoritative.** The frozen approval verifier computes `evidence_set_hash` from the canonical evidence records it will display; the request and receipt carry that value. A human ruling gesture MUST carry `requestId`, `interpretation_hash`, `plan_hash`, `evidence_set_hash`, the explicit `ruling`, and the `consentToken`. The endpoint recomputes the evidence hash and rejects with `APPROVAL_EVIDENCE_MISMATCH` unless the gesture's hash equals the server-issued hash for the request and the receipt's bound hash. Presentation (labels, ordering, summaries) remains mutable and MAY differ; the canonical evidence set and the bound growth hashes do not. This keeps the mutable UI outside the trust root while making the human's consent bind to what the frozen verifier computed.

```ts
interface ApprovalReceipt {
  id: RecordId;
  request_ids: RecordId[];
  ruling: "approve" | "amend";
  bound: {
    anchor_id: RecordId;
    accepted_interpretation_hash: Digest;
    plan_hash: Digest;
    work_item_ids: RecordId[];          // exact ruled work items
    work_generation_ids: RecordId[];     // exact WorkGeneration records selected when ruled
    subject_hash: Digest;               // current WorkGeneration completion subject
    evidence_set_hash: Digest;          // server-issued
    consent_digest: Digest;             // frozen surface display digest (§4.5.5)
    consent_display_id: RecordId;       // canonical ConsentDisplayManifest the human saw
    consent_ruling_commitment: Digest;  // over consent_digest, ruling, and consent_token_hash
    granted_tier: AutonomyTier;
  };
  envelope: DeltaEnvelope;              // exact refinements covered
  granted_scope: DomainScope;
  max_effect_class: string;             // ordered by the active AutonomyPolicy
  constraints: Constraint[];
  expires_at_us: UtcUs;
  post_promotion_revoke_until_us?: UtcUs;
  batch_id?: string;
  parent_receipt_id?: RecordId;
  author_id: HumanId;
  recorded_at_us: UtcUs;
}
```

A `reject`, `defer`, or `amend` ruling is recorded against the request bound to the consumed token's `ConsentDisplayManifest` and `ConsentRulingCommitment`; only `approve` or `amend` creates an active receipt. `defer` leaves the request queued with a new, policy-clamped deadline. `amend` supersedes the prior receipt and creates a new receipt bound to new hashes. A consent token is single-use and authorizes exactly one ruling: its `ConsentDisplayManifest` and token row commit in one transaction at render, and token consumption, the ruling commitment, the ruling, and the lifecycle transition commit in one transaction at gesture; a replay of the same token is rejected. `ApprovalReceipt` is immutable; `active/expired/revoked/superseded` is a lifecycle projection (`family='approval_receipt'`). A revocation is one revocation record per receipt, not a lifecycle-class record.

| From | Event | Actor | Guard | To |
|---|---|---|---|---|
| (none) | `issue` | Human gesture endpoint | Request `decided` with `approve`/`amend`; consumed token links a persisted `ConsentDisplayManifest`; `consentDigest` recomputed from its preimage and the ruling commitment recomputed over it, the ruling, and the token hash match; bound hashes recomputed | `active` |
| `active` | `expire` | Frozen append path | `now ≥ expires_at_us` | `expired` |
| `active` | `revoke` | Human gesture endpoint or frozen security component | A `Revocation` record for this receipt | `revoked` |
| `active` | `supersede` | Human gesture endpoint | New `amend` receipt bound to new hashes | `superseded` |

No row reactivates a receipt. Any `reactivate`/`reinstate`-shaped event is rejected with `TRANSITION_REJECTED`, including one appended by an agent or plugin.

```ts
interface Revocation {
  id: RecordId;
  subject_kind: "approval_receipt" | "constraint_override" | "enrollment" | "tier_exception" | "quarantine";
  subject_id: RecordId;
  reason: string;
  author_id: HumanId | SystemPrincipalId;
  recorded_at_us: UtcUs;
}
```


A revocation is a new record, never a mutation. Its `authority_class` is `human_authority` for a human actor and `security_action` for the frozen security component; the append path updates the revoked subject's projection in the same transaction (`approval_receipt`; `constraint_override` for a constraint override or override escalation; `tier_exception` for a tier exception; `enrollment`; `quarantine_scope` for a quarantine lift). `covers` observes it through that projection at `minSeq`; an effect admitted after that basis sees an inactive receipt.

```text
covers(receipt, work, effect, minSeq) :=
  lifecycle('approval_receipt', receipt.id, minSeq).state = "active"
  && now < receipt.expires_at_us
  && work.id ∈ receipt.bound.work_item_ids
  && current_work_generation(work, minSeq).id ∈ receipt.bound.work_generation_ids
  && work.anchor_id = receipt.bound.anchor_id
  && current_work_generation(work, minSeq).completion_subject_hash = receipt.bound.subject_hash
  && plan_binding(work, receipt.bound.plan_hash, minSeq)
  && evidence_current(work, receipt.bound.evidence_set_hash, minSeq)
  && work.scope ⊆ receipt.granted_scope                     // §4.2.1 per-field
  && effect_class_leq(effect.class, work.max_effect_class,
                      activePolicy(minSeq).effect_class_order)
  && effect_class_leq(work.max_effect_class,
                      receipt.max_effect_class,
                      activePolicy(minSeq).effect_class_order)
  && effect_class_leq(effect.class, receipt.max_effect_class,
                      activePolicy(minSeq).effect_class_order)
  && effect.target ∈ receipt.granted_scope                  // actual dispatch target, not a declaration
  && ( current_work_generation(work, minSeq).accepted_interpretation_hash = receipt.bound.accepted_interpretation_hash
       || refines_within(receipt.bound.accepted_interpretation_hash,
                         current_work_generation(work, minSeq).accepted_interpretation_hash,
                         receipt.envelope,
                         minSeq) )
  && criteria_unchanged(work, receipt.bound.accepted_interpretation_hash, minSeq)
  && constraints_satisfied(receipt.constraints,
                           active_overrides(work, minSeq),
                           work,
                           minSeq)
```

`effect` is the classified dispatch request the admission path is about to perform: its capability-derived effect class and resolved target. An approval never authorizes an effect merely because a `WorkItem` declares a compatible class or scope. `work.id ∈ receipt.bound.work_item_ids` binds coverage to the ruled work items, `current_work_generation(work, minSeq).id ∈ receipt.bound.work_generation_ids` and `current_work_generation(work, minSeq).completion_subject_hash = receipt.bound.subject_hash` bind the current generation and completion subject, and `plan_binding` and `evidence_current` bind the plan and the evidence the human saw.

`plan_binding(work, planHash, minSeq)` is true only when `current_work_generation(work, minSeq).plan_hash = planHash` and that generation's `plan_ref` resolves to a canonical `Plan` whose `hash` equals `planHash`; a refinement is accepted only through a recorded `Plan.parent_plan_hash` chain to `planHash`, never through a caller-declared envelope. `evidence_current(work, evidenceSetHash, minSeq)` is true only when the server-computed hash over the work's current canonical evidence set at `minSeq` equals `evidenceSetHash`; any evidence append, retraction, criterion change, or newer `CompletionClaim` changes the hash and stales the ruling.

The lifecycle state is read at the same `minSeq` basis as the work and constraint evaluation; a receipt revoked at or before that basis is inactive even if an older projection says `active`.

Refinement is evaluated on referenced plans, not declared envelopes. `refines_within` MUST resolve every step's `plan_ref` to a canonical `Plan` and require (a) each step's `Plan.hash` is the next step's `parent_plan_hash` up to `receipt.bound.plan_hash`, and (b) the resolved plan's `delta_envelope` and operations are contained in the receipt's `DeltaEnvelope` and `granted_scope`. A child interpretation that declares a narrow `delta_envelope` while referencing a plan that writes outside the granted scope does not refine; `covers` returns false and a fresh receipt is required. `refines_within` accepts only a recorded `refines` chain whose every step stays inside the receipt's `DeltaEnvelope` (permitted operations, paths, parameter ranges, child criteria derivations) and changes neither acceptance criteria, effect class, nor granted scope. Any other delta requires a fresh interpretation acceptance and receipt. This is the precise reading of "compliant child deltas may proceed without fresh approval."

`criteria_unchanged(work, acceptedHash, minSeq)` is true only when every `id`, `statement`, and `subjectHash` in the accepted interpretation's `acceptance_criteria` is present and equal in `work.acceptance_criteria`, every added criterion appears in the receipt's `envelope.childCriteria`, and no criterion was weakened. Order is irrelevant; equality is on the canonical criterion bytes.

`constraints_satisfied(originals, overrides, work, minSeq)` evaluates each constraint as a predicate over the work and its evidence. For each original constraint: if it evaluates true, it is satisfied; if it evaluates false, an override whose lifecycle state is `active` at `minSeq`, whose `failed_constraint_ids` contains that constraint, and whose replacement is weaker-or-equal must be present. A constraint with no evaluator is unsatisfied and non-overridable. A revoked override is inactive and does not satisfy anything.

**Constraint kinds and evaluator registry.** Constraint evaluation and weakening are closed over this registry. A `Constraint` whose `kind` is absent, or whose `params` do not validate against its row, is rejected at append and is non-overridable; there is no unregistered predicate.

| Kind | Params | Evaluator | Weaker-or-equal rule |
|---|---|---|---|
| `path_prefix` | `{path, mode}` | Path containment at `/` boundary; `deny` inverts | `c2.path` is a prefix of `c1.path` for `allow`; superset of denied paths for `deny` |
| `path_allowlist` | `{paths[]}` | Set membership by prefix | `c2.paths ⊇ c1.paths` |
| `numeric_range` | `{field, min?, max?}` | Integer comparison on the named field | `c2.min ≤ c1.min` and `c2.max ≥ c1.max` |
| `enum_allowlist` | `{field, values[]}` | Set membership | `c2.values ⊇ c1.values` |
| `effect_class_max` | `{value}` | `effect_class_leq` in the policy order | `c2.value ≤ c1.value` in the policy order |
| `data_class_allowlist` | `{values[]}` | Set membership | `c2.values ⊇ c1.values` |
| `forbidden_operation` | `{operation}` | Exact operation match denies | Only an identical or broader forbidden set is weaker; narrowing is never weaker |

**Constraint lattice and weakest precondition.** Define `c2 ≼ c1` ("`c2` is weaker than or equal to `c1`") iff, for every execution `e`, `wp(c1, post)(e) ⇒ wp(c2, post)(e)` for every postcondition `post`. The implementation MUST enforce only these decidable cases:

1. Same `kind`: apply that kind's row above.
2. Same kind with `id` changed: allowed only if `id` is not referenced by any human-authority record and the policy declares the kind renamable.
3. Different kinds: allowed only through an explicit `weaker_than` edge declared in the human-owned policy. There is no implicit kind ordering.
4. Unknown kind, unknown id, unregistered params, or an undeclared edge: not weaker, so the override is rejected with `CONSTRAINT_LATTICE_UNPROVEN`.

Coverage MUST be recomputed at every effect admission and every completion promotion; it MUST NOT be cached beyond a transaction. The verifier, tier resolver, and coverage predicate are frozen (§4.1.2); no candidate supplies them.

#### 4.5.6 Decision queue and batch rulings

There is one global decision queue. It projects requests grouped by `intent_anchor_id` and nearest policy-clamped `deadline_us`, with shared evidence deduplicated for presentation only.

- `dedupe_key` includes `evidence_set_hash`, so requests with different evidence sets never collapse into one row. Identical keys collapse into one queue row whose `request_ids` list grows only while the row is open and undisplayed.
- Once a row is displayed, its `request_ids` are frozen in a `batch_snapshot_seq`. A request arriving later is a separate row and is not covered by a ruling on the earlier snapshot. `BatchRuling` records the snapshot and the ordered ids it decided; "unconfigured items" cannot be silently added after review.
- Requests whose evidence sets differ but whose other fields match MAY be grouped visually, but each request MUST display its own bound hashes and MUST receive its own ruling; the queue MUST NOT show one decider for two evidence sets.
- Batch admissibility: every item shares the same `intent_anchor_id` and `evidence_set_hash`; no item is `forbidden`; no item's constraints conflict with another's; and every item is in the frozen snapshot. Otherwise the batch MUST be split.
- A `BatchRuling` record carries `batch_id`, `batch_snapshot_seq`, the ordered `request_ids`, a per-item exception map, and `common_evidence_hash`. It emits per-request receipts; each retains its own bound hashes and granted scope.
- Exceptions are explicit. A batch MUST NOT union scopes: work covered by one receipt is never covered by a sibling receipt.
- Batch receipts share `batch_id`, enabling one human revocation gesture that appends one revocation per receipt. Partial revocation is per receipt.

```ts
interface BatchRuling {
  id: RecordId;
  batch_id: string;
  batch_snapshot_seq: number;
  request_ids: RecordId[];              // frozen at display
  common_evidence_hash: Digest;
  per_item_exceptions: Record<RecordId, string>;
  ruling: "approve" | "deny" | "defer" | "amend";
  author_id: HumanId;
  authority_class: "human_authority";
}
```

```ts
interface QueueLimits {
  max_open_requests_per_principal: number;
  max_request_intake_per_window: number;
  min_deadline_us: Duration;
  max_deadline_us: Duration;
  window: Duration;
}
```

**Anti-abuse.** Request creation is rate-limited per principal and per loop by the frozen admission path: at most `queue_limits.max_open_requests_per_principal` open requests, and `queue_limits.max_request_intake_per_window` new requests per policy window. `deadline_us` is clamped to `[now + queue_limits.min_deadline_us, now + queue_limits.max_deadline_us]`; a caller cannot set urgency. Every request increments the requester's `decision_requests` counter in `LoopBudget`, and `LoopBudget` also tracks `decisions_consumed`. A principal over quota receives `DECISION_QUOTA_EXCEEDED`; the queue remains available to other principals.

#### 4.5.7 Agent constraint overrides

Approval binds to constraints, and agents have full discretion to reassess and override failed constraints with a recorded rationale, subject only to the hard guards below. The reconciliation is a schema distinction: **binding hashes and scope are hard for the agent; constraint satisfaction is defeasible delegated intent.**

```ts
interface ConstraintOverride {
  id: RecordId;
  receipt_id: RecordId;
  work_item_ids: RecordId[];
  failed_constraint_ids: string[];
  rationale: string;
  evidence_refs: RecordId[];
  replacement_constraints: Constraint[]; // weaker or equal in the declared lattice
  author: AgentId;
  recorded_at_us: UtcUs;
}
```

`ConstraintOverride` is immutable; `active/revoked` is a lifecycle projection (`family='constraint_override'`). Revocation appends a `Revocation` record and never deletes the override.

| From | Event | Actor | Guard | To |
|---|---|---|---|---|
| (none) | `propose` | Agent | Guards 1–5 hold; replacement proven weaker-or-equal | `active` |
| `active` | `revoke` | Human or frozen security component | A `Revocation` record for this override | `revoked` |
| (none) | `escalate` | Frozen append path | Receipt or loop override limit exceeded | `escalated` |
| `escalated` | `clear` | Frozen append path or human | Window advances or human ruling | `cleared` |

```ts
interface OverrideEscalation {
  id: RecordId;
  scope_kind: "receipt" | "loop";
  scope_id: RecordId;
  since_us: UtcUs;
  until_us: UtcUs;
  reason: string;
}
```

An `OverrideEscalation` is a lifecycle record (`family='constraint_override'`, states `escalated`/`cleared`) with its own subject id. Tier resolution consults `active_escalations(work, minSeq)` and raises the effective tier to at least the policy's `on_exceed` floor for the covered receipt or loop.

Overrides MUST satisfy all of these guards:

1. Only constraints that evaluated false may be overridden. A `reject` ruling is not a failed constraint and cannot be overridden.
2. An override cannot change `granted_scope`, `accepted_interpretation_hash`, `max_effect_class`, `envelope`, or the effective tier.
3. An override cannot touch `forbidden`, hard global ceilings, broker or sandbox policy, secret-exposure gates, or the `outcome_unknown` no-retry rule.
4. Unknown constraint IDs and undeclared lattice edges are non-overridable; the replacement MUST be proven weaker-or-equal by §4.5.5.
5. An active override is surfaced in the decision queue as a `review_after` item. Human revocation blocks future effects; it does not erase completed effects.
6. Override use is rate-limited by counting active overrides in the policy window: `count_active_overrides(receipt) ≤ policy.override_limits.per_receipt_per_window` and `count_active_overrides(loop) ≤ policy.override_limits.per_loop_per_window`. Exceeding either appends an `OverrideEscalation` (family `constraint_override`, event `escalate`) covering that receipt or loop; the frozen tier resolver then raises every subsequent admission and promotion under that scope to at least `on_exceed`, and no further override is accepted there until the window advances or a human clears the escalation. No work state is changed and no tier is stored on a work item.

```ts
interface OverrideLimits {
  per_receipt_per_window: number;
  per_loop_per_window: number;
  window: Duration;
  on_exceed: "approval_required";
}
```

`Overrides` are advisory delegated-intent relaxations; the count is computed from canonical override records plus `lifecycle_projection`, never from a mutable counter that could drift. Sandbox, broker, and global resource policies are separate hard human-owned boundaries.

---

### 4.6 Semantic memory and reconciliation

#### 4.6.1 Authority and derivability

| Thing | Canonical? | Authority |
|---|---|---|
| Human intent, approval, amendment, veto, curation, promotion | Yes | Human authority; non-defeasible as a record of human intent |
| Observation of source content | Yes | Evidence of capture, not truth |
| Agent claim, interpretation, classification | Yes | Candidate claim, never authority |
| Merge/split/alias/contradiction proposal | Yes | Derived proposal, never authority |
| Current semantic graph, aliases, working claims, contradictions, embeddings | No | Derived, versioned, rebuildable projection |
| Human promotion | Yes | Appends a linked human-attested assertion; does not certify objective truth |
| Retraction and quarantine | Yes | Retraction record or security action; derived effects are projection state |

The current semantic graph MUST be a derived, versioned, rebuildable projection of canonical inputs and curation. Agents MAY append identity merge, split, alias, contradiction, and other reconciliation proposals. Projections MAY apply them automatically only non-destructively: source identities remain stable and every operation is reversible by projection version.

#### 4.6.2 Semantic projection model

```ts
type ProjectionVersion = `${string}@${number}.${number}.${number}`;
type ClaimStatus =
  | "proposed" | "working" | "contested" | "superseded"
  | "retracted" | "suspect" | "excluded";

interface SuspectRef {
  recordId: RecordId;
  recordSeq: number;
}

interface SemanticEntity {
  entityId: EntityId;
  kind: string;
  canonicalLabel: string;
  aliases: AliasRef[];
  firstSeenSeq: number;
  mergedInto?: EntityId;               // logical view only; identity is not rewritten
}

interface AliasRef {
  aliasId: string;
  entityId: EntityId;
  label: string;
  sourceRecordIds: RecordId[];
  proposalRecordId?: RecordId;
}

interface SemanticClaim {
  claimId: string;                     // projection-local; never a binding referent
  subjectEntityId: EntityId;
  predicate: string;
  object: { kind: "entity"; entityId: EntityId }
        | { kind: "literal"; value: unknown; datatype: string };
  status: ClaimStatus;
  scope: DomainScope;
  validFromUs?: UtcUs;
  validToUs?: UtcUs;
  sourceRecordIds: RecordId[];
  derivationRecordIds: RecordId[];
  projectionVersion: ProjectionVersion;
  selfReportedConfidence?: number;     // uncalibrated metadata; never authority
}

interface ReconciliationOp {
  opId: string;
  kind: "merge" | "split" | "alias" | "contradiction" | "relink" | "scope";
  inputEntityIds: EntityId[];
  outputEntityIds: EntityId[];
  proposalRecordId: RecordId;
  projectionVersion: ProjectionVersion;
  appliedAtSeq?: number;
  revertedByVersion?: ProjectionVersion;
}

interface SemanticCoverage {
  asOfSeq: number;
  pending: number;
  suspect: SuspectRef[];               // ledger record ids plus their seq basis
  stale: boolean;
  rebuiltAtUs?: UtcUs;
  projectionVersion: ProjectionVersion;
}

interface EmbeddingArtifact {
  id: RecordId;
  sourceRecordIds: RecordId[];
  modelCallId: RecordId;               // recorded effect, not regenerated on rebuild
  vectorRef: BlobRef;
  dimensions: number;
  projectionVersion: ProjectionVersion;
}
```

`SuspectRef` joins the projection-side exclusion list to the ledger-side `suspect_record.record_seq`/`record_id`; neither store alone is authoritative for exclusion. An `EmbeddingArtifact` is a recorded semantic input: rebuilds MUST read `vectorRef` from the CAS and MUST NOT re-invoke the embedding model. Semantic projections that consume embeddings declare the class `recorded_effect` in §4.7.8, not `deterministic`.

#### 4.6.3 Reconciliation operations

| Operation | Non-destructive meaning | Reversal |
|---|---|---|
| Merge | Add equivalence and alias edges among stable entity IDs; select a preferred label for reads. No source identity is rewritten. | Rebuild or select a prior projection version. |
| Split | Create new entity IDs for differentiated clusters; retain the old entity and its claims; new claims carry derivation edges to the split proposal. | Rebuild or select a prior projection version. |
| Alias | Link a surface form or external identifier to an entity. | Remove by projection version. |
| Contradiction | Mark competing claims `contested`; keep all alternatives and derivation history visible. | Projection version or new proposal. |
| Relink | Move a claim or edge between subject identities with derivation provenance. | Projection version. |
| Scope | Record the read scope under which a claim is usable. | Projection version or retraction. |

A reconciliation proposal state machine:

| From | Event | Actor | Guard | To |
|---|---|---|---|---|
| `proposed` | `validate` | Agent, plugin, or system | Inputs exist; operation is reversible; scope is declared | `validated` |
| `validated` | `apply` | Semantic projection worker | Projection accepts the operation; no destructive identity rewrite | `applied` |
| `proposed` / `validated` | `reject` | Agent, plugin, or system | Evidence insufficient or operation destructive | `rejected` |
| `applied` | `revert` | Projection worker or human | A prior projection version is selected or a new reversing proposal is applied | `reverted` |

#### 4.6.4 Scoped resolver

```ts
interface Resolver {
  resolve(input: {
    subject: EntityId;
    predicate?: string;
    readerScope: DomainScope;
    projectionVersion: ProjectionVersion;
    minSeq?: number;
  }): Promise<{
    working: SemanticClaim | null;
    alternatives: SemanticClaim[];
    contested: boolean;
    coverage: SemanticCoverage;
    excluded: Array<{ claimId: string; recordIds: RecordId[]; reason: "retracted" | "suspect" | "quarantine" | "scope" }>;
  }>;
}
```

Rules:

- The resolver MUST exclude a claim when either (a) its semantic projection row is marked `suspect`/`retracted`, or (b) any of its `sourceRecordIds`/`derivationRecordIds` appears in ledger-side `suspect_record` or in an active `quarantine_scope`. Exclusion is the union, evaluated at read time; the asynchronous rebuild is not required to have completed.
- The resolver MAY prefer a working claim for action within `readerScope`, but contested status and alternatives MUST remain visible.
- Memory scope MUST be enforced at read time. A claim whose declared `DomainScope` is not contained in `readerScope` is excluded; write-time audits or note-text checks are not sufficient.
- `selfReportedConfidence` is explicitly uncalibrated metadata. It MUST NOT affect authority, automatic promotion, or approval, and SHOULD be displayed as uncalibrated.
- A claim still `proposed` is not a working claim. A `suspect` claim is excluded from current reads but remains inspectable.
- Coverage and pending work MUST be returned with every semantic read, and `coverage.suspect` MUST carry both `recordId` and `recordSeq` for every ledger-side suspect row.

#### 4.6.5 Retraction, suspect propagation, and quarantine

Retraction is a canonical record. It MUST take effect immediately at read time, before asynchronous rebuild completes:

1. Append the retraction record and its target link.
2. In the same append transaction, walk a bounded descendant batch over both `derivation_edge` and `ledger_link` rows of type `derives`/`informs`; record `pending_closure` for the remainder rather than holding the writer lock for an unbounded walk.
3. Insert `retraction_target` and `suspect_record` rows for the target and the walked batch. `suspect_record` carries both `record_seq` and `record_id`.
4. Mark dependent semantic projection rows `suspect` as an optimization. This mark is not the exclusion authority: readers MUST union it with the ledger-side `suspect_record`/`quarantine_scope` join by record id, so the window between commit and projection rebuild cannot return a retracted claim as current.
5. Trigger an asynchronous incremental rebuild of the affected subgraph; the worker drains `pending_closure` in bounded batches.
6. Preserve prior claims, derivation history, and the retraction record for inspection.
7. Schedule deeper audits on a configured cadence and perform non-destructive, evidence-preserving compaction.

A second retraction of the same target is rejected with `RETRACTION_CONFLICT`; a retraction cannot target an already retracted record. Append of a derivation from a retracted or suspect input is rejected with `DERIVATION_FROM_RETRACTED`, so a new record cannot silently recreate the escaped-dependency case.

Suspected poisoning follows the same exclusion mechanics with a broader scope:

- Append a `security_action` quarantine record.
- Insert an active `quarantine_scope` for the affected subgraph.
- Exclude all claims, derived multimodal artifacts, and disclosure nodes in that scope from current reads; the exclusion is applied to presentation and context projections as well, not only to retrieval.
- Trigger projection rebuild and human-visible security review.
- Lifting a quarantine requires a recorded human or frozen security action.

#### 4.6.6 Human promotion

Human promotion appends a `human_authority` record with a linked `kb.memory/promotion@1.0.0` component naming the promoted claim by its canonical `sourceRecordIds` and derivation record hashes, plus a stable projected claim key (`subjectEntityId + predicate + object hash + projectionVersion`), never the projection-local `claimId`. The semantic projection treats the result as a **human-attested assertion**, not objective truth. It MAY be preferred for action within its scope, but competing claims and their evidence MUST remain visible. Promotion MUST NOT erase alternatives, rewrite entity identity, or certify that the underlying observation is true. Because `claimId` is regenerated on rebuild, an attestation that names only `claimId` is invalid and MUST be rejected at append.

#### 4.6.7 Disclosure projection DAG

Disclosure is an arbitrary versioned projection DAG with coverage, provenance, and staleness metadata. Fixed world/feature/task levels MUST NOT be imposed, and disclosure depth MUST NOT be conflated with work depth.

```ts
type DisclosureSelector = {
  kind: "claim" | "entity" | "observation" | "artifact";
  id: string;
};

interface DisclosureNode {
  id: string;
  version: ProjectionVersion;
  parents: string[];                    // arbitrary DAG
  scope: DomainScope;
  selectors: DisclosureSelector[];      // claims, entities, observations, artifacts
  summary?: BlobRef;
  provenance: RecordId[];
  coverage: SemanticCoverage;
  staleness: "fresh" | "stale" | "unknown" | "suspect" | "quarantined";
}
```

Every disclosure output MUST carry its basis, projection version, coverage, provenance, and staleness. A consumer MUST NOT present stale or suspect disclosure as current.

---

### 4.7 Agent execution and durability

#### 4.7.1 Loop entity and states

A loop is a durable, persistent, self-triggering entity, not a turn. Kanbei does not require literal continuous model calls; wakeups, timers, and idle backoff determine when a loop reasons or acts.

```ts
interface AgentLoop {
  id: RecordId;
  identity_id: AgentId;
  subscriptions: RecordId[];
  budget_plan: LoopBudget;              // immutable plan; consumption is projected
  concurrency: { max_wakeups: number; max_effects: number };
  idle_backoff: { base_ms: number; cap_ms: number };
  watchdog_secs: number;
}

type LoopState =
  | "starting" | "idle" | "running" | "waiting_approval" | "waiting_external"
  | "backoff" | "suspended_budget_soft" | "suspended_budget_hard"
  | "suspended_policy" | "interrupted" | "stopping" | "stopped" | "failed";
```

`AgentLoop` is an immutable payload. Its lifecycle is `lifecycle_projection(family='loop')`. Mutable execution data is projected in `loop_runtime_projection`, never re-appended to the loop payload: the active branch set is derived from `Branch` records, budget consumption and active override deltas come from `BudgetOverride`/usage records, and `last_heartbeat_at_us`/`consecutive_idle` advance from heartbeat and wakeup records. Every checkpoint names the `wakeup_id` that produced it when it belongs to a wakeup; with concurrency > 1 a checkpoint without one is invalid. Concurrency above one is permitted only when every wakeup has its own checkpoint chain in the projected branch set; recovery resumes each interrupted wakeup from its own latest verified checkpoint and never merges two wakeups into one head.

| From | Event | Actor | Guard | To |
|---|---|---|---|---|
| `starting` | `recover` | Frozen loop scheduler | Branch set and inbox loaded from projections | `idle` |
| `idle` | `trigger` | Scheduler or human message | Hard budget, concurrency, and policy gates pass | `running` |
| `idle` | `idle_elapsed` | Scheduler | No pending trigger | `backoff` |
| `backoff` | `trigger` | Scheduler | Non-idle trigger | `running`; backoff reset |
| `backoff` | `backoff_elapsed` | Scheduler | | `idle` |
| `running` | `need_decision` | Loop | Coverage missing or approval required | `waiting_approval` |
| `waiting_approval` | `decision_applied` | Frozen append path | Receipt or override appended | `running` |
| `running` | `await_external` | Loop | Effect dispatched or long timer armed | `waiting_external` |
| `waiting_external` | `result` / `timer` | Scheduler | | `running` |
| `running` / `idle` / `backoff` | `heartbeat_timeout` | Watchdog | Exceeds step-class heartbeat budget | `interrupted` |
| `interrupted` | `resume` | Scheduler | A verified checkpoint exists for the interrupted wakeup | `running` |
| `interrupted` | `resume_degraded` | Scheduler | No verified checkpoint exists | `running` with `recovery_degraded` recorded; no silent fabrication |
| `suspended_budget_soft` | `soft_override` | Scheduler | Active `BudgetOverride` and hard ceilings hold | `running` |
| `suspended_budget_soft` | `soft_override_denied` | Scheduler | No active override or hard ceiling breached | stays `suspended_budget_soft` |
| any | `soft_budget_exceeded` | Scheduler | No active override | `suspended_budget_soft` |
| any | `hard_budget_exceeded` | Scheduler | | `suspended_budget_hard` |
| any | `policy_forbidden` | Scheduler | | `suspended_policy` |
| any | `stop` | Human or controller | | `stopping` → `stopped` |

A human message does not start a session; it enters the stream as a trigger/observation, and the loop decides whether to act. One wakeup yields at most one bounded decision, but that decision may admit multiple work items within concurrency policy.

#### 4.7.2 Wakeups, timers, backoff, and watchdog

```ts
interface Wakeup {
  id: RecordId;
  loop_id: RecordId;
  trigger: { kind: "event" | "timer" | "human" | "self" | "watchdog" | "resume"; ref: RecordId };
  dedupe_key: Digest;
  checkpoint_before: RecordId | null;   // attempt-scoped; a retry is a new attempt record
  checkpoint_after: RecordId | null;
  budget_snapshot_hash: Digest;
}
```

A wakeup's `pending/claimed/completed/failed/superseded` state is a lifecycle projection (`family='wakeup'`, subject = wakeup id); `claimed_by`, `lease_expires_at_us`, `attempts`, and `lease_fencing_token` are projection fields on that row, never payload fields. Claiming a wakeup is a compare-and-set transition: the intent MUST carry `expect.from="pending"` (or the projected lease state) and the current `lease_fencing_token`; the append path increments the token only on a successful claim. Two workers observing the same expiry cannot both claim: the second supplies a stale token and is rejected with `FENCING_STALE`. A reclaimed wakeup MUST NOT resume from another attempt's checkpoint: `checkpoint_before` is per attempt, and a stale claimant's late write is fenced out. After `max_wakeup_attempts`, the wakeup moves to `failed` and escalates a decision item.

| From | Event | Actor | Guard | To |
|---|---|---|---|---|
| `pending` | `claim` | Sandbox worker | Lease free or expired; `fencingToken` current; CAS `expect.from=pending` | `claimed` |
| `claimed` | `complete` | Claimant | Successor checkpoint durable; token current | `completed` |
| `claimed` | `fail` | Claimant | Token current; `attempts < max` | `failed` (retryable) or `failed` (terminal) |
| `claimed` | `lease_expired` | Frozen append path | `now ≥ lease_expires_at_us`; increments `attempts` | `pending` |
| `claimed` | `supersede` | Scheduler | Newer wakeup with the same `dedupe_key` | `superseded` |

- Wake signals coalesce last-wins per `dedupe_key`; payload messages are queued FIFO and never coalesced.
- At most one `running` wakeup per loop by default. Concurrency above one is explicit policy and consumes additional leases.
- Self-subscription is allowed but bounded. A self-wakeup MUST append a progress marker with new information or a declared no-op rationale. After `N` consecutive no-progress self-wakes, backoff doubles and the loop escalates a `loop_stalled` decision item.
- `Timer { id, loop_id, fire_at_us, dedupe_key }` has no stored state; `pending/fired/cancelled/missed` is lifecycle projection (`family='timer'`, subject = timer id). Transitions are `pending → fire | cancel | miss`; firing is CAS on `expect.from="pending"` with the scheduler's fencing token, so a recover-and-fire race yields exactly one `fired`. On recovery, missed timers fire once, latest-only; no catch-up storm.
- Idle backoff is `min(cap_ms, base_ms × 2^consecutive_idle)`, reset by any non-idle trigger.
- The watchdog appends a synthetic `idle` trigger when the loop is quiescent beyond `watchdog_secs`. A running loop is covered by a per-step heartbeat budget; an expired heartbeat moves it to `interrupted` rather than holding the watchdog off indefinitely.

#### 4.7.3 Budgets and hard ceilings

```ts
interface HardCeilings {                 // operator configuration; frozen trust root; ONE definition
  generation: NixGenerationId;
  window: "rolling_1h" | "calendar_day" | "calendar_month";
  spendUsd: number;
  tokensIn: number;
  tokensOut: number;
  modelRequestsPerWindow: number;
  brokerRequestsPerWindow: number;
  concurrentLoops: number;
}

interface LoopBudget {                   // soft
  tokens: number;
  spend_usd: number;
  wall_clock: Duration;
  model_calls: number;
  effect_calls: number;
  decision_requests: number;            // anti-abuse counter (§4.5.6)
  decisions_consumed: number;
  max_override_multiplier: number;
}

interface BudgetOverride {
  id: RecordId;
  loop_id: RecordId;
  rationale: string;
  delta: { tokens?: number; spend_usd?: number; wall_clock?: Duration; effect_calls?: number };
  expires_at_us: UtcUs;
  author: AgentId;
  authority_class: "candidate_claim";
}
```

- `HardCeilings` is defined once, here, and referenced by admission and reservations (§4.10.5). No second shape exists.
- Hard ceilings MUST be checked before model-call and effect admission inside the frozen trust path. Breach denies admission and appends `BudgetExceeded`; no partial dispatch.
- Soft breach pauses the loop at the next checkpoint and emits a `review_after` decision item. An agent MAY append a `BudgetOverride { rationale, delta, expires_at }`; the override cannot exceed hard ceilings or `max_override_multiplier`, and an active override moves `suspended_budget_soft` back to `running`. Human ownership of the multiplier and ceilings is explicit.
- Concrete numeric values are mandatory operator configuration and MUST be informed by cost measurements. Enforcement plumbing is built and validated against synthetic operator-configured limits before `LIM-1`; the final values are the human-approved output of `LIM-1`. A phase that cannot enforce a real ceiling still MUST enforce its configured provisional ceiling; it MUST NOT run with ceilings absent.

#### 4.7.4 Ledger-backed messaging and subscriptions

```ts
interface Message {
  id: RecordId;
  from: AgentId | HumanId;
  to: AgentId | HumanId | "broadcast";
  kind: "delegate" | "handoff" | "result" | "question" | "notice" | "decision";
  causation_id?: RecordId;
  correlation_id: RecordId;
  idempotency_key: Digest;
  payload_ref: BlobRef;
  recorded_at_us: UtcUs;
}

interface InboxItem {
  id: RecordId; consumer_id: AgentId; message_id: RecordId; dedupe_key: Digest;
}

interface OutboxItem {
  id: RecordId; producer_id: AgentId; message_id: RecordId;
  idempotency_key: Digest; target: string;
}

interface Subscription {
  id: RecordId; loop_id: RecordId;
  filter: { kinds?: string[]; namespaces?: string[]; entity_scopes?: DomainScope[] };
  mode: "enqueue" | "wake" | "coalesce";
  cursor: RecordId | null;
}
```

Inbox and outbox item states are lifecycle projections over coordination/lifecycle records; the interfaces carry no mutable `state` field. Transitions:

| From | Event | Actor | Guard | To |
|---|---|---|---|---|
| `pending` | `lease` | Consumer | Consumer lease available; fencing token current | `leased` |
| `leased` | `process_ok` | Consumer | Handler idempotent by `(consumer_id, message_id)` | `processed` |
| `leased` | `process_err_retryable` | Consumer | `attempts < max` | `failed_retryable` → `pending` |
| `leased` | `lease_expired` | Frozen append path | Crash or timeout | `pending` |
| `leased` | `attempts ≥ max` | Frozen append path | | `dead_letter` |
| any | `duplicate_delivery` | Frozen append path | Unique `(consumer_id, message_id)` already present | `skipped_duplicate` |

Outbox transitions:

| From | Event | Actor | Guard | To |
|---|---|---|---|---|
| `staged` | `commit` | Producer | Same ledger transaction as inbox insert | `committed` |
| `committed` | `dispatch` | Frozen transport | Ephemeral transport attempt | `dispatched` |
| `dispatched` | `ack` | Frozen transport | | `delivered` |
| `dispatched` | `transport_err` | Frozen transport | Retry budget remains | `failed_retryable` → `committed` |
| any | `give_up` | Frozen transport | | `dead_letter` |

Invariants:

- `Message`, recipient `InboxItem`, and producer `OutboxItem(committed)` are `coordination` records written in one SQLite transaction. Direct streams are post-commit optimizations whose outcomes are appended as delivery-attempt records.
- Uniqueness is enforced natively by the schema in §4.2.6: `inbox_dedupe(consumer_id, message_id)` and `outbox_dedupe(producer_id, idempotency_key)`. `OutboxItem` carries `idempotency_key`; `InboxItem.state` and `OutboxItem.state` are lifecycle projections over `coordination`/`lifecycle` records, not mutable canonical fields.
- Subscription cursors MUST advance only after the corresponding inbox insert commits. A crash between read and insert redelivers; dedupe removes the duplicate.
- Delivery is at-least-once. Kanbei MUST NOT claim exactly-once delivery.
- `wake` subscriptions coalesce; `enqueue` subscriptions preserve order per `(consumer, correlation_id)` and never coalesce payloads.

#### 4.7.5 Effects, journals, and `outcome_unknown`

```ts
type EffectSubjectKind = "completion" | "work" | "exploration" | "system";

interface Effect {                       // operational request view over the journal
  id: EffectId;
  subject_kind: EffectSubjectKind;
  work_item_id: RecordId | null;         // null for exploration/system effects
  attempt: number;
  idempotency_key: Digest;              // stable across retries of the same intent
  capability: string;
  target: string;
  params_hash: Digest;
  precondition: { observed_generation?: Digest; receipt_id?: RecordId };
  lease_fencing_token: number;
  started_at_us?: UtcUs;
  completed_at_us?: UtcUs;
  result_ref?: RecordId;
  error_class?: string;
}

type EffectState =
  | "intended" | "admitted" | "in_flight" | "succeeded"
  | "failed_retryable" | "failed_terminal" | "outcome_unknown"
  | "reconciled_effective" | "reconciled_not_effective"
  | "denied" | "cancelled" | "compensated";
```

`EffectState` is the effect journal's lifecycle projection. The canonical record is the single `EffectReceiptRecord` envelope:

```ts
interface EffectReceiptRecord {
  effect_id: EffectId;                  // journal-assigned at admission, never a sequencer RecordId
  subject_kind: EffectSubjectKind;      // completion | work | exploration | system
  work_item_id: RecordId | null;        // required when subject_kind = "completion" or "work"
  attempt: number;
  request_id: string;                   // broker/supervisor-minted at ingress
  fencing_token: number;                // lease token in force at dispatch
  idempotency_key: Digest;
  capability: string;
  target: string;
  producer: Principal;
  producer_class: "journal" | "broker" | "supervisor" | "system";
  outcome: "succeeded" | "failed" | "failed_terminal" | "denied" | "outcome_unknown"
         | "reconciled_effective" | "reconciled_not_effective";
  reconciles_receipt_id?: RecordId;    // present when a read-only probe resolves an earlier outcome_unknown receipt
  status_code?: number;
  request_digest: Digest;
  response_digest?: Digest;
  bytes_in?: number;
  bytes_out?: number;
  original_commitment?: Digest;         // pre-redaction commitment; no secret retained by default
  redacted_classes?: string[];
  subject_hash: Digest | null;          // present iff subject_kind = "completion"; matches CompletionClaim
  cost?: UsdMicros;
  error_class?: string;
  redaction: "full" | "partial" | "none";
  authority_class: "effect_receipt";
}
```

`EffectReceiptRecord` is the only canonical receipt shape. The `EffectReceipt` type in §4.10.7 is an alias for it, and `EffectOutcome` is an API result carrying its `EffectId` and canonical `RecordId`. It participates in the ledger hash chain and carries no separate `auditSeq` or `predecessorHash`; the ledger sequence and chain already provide those. Only the frozen journal, broker, supervisor, or system appender may write it. Receipt `outcome` is the canonical observation for one attempt and maps to the effect lifecycle: `succeeded`, `failed` (`failed_retryable`), `failed_terminal`, `denied`, `outcome_unknown`, `reconciled_effective`, and `reconciled_not_effective`. A read-only probe after `outcome_unknown` appends a new `EffectReceiptRecord` for the same `effect_id`, attempt, fencing token, and original `request_id` with `outcome` `reconciled_effective` or `reconciled_not_effective` and `reconciles_receipt_id` naming the superseded `outcome_unknown` receipt; the effect lifecycle and the probe receipt transition in one transaction. A `reconciled_effective` receipt is canonical completion evidence for an `effect_receipt` criterion; `reconciled_not_effective` is not.

`effect_id` is journal-assigned at admission and is not a `RecordId`; the sequencer still assigns the receipt's `RecordId`. `subject_kind` selects the semantics of `subject_hash`: `completion` requires a non-null `work_item_id` and `subject_hash` equal to that generation's `CompletionClaim.completion_subject_hash`; `work` requires `work_item_id` and a null `subject_hash`; `exploration` and `system` require a null `subject_hash` and may have a null `work_item_id`. Only a `completion` receipt can be cited as completion evidence; a model, search, read-only, or speculative effect records `exploration` and is audited without pretending to serve a completion subject.

| From | Event | Actor | Guard | To |
|---|---|---|---|---|
| `intended` | `admit` | Frozen journal or broker | Tier, receipt coverage, hard policy, budget, precondition, and lease fencing pass | `admitted` |
| `intended` | `deny` | Frozen journal or broker | Any hard guard fails | `denied` |
| `admitted` | `dispatch` | Frozen journal or broker | Effect journal row durable; fencing token current | `in_flight` |
| `in_flight` | `result_ok` | Frozen journal or broker | | `succeeded` |
| `in_flight` | `result_err_retryable` | Frozen journal or broker | `attempt < max` and declared idempotent or provider key supported | `failed_retryable` |
| `failed_retryable` | `retry` | Frozen journal or broker | Same `idempotency_key` | `admitted` (`attempt + 1`) |
| `in_flight` | `result_err_terminal` | Frozen journal or broker | | `failed_terminal` |
| `in_flight` | `crash` / `timeout` / `no_result` | Frozen journal or broker | Ambiguous termination | `outcome_unknown` |
| `outcome_unknown` | `probe_effective` | Frozen journal or human | Read-only external evidence | `reconciled_effective` (terminal for automation) |
| `outcome_unknown` | `probe_not_effective` | Frozen journal or human | Read-only external evidence | `reconciled_not_effective` (terminal for automation) |
| `outcome_unknown` | `human_authorized_new_attempt` | Human | New `EffectId`, new key, explicit receipt | `admitted` (new effect) |
| `reconciled_not_effective` | `new_effect` | Frozen journal or human | New `EffectId` and new key; fresh admission | `intended` (a distinct effect, not a retry) |
| `succeeded` | `compensate` | Frozen journal | New effect with a new key | `compensated`; original stays `succeeded` |

Hard rules:

- Journal before dispatch. A journaled step reduces duplicate work but does not make the external effect atomic with the journal; the ambiguous window is explicit.
- `outcome_unknown` is terminal for automation. It blocks completion promotion, appears in the work DAG and decision queue, and can be resolved only by a read-only probe or a human-authorized new attempt with a new key.
- `reconciled_effective` and `reconciled_not_effective` are terminal for automation and have no automatic outgoing transition. `reconciled_effective` satisfies a completion criterion whose receipt verifies. `reconciled_not_effective` blocks that criterion until a new effect under a new id/key succeeds; it does not wedge the work item forever and it is never retried under the old key.
- `failed_terminal` and `denied` are likewise closed for automation; a human-authorized new effect is a new record.
- Idempotency keys are mandatory. If the capability or provider supports no key and the effect is not safely repeatable, the effect is `non_idempotent`; admission requires at least `approval_required`, and ambiguous termination always yields `outcome_unknown`.
- `denied` is recorded. Attempted policy denials are audit evidence, not silent drops.
- Compensation is a separate effect and never assumes a perfect inverse. Residual external effects remain visible in undo views.
- A replay MUST NOT dispatch an effect. Effect results come from the journal.
- Every completion `EffectCitation` MUST match the canonical receipt on `EffectId`, attempt, fencing token, minted `request_id`, producer class, and `subject_kind="completion"` with the completion subject hash.

#### 4.7.6 Checkpoints and exact available model I/O

```ts
interface Checkpoint {
  id: RecordId;
  loop_id: RecordId;
  wakeup_id?: RecordId;                 // present for a wakeup chain; required when concurrency > 1
  branch_id: RecordId;
  work_item_id: RecordId | null;
  basis_seq: number;                    // ledger sequence
  state_digest: Digest;                 // deterministic reducer digest at basis
  open_effects: EffectId[];
  inbox_cursor: Record<string, RecordId>;
  pending_timers: RecordId[];
  model_io_head: RecordId;
  context_projection_version: string;
  plugin_state_refs: { store_id: string; snapshot_ref: BlobRef; journal_seq: number }[];
  created_at_us: UtcUs;
}
```

A checkpoint is a replay basis, not a full state copy. It MUST be created after each completed wakeup, before any effect dispatch, after approval mutations, and before a branch fork. `state_digest` is the digest of the deterministic reducer at `basis_seq`; a mismatch blocks resume and triggers replay diagnosis.

```ts
interface CaptureRedaction {
  policy: "redact_enrolled_secrets";
  redaction: "full" | "partial" | "none"; // full = all known values redacted
  redactedClasses: string[];            // e.g. "authorization", "cookie", "api_key"
  originalCommitment?: Digest;          // commitment over pre-redaction bytes; default omitted
  originalRetained: boolean;            // false unless a human enables broker-only retention
}

interface ModelCall {
  id: RecordId;
  loop_id: RecordId;
  wakeup_id: RecordId;
  attempt: number;
  retry_of?: RecordId;
  provider: string;
  model: string;
  routing_policy_version: string;
  request: {
    semantic_ref: BlobRef;              // exact agent-visible payload after redaction
    redacted_raw_ref: BlobRef;          // exact wire bytes after redaction
    redaction: CaptureRedaction;
    params_hash: Digest;
  };
  response: {
    semantic_ref: BlobRef;
    redacted_raw_ref: BlobRef;          // exact provider bytes after redaction
    redaction: CaptureRedaction;
    finish_reason: string;
    usage: Usage;
  };
  output_events_ref?: BlobRef;
  reasoning: { availability: "provided" | "redacted" | "absent"; artifact_ref?: BlobRef; note?: string };
  started_at_us: UtcUs;
  completed_at_us: UtcUs;
  correlation_id: RecordId;
}
```

**Capture point and exactness.** Capture happens in the broker/router boundary after credential redaction and before any agent-visible surface. The canonical store holds the exact *post-redaction* semantic payload and the exact post-redaction wire payload; those are the replay inputs. Credential-bearing bytes are not persisted by default. The record carries `redaction` metadata and, when the broker computes one, an `originalCommitment` digest over the pre-redaction bytes so verification can prove the redaction step ran without retaining the secret. Retaining unredacted provider bytes requires an explicit human-configured broker-only store; the ledger records only its digest, and agent-adjacent code never mounts it. A redacted payload is marked `redacted_for_agent` and MUST NOT be presented as byte-exact provider evidence.

"Exact available" now means the exact post-redaction payloads Kanbei sent and received, parameters, normalized output, output events, and any reasoning artifact the provider exposes. If reasoning is hidden, the record MUST say `availability="absent"`; hidden provider chain-of-thought MUST NOT be synthesized or summarized into the record. A retry is a new `ModelCall` linked by `retry_of`; existing records are immutable. Execution replay injects `response.semantic_ref` and `output_events_ref`; it never re-invokes the provider. calls the model for a past decision. "What would the model do now" is a new speculative branch, not replay.

#### 4.7.7 Undo forests and dependency-aware branch sets

```ts
interface Branch {
  id: RecordId;
  loop_id: RecordId;
  parent_branch_id: RecordId | null;
  fork_checkpoint_id: RecordId | null;
  head_record_id: RecordId;
  reason: string;
}

interface UndoRequest {
  id: RecordId;
  target_branch_ids: RecordId[];
  mode: "single" | "set";
  fork_checkpoints: RecordId[];
  rationale: string;
  author: HumanId | AgentId;
}

interface UndoBranchSet {
  id: RecordId;
  members: { branch_id: RecordId; fork_checkpoint_id: RecordId }[];
  affected_work_items: RecordId[];
  consistency: "none" | "per_branch" | "global_cut"; // explicit; never inferred
  residual_effects: RecordId[];
}
```

`Branch`, `UndoRequest`, and `UndoBranchSet` are immutable payloads. Their `active/frozen/undone`, `proposed/validated/applied/rejected`, and `proposed/validated/applied/partial` states are lifecycle projections (`family='branch'` and `family='undo'`), and every transition appends a lifecycle record.

| Family | From | Event | Actor | Guard | To |
|---|---|---|---|---|---|
| `branch` | (none) | `fork` | Loop or human | `fork_checkpoint_id` verifies | `active` |
| `branch` | `active` | `freeze` | Loop or human | No in-flight effect on the branch | `frozen` |
| `branch` | `active` | `undo` | Human or agent with undo authority | Undo marker appended; descendants preserved | `undone` |
| `undo` | (none) | `propose` | Human or agent | Target branches and fork checkpoints named | `proposed` |
| `undo` | `proposed` | `validate` | Frozen append path | Checkpoints verify; no approval/tier undo | `validated` |
| `undo` | `validated` | `apply` | Frozen append path | Residual effects recorded; consistency explicit | `applied` |
| `undo` | `proposed` / `validated` | `reject` | Human | | `rejected` |
| `undo` | `applied` | `partial` | Frozen append path | Some members applied; residual set recorded | `partial` |

Rules:

- Single undo MUST validate the checkpoint, fork a new `Branch` from `fork_checkpoint_id`, append an undo marker on the target, and mark the target branch `undone`. No record, checkpoint, or exact available model I/O is deleted, and all descendants remain.
- Descendants of an undone branch remain on their original lineage and receive a derived `contains_undone_ancestor` flag. Completed work stays completed; affected projections show `basis_superseded` and route review to the human.
- Undo MUST NOT undo an approval or a tier. Use revocation plus a new interpretation.
- Cross-agent branch sets MUST compute the transitive closure over `caused_by` and `blocked_by`, not just ownership descendants, and MUST record `affected_work_items` before applying.
- `consistency="global_cut"` is admissible only when all members share a common `basis_seq` and no external effect occurred after it. Otherwise reject with `NoConsistentCut`. Default `consistency="none"`; the UI and API MUST state partial undo explicitly rather than implying a consistent global rollback.
- External effects are never undone. They appear in `residual_effects`; compensation is a new effect with a new idempotency key.

#### 4.7.8 Replay contracts

"Replay" denotes three distinct contracts; conflating them is a known failure.

| Contract | Input | Output | Prohibited |
|---|---|---|---|
| `projection_rebuild` | Canonical records up to `basis_seq` plus reducer version | Operational or semantic projection at that basis | External effects |
| `execution_replay` | Checkpoint plus recorded `ModelCall`, effect results, and external observations up to `basis_seq` | Loop state at basis; new `state_digest` | Re-calling models or capabilities |
| `presentation_replay` | `ViewEvent` records, artifact refs, and presentation cursors | UI timeline at a declared basis | Mutation of domain state |

- Replay determinism is declared per projection. Operational projections and explicitly deterministic projections MUST rebuild to an identical digest from the same canonical inputs and frame. Semantic projections whose state depends on model outputs or embeddings declare `recorded_effect` and rebuild to an identical digest over *recorded* canonical inputs, recorded effect results, and recorded vectors; they MUST NOT re-invoke a model, embedding provider, or capability and MUST NOT promise that a re-run of generation would produce the same bytes (`00:84`, §4.7.8). Presentation replay reconstructs the recorded timeline rather than recomputing it.
- Execution replay MUST re-run deterministic loop code with journaled nondeterminism injected. It MUST NOT dispatch an effect; effect results come from the journal. A digest mismatch appends `ReplayDivergence` with the differing records and marks the affected projection `suspect`; the coverage watermark does not advance.
- Presentation replay MUST reconstruct what the human saw, including old cursors and catalog versions. It may run against an old basis while current state is newer; the basis MUST be displayed and carried in the payload.
- Kanbei MUST NOT promise bit-identical LLM regeneration or external-effect replay. Recorded post-redaction I/O is evidence of what occurred, not a generator.

#### 4.7.9 Crash and restart recovery

| Crash point | Durable fact | Recovery action | Visible result |
|---|---|---|---|
| Before ledger append | Nothing | Drop in-memory work; trigger may reoccur | No change |
| After `Effect(admitted)`, before dispatch | Intent only | Mark `failed_retryable`; retry same key | Effect pending |
| After dispatch, before result append | `in_flight` | Probe; if ambiguous → `outcome_unknown` | Unknown; no auto-retry |
| After result append, before checkpoint | Effect result durable | Replay from prior checkpoint; journal supplies result; re-checkpoint | Resumes |
| During inbox lease | Message + lease | Expire lease; redeliver | Duplicate possible; deduped |
| During message + inbox + outbox transaction | Atomic | Nothing to repair | Consistent |
| During projection update | Ledger durable, projection stale | Rebuild to basis before serving | Read-your-writes restored |
| During timer-fire append | Timer fired or not | Deduplicated fire-once | One wake |

Recovery algorithm (all steps append records):

1. Open the ledger read-write; verify the hash chain and last checkpoint digest; refuse to serve on integrity failure.
2. Acquire the single-writer lease with a fencing token; expire stale writer, inbox, concurrency, and effect leases.
3. Append `RecoveryEpoch { basis_seq, cause, actions }`.
4. Resolve every `in_flight` effect before resuming any loop: probe read-only where possible, otherwise `outcome_unknown`. Never dispatch a blind retry.
5. Rebuild operational projections whose digests mismatch; rebuild semantic projections asynchronously with explicit coverage.
6. Requeue `pending`/`leased` inbox items; re-arm `pending` timers; fire missed timers once, latest-only.
7. Convert every loop still `running` with an expired heartbeat to `interrupted`. For each interrupted wakeup, resume from that wakeup's own latest verified checkpoint in the projected branch set (`loop_runtime_projection.branch_set_json`, derived from `Branch` records). If no verified checkpoint exists for that wakeup, append `recovery_degraded` and resume from the loop's durable inbox plus recorded effect results; never fork silently from another wakeup's checkpoint.
8. Re-evaluate receipt expiry, revocations, overrides, and soft budgets; a paused loop does not auto-resume on a budget that is still exceeded, but an active `BudgetOverride` whose hard ceilings hold moves `suspended_budget_soft` to `running`. Expired wakeup leases return to `pending` with incremented attempts; stale fencing tokens cannot write.
9. Close `RecoveryEpoch` as `healthy` or `degraded`; `degraded` blocks completion promotion that depends on quarantined projections.

There is no second workflow authority. Durable timers, inboxes, journals, branch heads, and checkpoints are ledger records; the control plane's own recovery is the ledger's recovery.

---

### 4.8 Plugin kernel and self-modification

#### 4.8.1 Kernel scope and invariants

The kernel is a TypeScript process built from Nix and installed in the frozen trust root. It is the composition root; it is not a second source of domain truth. It borrows effect/coeffect, scoped registry, monotonic guard, revision-checked write, and append-only session patterns from Cordis and DeepSeek Harness, but their APIs MUST NOT define Kanbei's domain model.

| ID | Invariant |
|---|---|
| K1 | Every canonical write passes through the frozen append API. No mutable agent or plugin receives a canonical SQLite handle, path, or connection. |
| K2 | Plugin code executes outside the kernel address space, inside an OS sandbox. Node, Deno, Monty, Lua, Bash, and full CPython are inner runtimes only. |
| K3 | Authority is enforced at append time. Plugins and agents may append observations, proposals, derivations, policy-approved desired state, and the stable-domain components their class permits; they MUST NOT mint human authority, approvals, vetoes, activation receipts, effect receipts, security actions, or kernel generations. |
| K4 | Activation is per scope. A scope pointer flips atomically; no request observes a mixed candidate generation within one scope. Cross-scope changes are separate flips. |
| K5 | External effects are never rolled back by a pointer flip. Rollback compensates only kernel-owned resources; affected external effects receive new effect records and may be `outcome_unknown`. |
| K6 | Mutable plugin state is always scoped, separate from the canonical ledger, and either rebuildable from canonical inputs plus recorded effect results or explicitly checkpointed. |
| K7 | Unknown record components survive round trips. Projections declare the component names and versions they understand. |
| K8 | Replay rebuilds projections and recorded plugin state; it never repeats external effects and never promises bit-identical model regeneration. |
| K9 | Tool discovery is scope-filtered and versioned. `execute` resolves to an exact descriptor digest, and that digest is recorded. |
| K10 | Context-projection mutation cannot make a retracted record current, cannot hide a record from the audit path, and cannot bypass coverage/staleness reporting. |
| K11 | Trusted Nix updates are human-approved. The runtime never self-updates the kernel, and no automatic destructive down migration is permitted. |

#### 4.8.2 Contract handles

A plugin is a `PluginModule` that receives only contract handles:

```ts
interface PluginModule {
  manifest: PluginManifest;
  activate(ctx: PluginContext): Promise<PluginActivation>;
}

interface PluginContext {
  readonly scope: IsolationScope;
  readonly record: CanonicalAppend;
  readonly query: CanonicalQuery;
  readonly state: ScopedStore;
  readonly effects: EffectScope;
  readonly projections: ProjectionRegistrar;
  readonly capabilities: CapabilityClient;
  readonly loops: LoopRuntime;
  readonly views: ViewRuntime;
  readonly tools: ToolRegistrar;
  readonly models: ModelClient;
}
```

The Effect contract owns scoped runtime resources, not authority:

```ts
interface EffectScope {
  readonly id: EffectScopeId;
  effect<T>(
    name: string,
    acquire: () => Promise<T>,
    dispose: (value: T) => Promise<void>
  ): Promise<T>;
  onDispose(fn: () => Promise<void>): void;
  dispose(): Promise<DisposeReport>;
}

type EffectOutcome<T> =
  | { status: "succeeded"; value: T; receipt: RecordId }
  | { status: "failed"; error: StructuredError; receipt: RecordId }
  | { status: "outcome_unknown"; error: StructuredError; receipt: RecordId };
```

Disposers MUST run in reverse registration order, so dependent teardown completes before a dependency's effects are disposed. The kernel implements at-least-once attempts, records every attempt before the call and every result after it, and appends `outcome_unknown` on ambiguous interruption.

The Projection contract owns derived state and coverage:

```ts
interface Projection<S> {
  name: string;
  version: string;
  mode: "deterministic" | "async";
  determinism: "deterministic" | "recorded_effect"; // §4.7.8 rebuild class
  inputComponents: ComponentType[];
  inputVersions: Record<ComponentType, string>;

  initial(): S;
  reduce(state: S, record: RecordEnvelope): S;
  coverage(state: S): Coverage;

  checkpoint?(state: S): Promise<StateCheckpoint>;
  restore?(checkpoint: StateCheckpoint): Promise<S>;
}

interface Coverage {
  asOfSeq: number;
  pending: number;
  suspect: SuspectRef[];               // ledger ids plus seq basis; retraction-affected inputs
  stale: boolean;
  rebuiltAt?: UtcUs;
}
```

The Capability contract owns external authority:

```ts
type CapabilityName =
  | "fs.read" | "fs.write"
  | "process.spawn"
  | "net.direct"              // audit-only under the accepted egress policy
  | "broker.http"             // credential injection for enrolled logical hosts
  | "model.invoke"
  | "search.query"
  | "secret.use"
  | "canonical.append" | "canonical.query"
  | "plugin.state"
  | "tool.register" | "view.publish"
  | "loop.spawn";

interface CapabilityRequest {
  name: CapabilityName;
  constraints: unknown;               // discriminated by name
  reason: string;
}

interface CapabilityGrant {
  id: string;
  scope: IsolationScope;
  request: CapabilityRequest;
  grantedBy: RecordId;                // human authority or policy record
  expiresAt?: UtcUs;
  revokedBy?: RecordId;
}

interface CapabilityBroker {
  decide(grant: CapabilityGrant, request: CapabilityRequest): Promise<Decision>;
  invoke<T>(grant: CapabilityGrant, request: CapabilityRequest): Promise<CapabilityResult<T>>;
}
```

Capability constraints are capability-specific. Filesystem grants name roots and modes; process grants name executables and concurrency; model grants name task classes and per-call token ceilings; secret grants name logical secret identifiers; canonical grants name component namespaces and maximum records per interval. `net.direct` records destination and attribution but MUST NOT deny by host or port. `broker.http` names an enrolled logical host and is subject to the broker contract in §4.10.6.

The Loop contract exposes runtime handles over durable loop records:

```ts
interface LoopRuntime {
  spawn(spec: LoopSpec): Promise<LoopHandle>;
  subscribe(spec: SubscriptionSpec): Promise<Subscription>;
  branch(checkpoint: StateCheckpoint): Promise<LoopHandle>;
}

interface LoopSpec {
  id: LoopId;
  parent?: LoopId;
  goal: RecordId;
  autonomy: AutonomyTier;
  budget: LoopBudget;
  grants: CapabilityGrant[];
}
```

The View contract owns presentation, never canonical domain state:

```ts
interface CurationGestureSpec {
  kind: CurationGesture["kind"];
  label: string;
}

interface ViewContribution {
  id: string;
  slot: string;
  cardinality: "single" | "list" | "keyed" | "chain";
  spec: unknown;                       // renderer-neutral catalog/artifact
  gestures: CurationGestureSpec[];
}

interface ViewRuntime {
  register(contribution: ViewContribution, scope: EffectScope): Promise<Dispose>;
}
```

A view contribution's lifetime is its effect scope. `ViewRuntime` does not append gestures: judgment gestures are routed through the frozen consent surface and its human-gesture endpoint (§4.1.2), which verifies a human principal and appends merge, split, link, scope, correction, retraction, or promotion records through the frozen append path. Plugins and agents MUST NOT invoke that endpoint. Views MUST NOT write domain state directly. Arbitrary plugin artifacts are isolated behind an MCP Apps-compatible sandboxed boundary.

```ts
interface HumanGestureEndpoint {
  submit(input: {
    gesture: CurationGesture;
    requestId?: RecordId;               // approval gestures only
    ruling?: "approve" | "reject" | "defer" | "amend";
    evidenceSetHash?: Digest;           // required for approval gestures (§4.5.5)
    consentToken?: string;              // required for approval gestures; single-use, minted by the consent surface
    consentDigest?: Digest;             // required for approval gestures; minted by the consent surface
    origin: string;                     // must equal the frozen consent origin
    csrfToken: string;                  // bound to the host-only consent session
  }): Promise<AppendReceipt>;
}
```

The endpoint derives the human identity and session from the verified Access session; callers cannot supply either. It validates the frozen consent origin and CSRF token, the host-only `SameSite=Strict` consent session, the single-use token hash bound to that session, `requestId`, the full displayed-manifest digest, and expiry; it reloads the token's persisted `ConsentDisplayManifest`, recomputes `consentDigest` from that preimage, and commits the `ConsentRulingCommitment` over `consentDigest`, the explicit ruling, and the token hash atomically with token consumption. A mismatched digest, manifest, or ruling is rejected with `APPROVAL_DISPLAY_MISMATCH`; a reused, expired, unknown, or cross-origin token is rejected with `APPROVAL_TOKEN_INVALID`. The mutable UI may deep-link to the surface but cannot supply its content, session, token, or digest; the frozen surface is served from a separate origin with no shared DOM, script context, or browser storage.

The endpoint was previously exposed as `ViewRuntime.acknowledge`. Removing it closes the authority bypass: a plugin cannot append a human-authority record by calling a view method.

Composition rules:

1. Activation creates an `EffectScope`, resolves declared coeffects, and registers projections, tools, and views through that scope.
2. Every effect attempt/result, tool execution outcome, model call, activation, and context-projection activation appends a canonical record through `CanonicalAppend`.
3. Projections read canonical records and authorized `CanonicalQuery` views; they never read another scope's plugin state.
4. Capabilities are checked before external calls; effect receipts are written after. Neither capability grants nor effects can create human authority.
5. Loops are the only contract that schedules work; they consume projections, use capabilities, and append records.
6. Views render projections and append gestures; they are not authoritative.
7. No contract exposes the raw kernel, the sequencer, the canonical database, the trust-root filesystem, or another scope's state.

#### 4.8.3 Scopes, manifests, and scoped state

```ts
interface ScopeNode {
  id: IsolationScope;
  kind: "agent" | "plugin" | "project" | "global";
  parent?: IsolationScope;
  generation: number;
}
```

`ScopeNode` and `ScopePointer` are isolation-scope structures; their `id`/`scope` values are `IsolationScope`, never `DomainScope`.

A scope determines service visibility, tool/view visibility, state partition, and capability namespace. Scopes form a dependency graph. Activation starts dependencies before dependents in topological order; teardown reverses that order. A plugin declares required and optional dependencies with contract-version ranges; a dependency change that violates a range fails validation before activation.

The kernel MAY co-locate plugins in one OS-sandboxed process when they share a scope and trust class. Inter-agent isolation is for crash and resource containment, not a required security boundary; the outer OS sandbox and the broker protect host and secrets. Where per-scope bind mounts are cheap, the supervisor uses them to reduce accidental cross-scope reads, but Kanbei does not promise cross-agent state confidentiality.

```ts
interface PluginManifest {
  id: string;
  version: string;
  contractVersion: string;
  scope: IsolationScope;
  entry: ContentRef;                   // content-addressed artifact
  dependencies: Array<{ id: string; range: string; contractVersion: string }>;
  capabilities: CapabilityRequest[];
  state: Array<{
    store: string;
    dataVersion: number;
    durability: "rebuildable" | "checkpointed";
  }>;
  projections: Array<{ name: string; version: string; mode: "deterministic" | "async"; determinism: "deterministic" | "recorded_effect" }>;
  tools: Array<{ name: string; version: string; digest?: string }>;
  views: Array<{ id: string; slot: string }>;
}
```

The manifest is declarative. Undeclared capabilities, undeclared state stores, and undeclared cross-scope dependencies are validation errors. A plugin MUST NOT import host modules by bare specifier; the kernel exposes only `PluginContext` handles. Build artifacts are content-addressed and immutable at activation; activation never runs package install scripts.

Plugin state is durable, mutable, and non-canonical. It uses a separate scoped store, never the canonical SQLite database:

```ts
type StateKey = string;

interface StateTx {
  get<T>(key: StateKey, codec: Codec<T>): Promise<T | undefined>;
  put<T>(key: StateKey, value: T, codec: Codec<T>): Promise<void>;
  delete(key: StateKey): Promise<void>;
}

interface ScopedStore {
  readonly scope: IsolationScope;
  readonly name: string;
  read<T>(key: StateKey, codec: Codec<T>): Promise<T | undefined>;
  transact<T>(
    fn: (tx: StateTx) => Promise<T>,
    opts: { idempotencyKey: string; checkpoint?: boolean }
  ): Promise<T>;
  checkpoint(): Promise<StateCheckpoint>;
  restore(checkpoint: StateCheckpoint): Promise<void>;
}

interface StateCheckpoint {
  scope: IsolationScope;
  store: string;
  dataVersion: number;
  seq: number;
  digest: string;
  canonicalCursors: Record<string, number>;
}
```

Storage lives under `~/.local/state/kanbei/plugin-state/<scope>/<store>/`, outside the canonical ledger. The plugin never receives a filesystem path or SQLite handle for this store; it receives the `ScopedStore` RPC handle through the `plugin.state` capability. The store uses its own transactional metadata and enforces per-store size and rate ceilings.

Durability modes:

- `rebuildable`: every durable value is a pure function of an authorized canonical query plus recorded effect results. The projection rebuild is deterministic. Arbitrary writes outside `ScopedStore.transact` are rejected.
- `checkpointed`: transactions are journaled, and the kernel appends a scoped-state checkpoint record at commit boundaries or configured intervals. The checkpoint records scope, store, `dataVersion`, canonical cursors, and digest. Restore uses the checkpoint plus subsequent journaled mutations; it does not replay external effects.

Arbitrary in-memory plugin state is not replayable. Durable state MUST be a rebuildable fold or a checkpointed transaction log. A plugin that keeps non-rebuildable, non-checkpointed durable state fails conformance.

Canonical access is narrow. A plugin submits a logical draft; the frozen append API validates it, materializes the canonical envelope, and returns a receipt:

```ts
interface RecordDraft {
  authority?: AuthorityClass;           // omitted means no authority claim; principal and scope come from the channel
  subject?: EntityId;
  parents?: RecordId[];                 // explicit causal links
  cause?: RecordId;
  components: ComponentEnvelope[];
}

interface AppendReceipt {
  ids: RecordId[];
  firstSeq: number;
  lastSeq: number;
  headHash: Digest;
}

interface CanonicalAppend {
  append(draft: RecordDraft, opts: { idempotencyKey: string }): Promise<AppendReceipt>;
  link(from: RecordId, link: WorkLink, opts: { idempotencyKey: string }): Promise<AppendResult>;
  transition(intent: Omit<Extract<AppendIntent, { kind: "transition" }>, "actor" | "scope">): Promise<AppendResult>;
  retract(target: RecordId, reason: string, opts: { idempotencyKey: string }): Promise<AppendResult>;
}
```

`actor` and `scope` are never caller-supplied; the frozen append API materializes them from the authenticated channel and rejects a mismatched explicit value. The idempotency key is scoped by `(actor, scope)`, so two principals cannot collide or block each other.

The append handle is bound to the caller's `IsolationScope`. It MUST reject `human_authority`, `delegated_authority`, `activation_receipt`, `security_action`, `effect_receipt`, `kernel_generation`, promotions, attestations, and any stable-domain component outside the (class, appender role, component type) matrix of §4.2.2 from a plugin or agent; reject arbitrary `lifecycle` payloads (a plugin may request only the transitions its authority permits); reject component names outside the scope's declared allowlist; reject records whose subject `DomainScope` is outside the scope; reject oversized batches or payloads above configured admission limits; and reject duplicate idempotency keys with differing bytes. Direct canonical SQLite access is denied three ways: no handle or path is exposed; the OS sandbox does not bind the canonical database; and a hostile direct-open attempt is a zero-tolerance `ZT-02` failure.

Direct canonical SQLite access is denied three ways: no handle or path is exposed; the OS sandbox does not bind the canonical database; and the append API is the only writer. An attempted open is a structured security violation, appended to the audit path, and quarantines the candidate that attempted it.

#### 4.8.4 Candidate staging, validation, activation, and rollback

```ts
// Mutable staging under mutable/candidates/<scopeId>/<candidateId>/; never canonical.
interface CandidateWorkspace {
  id: CandidateId;
  scope: IsolationScope;
  generation: number;
  createdAt: UtcUs;
  base: CandidateId | null;            // active candidate at staging time
  artifact?: ContentRef;                // code bundle or declarative doc set once staged
  manifest?: PluginManifest;
  dependencyLock?: DependencyLock;
  capabilityRequests: CapabilityRequest[];
  schema?: { contractVersion: string; dataVersion: number; migrations: MigrationRef[] };
  validation?: ValidationReport | null;
  diff?: CapabilityDiff | null;
  health?: HealthReport | null;
}

// kb.state/candidate_manifest@1.0.0; immutable canonical payload and candidate lifecycle subject.
interface CandidateManifest {
  id: CandidateId;
  scope: IsolationScope;
  generation: number;
  base: CandidateId | null;
  artifact: ContentRef;                // exact staged artifact digest
  manifest_digest: Digest;             // hash of the exact PluginManifest
  dependency_lock_digest: Digest;
  capability_requests_digest: Digest;
  schema: { contractVersion: string; dataVersion: number; migrations: MigrationRef[] };
  validation_report: ContentRef | null;
  capability_diff: ContentRef | null;
  health_report: ContentRef | null;
  created_at_us: UtcUs;
}

type CandidateState =
  | "staged" | "validated" | "diffed" | "policy_pending"
  | "prepared" | "activating" | "active"
  | "rejected" | "failed_health" | "rolled_back" | "quarantined" | "retired";
```

The mutable staging artifact is `CandidateWorkspace`; it is agent-authored, lives under `mutable/candidates/<scopeId>/<candidateId>/`, and is never canonical. At `stage` the frozen append path materializes an immutable `CandidateManifest` from the workspace's artifact, manifest, dependency lock, capability requests, and schema; validation, diff, and health reports are content-addressed and referenced by digest; `diffed` means the capability diff against the base candidate is computed and content-addressed. `staged` means the artifact is in the CAS and the manifest parses. `validated` means static and dynamic validation passed and the diff exists. `policy_pending` means a broadening capability or data migration requires human approval under autonomy policy. `prepared` means the shadow state generation and dependency resolution are ready. `activating` means the pointer flip is in progress. `active` means post-flip health passed.

| From | Event | Actor | Guard | To |
|---|---|---|---|---|
| (none) | `stage` | Author (agent or human) | Artifact in CAS; manifest parses; immutable `CandidateManifest` appended | `staged` |
| `staged` | `validate` | Frozen activation supervisor | All eight validation steps pass | `validated` |
| `staged` | `validate` | Frozen activation supervisor | Any validation step fails | `rejected` |
| `validated` | `diff` | Frozen activation supervisor | Capability diff against the base candidate computed and content-addressed | `diffed` |
| `diffed` | `request_policy` | Frozen activation supervisor | Broadened capability or migration | `policy_pending` |
| `diffed` | `prepare` | Frozen activation supervisor | No policy escalation required; shadow state ready | `prepared` |
| `policy_pending` | `prepare` | Frozen activation supervisor | Policy satisfied; shadow state ready | `prepared` |
| `prepared` | `activate` | Frozen activation supervisor | Scope lock; dependency locks; gate held | `activating` |
| `activating` | `commit` | Frozen activation supervisor | `ActivationReceipt` appended with the health report; pointer and state generation repointed | `active` |
| `activating` | `fail_health` | Frozen activation supervisor | Health failure; pointer restored to the previous candidate and state generation; new effects disposed; failure receipt appended | `failed_health` |
| `failed_health` | `rollback` | Frozen activation supervisor | Rollback complete; the failed candidate remains for diagnosis | `rolled_back` |
| `active` | `rollback` | Frozen activation supervisor | Health failure or human rollback | `rolled_back` |
| `active` | `retire` | Frozen activation supervisor | Replacement candidate committed to `active` | `retired` |
| any | `quarantine` | Frozen security component | Security probe failure | `quarantined` |

No row leaves `retired` or `rolled_back`; a retired candidate is retained only for rollback forensics and is never reactivated. Every successful activation appends `retire` for the previous candidate, so its lifecycle subject records the retention.

Validation MUST run in this order and append a `CandidateValidated` or `CandidateRejected` record:

1. **Manifest and schema validation.** Contract versions, dependency ranges, state-store declarations, tool/view names, and migration descriptors.
2. **Dependency resolution.** The full closure is available at compatible versions. Missing or ambiguous dependencies reject.
3. **Static capability scan.** Declared imports and host calls are compared with `capabilityRequests`. An undeclared `fs.write`, `process.spawn`, `broker.http`, `model.invoke`, `secret.use`, `canonical.append`, or `canonical.query` rejects.
4. **Artifact build.** The code bundle is built in a sandbox with a pinned toolchain and lockfile. Build output is content-addressed; activation uses the immutable output, not a fresh build.
5. **Dynamic conformance.** The candidate runs in a quarantine scope with all external capabilities denied. It must boot, register its declared projections/tools/views, pass contract conformance tests, and fail closed on denied calls.
6. **State migration dry-run.** Forward migrations run against a shadow copy of the scope state. Live state is untouched.
7. **Security probes.** Attempts to open canonical SQLite, read the trust root, read another scope's state, read broker configuration, or write the Nix store MUST fail closed and produce an auditable error.
8. **Health-plan validation.** Declared health checks must be executable without unresolvable external dependencies; any external probe must name its effect class and idempotency behavior.

```ts
interface CapabilityDiff {
  added: CapabilityRequest[];
  removed: CapabilityRequest[];
  changed: Array<{
    before: CapabilityRequest;
    after: CapabilityRequest;
    broadening: boolean;
  }>;
  dependencyChanges: Array<{ id: string; before: string; after: string; compatible: boolean }>;
  migrations: Array<{
    store: string;
    from: number;
    to: number;
    mode: "compatible" | "forward-only" | "requires-checkpoint";
  }>;
  trustRootAccess: boolean;            // must always be false
}
```

Diff policy:

- `trustRootAccess: true` MUST reject unconditionally.
- Added or broadened `secret.use`, `model.invoke`, `broker.http`, `process.spawn`, `fs.write`, or `canonical.append` requires human approval under the autonomy tier unless human-owned policy explicitly delegates it.
- Removed or narrowed grants MAY activate automatically after health passes.
- A `forward-only` migration activates only with an explicit `roll_forward_only` decision; rollback is unavailable.
- `requires-checkpoint` requires a verified state checkpoint before preparation.
- `net.direct` changes are recorded in the diff but are not denied, because accepted egress policy makes it audit-only. This is a deliberate exception, not a claim that `net.direct` is a containment control.

Each scope has a trust-root-owned activation pointer, separate from plugin-writable state:

```ts
interface ScopePointer {
  scope: IsolationScope;
  generation: number;
  candidateId: CandidateId;
  stateGeneration: number;
  activatedAt: UtcUs;
  activationReceipt: RecordId;
  prev: { candidateId: CandidateId; stateGeneration: number } | null;
}
```

The pointer MUST be published with a write-temp, `fsync`, `rename`, `fsync`-directory sequence, or an equivalent atomic transaction in the trust-root control store. The plugin cannot write the pointer directory. State generations are immutable directories; the pointer selects code and state generation together. Activation is serialized per scope. Cross-scope activations acquire locks in lexical scope-id order to avoid deadlock; nested activation from inside an activation is rejected.

Activation protocol:

1. **Lock and intent.** Acquire the scope lock and dependency locks. Append `ActivationRequested` with candidate digest, base candidate, diff digest, and migration modes.
2. **Snapshot.** Record the active pointer and a state checkpoint for every declared store.
3. **Shadow preparation.** Materialize the new state generation from the checkpoint or a copy-on-write snapshot. Run forward migrations in the shadow. Do not mutate live state.
4. **Standby boot.** Start the candidate's sandboxed instances with the candidate's capability grants, but hold external effect calls behind the activation gate. Run pre-flip readiness checks: process boot, contract conformance, dependency service resolution, read/write probe on shadow state, and denied-capability probes.
5. **Drain old generation.** Stop routing new work to the old generation. Allow in-flight work a bounded drain deadline; cancel the remainder. Dispose old effects in dependency order only after dependents have unloaded.
6. **Pointer flip.** Atomically publish the new `ScopePointer`. The new generation becomes authoritative for new work; the old generation remains on disk for rollback.
7. **Post-flip health.** Run live health checks until a deadline: canonical append/read under the scope's grant, tool catalog resolution, view registration, dependency calls, memory/CPU ceilings, crash-loop check, and absence of orphaned processes. Health probes may append only activation-health records; they must not perform unrecorded external effects.
8. **Commit or roll back.** On success, append `ActivationReceipt` with the health report and mark the previous candidate `retired`. On failure, repoint the pointer to the previous candidate and state generation, dispose the new effects, mark the candidate `failed_health`, and append a failure receipt.
9. **Release locks.**

Health checks MUST cover:

- process and sandbox boot within the configured deadline;
- contract conformance: declared projections, tools, and views register and unregister cleanly;
- capability denial: a probe for an undeclared capability fails closed;
- canonical append/read: the scope can append an activation-health record and read its own coverage;
- scoped state: read/write/rollback on the shadow generation;
- dependency: every required coeffect resolves at the pinned contract version;
- resource: memory, CPU, process count, and host-call rate within ceilings;
- leak: no orphan timers, sockets, child processes, or temporary files after disposal.

A failed health check produces a structured `HealthFailure` naming the failing probe, observed value, expected bound, and whether the failure is retryable.

Crash recovery reads the intent record:

- Crash before pointer flip: the old pointer remains active; the staged candidate is orphaned and later reclaimed.
- Crash after pointer flip and before health: the activation supervisor re-runs health on boot. If health fails, it rolls back; if health cannot be determined, it quarantines the scope and leaves the pointer on the last known-good candidate.
- Crash during rollback: the supervisor retries rollback idempotently; if the state generation was migrated forward-only, it does not attempt a down migration and instead reports `rollback_blocked_forward_only`.

Rollback is resource rollback, not effect rollback. A broker call, model call, file write, or process effect that occurred under the failed candidate remains an external fact. Rollback appends effect-outcome records; where the kernel cannot determine whether an effect applied, it records `outcome_unknown` and requires a separate compensation decision.

#### 4.8.5 Live arbitrary plugins outside the frozen trust root

"Live" means a candidate can become active without rebuilding or restarting the kernel. It does not mean plugin code runs in the kernel process.

Plugin code is arbitrary and untrusted. It MUST load as a content-addressed artifact into a separate OS-sandboxed process or worker. The kernel communicates over a framed RPC protocol exposing only `PluginContext` handles. The OS sandbox is the outer boundary; the language runtime is an inner runtime. No in-process interpreter is the outer security boundary.

The sandbox supervisor:

- bind-mounts only selected project roots, scratch space, the agent-mutable Kanbei paths for that scope, and read-only runtime inputs;
- withholds the canonical database, trust-root directory, Nix store, broker configuration, and secret material;
- applies process, memory, CPU, file-descriptor, and wall-clock ceilings;
- kills the process group on cancellation or ceiling breach;
- records a sandbox violation as a canonical audit record and quarantines the candidate.

Build and activation are separated. Building a plugin MAY use a build capability, including network if human policy allows it, and produces immutable artifacts plus a build receipt. Activation uses the built artifact and never runs package install scripts. Dependencies are locked and content-addressed; a lockfile change is a dependency diff and follows candidate policy.

Live mutation is intentionally broad: arbitrary tools, plugins, declarative context projections, UI, keybinds, themes, settings, and tool wiring MAY be activated as candidates. This is an experiment to be tightened if practice demonstrates unacceptable risk. The trust root remains frozen. A candidate that references a trust-root path, attempts to write the activation pointer, or asks the append API for `human_authority` or `activation_receipt` MUST be rejected.

The accepted residual is explicit: because outbound network is unrestricted, a malicious plugin can exfiltrate any project data it can read. The broker protects enrolled credentials from disclosure to agent code, but it does not stop a plugin from reaching the real endpoint directly or from using an enrolled credential against any method, path, or payload the provider permits. Capability diffs and audit records provide attribution, not confidentiality.

#### 4.8.6 Context-projection mutation

Context projection is the agent-editable lens over canonical state. It is a declarative document or, when executable, a projection candidate.

```ts
interface ContextProjectionDoc {
  version: string;
  scope: IsolationScope;
  sources: ContextSource[];
  filters: FilterExpr[];
  ranking: RankingSpec;
  budgets: { tokens: number; items: number; bytes: number };
  summaries: SummaryRule[];
  disclosure: DisclosureRule[];
  fallback: "error" | "degrade" | "mark_stale";
}
```

Declarative documents cover filters, queries, budgets, summaries, and disclosure rules. They are versioned candidates and follow the staging, validation, and activation protocol, except activation is **next-turn**: the executing loop turn pins the projection version it started with; the new version applies at the next turn boundary. This preserves a coherent turn while allowing live perception changes.

Validation for declarative context projections MUST check:

- schema and budget bounds;
- source queries resolve only to authorized canonical or projection views;
- no filter references a component version the projection does not declare;
- no rule presents a `suspect` or retracted record as current;
- disclosure rules are ordered and total on the declared result set;
- fallback behavior is explicit.

Executable projection code is a plugin candidate implementing `Projection<S>`. It follows the full code-candidate path: static capability scan, sandboxed conformance, capability diff, shadow state, pointer flip, health, and rollback. It cannot write canonical authority records, cannot read the trust root, and cannot bypass the coverage watermark.

Every context projection output carries `Coverage`. If the projection is behind, the context builder MUST wait within the turn budget, degrade to a declared fallback, or mark the section stale and include the watermark. It MUST NOT silently present stale derived state as current. A retraction marks dependent outputs `suspect` immediately at read time and triggers an incremental rebuild.

Agents MAY edit the lens, including filters that hide evidence from their own context. That is permitted self-modification, but it does not affect the canonical ledger, the audit sink, or human decision views, which read outside the agent's context scope. Every context-projection activation appends a receipt and a diff identifying what became visible or hidden.

#### 4.8.7 Trusted Nix updates and migration compatibility

The kernel, sandbox supervisor, broker, append API, activation supervisor, receipt verifier, consent surface and gesture endpoint, and audit sink are Nix-built artifacts. They change only through a human-approved Nix generation. The runtime MUST NOT download or hot-patch trust-root code.

```ts
interface KernelManifest {
  kernelVersion: string;
  nixGeneration: string;
  contractVersions: Record<string, string>;
  canonicalFormat: number;
  pluginApi: string;
  stateSchemas: Record<string, { min: number; max: number }>;
}
```

At boot, the kernel MUST run a compatibility gate before activating any plugin scope:

1. **Canonical format.** If the ledger's format is newer than `canonicalFormat`, the kernel refuses to start. It does not run a down migration.
2. **Plugin state schemas.** For each active scope, if the store's `dataVersion` is newer than the kernel's `max`, the scope is quarantined read-only and surfaced in the decision queue. If it is older, the kernel runs the registered forward migration against a shadow generation, records a state-migration receipt, and only then flips the state generation.
3. **Contract versions.** Each plugin manifest's contract-version range must intersect the kernel's supported range. An incompatible plugin is quarantined, not automatically rewritten.
4. **Artifact availability.** Content-addressed plugin artifacts must exist and verify. Missing or corrupted artifacts quarantine the scope; the kernel does not fetch from the network at boot.

Migration rules:

- Migrations are forward-only by default. Each descriptor names `from`, `to`, `mode`, and digest.
- `compatible` means the previous kernel can still read the new state. Code rollback is allowed.
- `forward-only` means the previous code cannot read the new state. The activation is marked `roll_forward_only`; code rollback is forbidden.
- `requires-checkpoint` means a verified checkpoint must exist before migration; rollback uses the checkpoint as a new branch, not a down migration.
- No automatic destructive down migration is ever executed.
- Human-approved Nix rollback is allowed only when the data remains readable by the older kernel. Otherwise the supervisor refuses with `rollback_blocked_data_incompatible` and offers roll-forward or checkpoint-branch recovery.

A Nix switch restarts the kernel and re-runs the compatibility gate. Isolation-scope activation pointers and plugin-state generations live under `~/.local/state/kanbei/`, outside the Nix store, so they survive a kernel generation change. A Nix update that changes contract versions may quarantine old plugins; the human sees the compatibility report before or at switch time.

---

### 4.9 Tool surface, model routing, and runtime selection

#### 4.9.1 Agent-visible discovery and execution

The agent sees one execution surface, not hundreds of per-tool schemas in the default context. The stable meta-tools are `search`, `list`, `describe`, and `execute`. Direct per-tool schemas are fetched only through `describe`.

```ts
interface ToolDescriptor {
  name: string;
  version: string;                     // semver
  digest: string;                      // content digest of the exact descriptor
  kind: "host" | "plugin" | "model" | "search" | "fetch" | "projection" | "loop";
  summary: string;
  input: JsonSchema;
  output: JsonSchema;
  errors: ToolErrorCode[];
  effect: "none" | "read" | "write" | "external";
  idempotency: "required" | "supported" | "none";
  timeoutMs: number;
  streaming: boolean;
  capabilities: CapabilityRequest[];
  docs: ContentRef;
  examples: Array<{ input: unknown; output?: unknown; note?: string }>;
  deprecated?: { since: string; replacement?: string };
}

interface ToolSummary {
  name: string;
  version: string;
  digest: string;
  kind: ToolDescriptor["kind"];
  summary: string;
  tags: string[];
  deprecated?: boolean;
}

interface ToolSearchRequest {
  query: string;
  kind?: ToolDescriptor["kind"][];
  tags?: string[];
  limit?: number;                      // operator-configured default and maximum
  cursor?: string;
}

interface ToolListRequest {
  prefix?: string;
  kind?: ToolDescriptor["kind"][];
  limit?: number;
  cursor?: string;
}

interface ToolDescribeRequest {
  name: string;
  version?: string;                    // omitted means latest visible
}
```

Contracts:

- Discovery is scope-filtered. A caller sees only tools granted to its scope.
- `list` is deterministically ordered by `(name, version)`; `search` is deterministically ordered by the configured ranking, then `(name, version)`.
- Pagination cursors are opaque and stable for a catalog generation. A cursor from a newer catalog generation returns `version_conflict`, not a silent mixed page.
- `describe` returns the exact descriptor, input/output schemas, error codes, capability requirements, effect class, idempotency behavior, timeout, streaming support, examples, and documentation reference, plus `digest`.
- A tool name without a version resolves to the highest non-deprecated compatible version visible to the caller; the resolved version and digest are returned and recorded.
- `execute` resolves to an exact descriptor digest, and that digest is recorded.

```ts
type RuntimeId = "monty" | "node" | "deno" | "lua" | "bash" | "cpython";

type ExecuteRequest =
  | {
      mode: "call";
      name: string;
      version?: string;
      args: unknown;
      idempotencyKey?: string;
      timeoutMs?: number;
      stream?: boolean;
    }
  | {
      mode: "program";
      language: RuntimeId;
      source: string;
      entry?: string;
      input?: unknown;
      idempotencyKey?: string;
      timeoutMs?: number;
      stream?: boolean;
      budget?: { maxHostCalls?: number; maxOutputBytes?: number; maxWallMs?: number };
    };

interface HostCall {
  id: string;
  name: string;
  version: string;
  digest: string;
  args: unknown;
  timeoutMs: number;
  idempotencyKey: string;
}

type HostCallResult =
  | { ok: true; value: unknown; receipt?: RecordId }
  | { ok: false; error: ToolError; receipt?: RecordId };
```

In `call` mode, the kernel dispatches one versioned function with structured arguments. In `program` mode, code runs in the selected runtime inside the OS sandbox. Host functions are generated as typed stubs from the visible tool catalog; each stub call carries the exact name, version, and digest.

Host calls are effects. They are recorded, cancellable, rate-limited, and counted against `maxHostCalls`. Stub calls are the only *attributable* host path: the runtime can still open a raw socket under the accepted unrestricted-egress policy, but that path carries no host capability, no broker credential, and no audit attribution. This is attribution, not containment; Kanbei does not claim the runtime cannot bypass the stubs. The runtime is selected by the bake-off and remains replaceable behind `RuntimeId`.

#### 4.9.2 Results, structured errors, streaming, and cancellation

```ts
type ToolErrorCode =
  | "invalid_request"
  | "not_found"
  | "version_conflict"
  | "capability_denied"
  | "sandbox_violation"
  | "timeout"
  | "cancelled"
  | "resource_exhausted"
  | "runtime_error"
  | "tool_error"
  | "outcome_unknown"
  | "internal";

interface ToolError {
  code: ToolErrorCode;
  message: string;
  retryable: boolean;
  effect: "none" | "not_applied" | "applied" | "outcome_unknown";
  details?: Record<string, unknown>;
  correlationId: string;
  receipts?: RecordId[];
}

type ToolResult<T> =
  | {
      ok: true;
      value: T;
      receipts: RecordId[];
      meta: { tool: string; version: string; digest: string; durationMs: number };
    }
  | {
      ok: false;
      error: ToolError;
      receipts: RecordId[];
      meta: { tool?: string; version?: string; digest?: string; durationMs: number };
    };
```

Errors are structured values, not transport failures. `capability_denied` and `sandbox_violation` are security-audited. `effect` distinguishes a failure before any external attempt from one after an ambiguous attempt. Known credential values MUST be redacted from messages and `details`. Every error carries a correlation id linking the tool call, host calls, effect receipts, and audit records.

Streaming execution emits an ordered event stream:

```ts
type ExecutionEvent =
  | { type: "started"; executionId: string; runtime: RuntimeId }
  | { type: "stdout"; chunk: string }
  | { type: "stderr"; chunk: string }
  | { type: "progress"; message: string; fraction?: number }
  | { type: "host_call"; callId: string; name: string; version: string; argsDigest: string }
  | { type: "host_result"; callId: string; ok: boolean; resultDigest: string }
  | { type: "artifact"; ref: ContentRef; mediaType: string }
  | { type: "error"; error: ToolError }
  | { type: "done"; result: ToolResult<unknown> };

interface ExecutionHandle {
  id: string;
  events: AsyncIterable<ExecutionEvent>;
  cancel(reason: string): Promise<CancelReport>;
}
```

Stream events are ordered per execution, backpressured, and bounded by `maxOutputBytes`. `done` is emitted exactly once and is terminal. If the client disconnects or cancels:

1. No new host calls are admitted.
2. The cancellation token is delivered to the runtime and to in-flight host calls.
3. The runtime gets a bounded grace period to settle its owned work.
4. The sandbox supervisor kills the process group if the runtime does not reach quiescence.
5. In-flight effects append their results or `outcome_unknown`; the kernel never blindly retries an unknown non-idempotent effect.

Cancellation is idempotent and returns a report naming which host calls were cancelled, which completed, and which are unknown.

#### 4.9.3 Provider-neutral search and fetch

Search and fetch sit behind a provider-neutral contract. TinyFish is one adapter, not the contract.

```ts
interface SearchQuery {
  text: string;
  filters?: {
    domains?: string[];
    recency?: { after?: Iso; before?: Iso };
    language?: string;
    region?: string;
  };
  limit: number;
}

interface SearchHit {
  locator: string;             // URL
  title?: string;
  snippet?: string;
  providerRank?: number;
  providerScore?: number;      // opaque; never compared across providers
  sourceTime?: { value: Iso; precision: "instant" | "day" | "estimate" };
  native: unknown;
}

type SearchRequest = SearchQuery;

interface SearchResult {
  hits: SearchHit[];
  providerRequestId?: string;
  rawEnvelope: Digest;
  cost?: unknown;
}

interface FetchRequest {
  url: string;
  acceptMediaTypes: string[];
  conditional?: { etag?: string; lastModified?: string };
}

interface FetchResult {
  status: "ok" | "not-modified" | "not-found" | "forbidden" | "error";
  finalUrl: string;
  mediaType?: string;
  body?: Digest;
  rawEnvelope?: Digest;
  etag?: string;
  lastModified?: string;
  providerRequestId?: string;
  errors: ProviderError[];
}

interface ProviderError {
  code: "auth" | "quota" | "rate_limit" | "upstream_forbidden" | "transient"
      | "invalid_request" | "unsupported_media" | "not_found" | "gone" | "too_large";
  message: string;
  retryable: boolean;
  retryAfter?: Iso;
}

interface SearchProvider {
  id: string;
  capabilities: {
    maxResults: number;
    pagination: "none" | "page" | "cursor";
    async: boolean;
    streaming: boolean;
    contentCoReturned: boolean;
    scores: boolean;
    publishedTime: "exact" | "estimate" | "none";
    conditionalFetch: boolean;
    mediaTypes: string[];
    provenanceFields: string[];
    units: string;                      // e.g. "requests/min", "urls/min", "credits/10 results"
    limits: Record<string, number>;
  };
  search(req: SearchRequest, ctx: CapabilityContext): Promise<SearchResult>;
  fetch(req: FetchRequest, ctx: CapabilityContext): Promise<FetchResult>;
}
```

Rules:

1. **A search hit is a candidate locator, not a capture.** Only a fetch produces a capture observation. A snippet is a provider-generated derivative and MUST NOT be cited as source content.
2. **Preserve provider-native fields.** Normalization adds the core fields; `native` and the raw envelope remain evidence. Grouped result shapes are flattened without discarding grouping.
3. **Do not assume content co-location or scores.** Search may inline fetch content, may return no body, and may return ranks or scores with incompatible meanings. Provider scores MUST NOT be compared across providers.
4. **Use conditional fetch where supported.** A `304` advances validators and produces coverage, not a duplicate capture.
5. **Respect documented ceilings.** TinyFish Search is 30 requests/minute and 12,000/day, with page index at most 10. Backpressure is applied before the provider call; global ceilings remain hard.
6. **Typed failure mapping.** `402` is `quota`; `403` may be `upstream_forbidden`; `429` is `rate_limit`; `5xx` is `transient`; an unsupported image fetch is `unsupported_media`. No failure produces a content-bearing observation.
7. **Transport is part of provenance.** If an image or PDF is fetched by a different transport than search, `evidence.request.method` and the transport identity MUST record that choice. Transport selection for non-TinyFish media is `[SPIKE: NET-1]`.

Every call records the provider, exact parameters, request id, response time, and raw request/response in the CAS. Normalization produces a common result shape without claiming capability equivalence.

#### 4.9.4 Provider-neutral model routing and exact I/O

Model routing is policy-driven, not plugin-discovered. Human-owned task policies are canonical authority records; agents may propose policy changes but cannot grant their own routing authority.

```ts
type TaskClass = string;               // e.g. "plan", "code.edit", "memory.reconcile"

interface TaskPolicy {
  task: TaskClass;
  primary: ModelRef;
  fallback: ModelRef[];
  maxInputTokens: number;
  maxOutputTokens: number;
  maxCostUsd?: number;
  timeoutMs: number;
  retry: { maxAttempts: number; retryOn: string[] };
  allowProviderTools: boolean;
}

interface ModelRouter {
  route(task: TaskClass, constraints: RouteConstraints): Promise<RouteDecision>;
  invoke(
    decision: RouteDecision,
    request: ModelRequest,
    ctx: ModelCallContext
  ): Promise<ModelResponse>;
}

interface RouteDecision {
  task: TaskClass;
  provider: string;
  model: string;
  policyRecord: RecordId;
  fallback: ModelRef[];
  limits: { maxInputTokens: number; maxOutputTokens: number; timeoutMs: number };
}
```

Exact I/O capture is mandatory:

1. Before the call, append `ModelCallRequested` with the task class, policy record, route decision, provider, model, normalized request digest, post-redaction request digest, and idempotency key.
2. After the call, append `ModelCallCompleted` with the normalized response, output events, usage, finish reason, post-redaction response digest, latency, and provider request id.
3. On provider error or timeout, append `ModelCallFailed` with the structured error and whether an external effect is `not_applied`, `applied`, or `outcome_unknown`.
4. Store the exact post-redaction semantic payload and post-redaction wire bytes in the CAS and reference them from the records. The normalized form is an evidence convenience, not a replacement. Unredacted credential-bearing bytes are not persisted by default; the record carries redaction metadata and an optional original-bytes commitment (§4.7.6).
5. Record exposed reasoning artifacts only. Hidden provider chain-of-thought is not promised or reconstructed.

Provider-neutral normalization covers messages, tool calls, output events, usage, and finish reason. Provider-specific fields remain in `providerOptions` and the raw payload; the router MUST NOT assume two providers expose equivalent tools, reasoning, caching, or structured output. A route decision may fall back only within the human-owned policy chain. Global spend, token, rate, and concurrency ceilings are hard; per-loop budgets are soft and require a recorded rationale for override.

#### 4.9.5 Agent-facing execution-language bake-off

The agent-facing execution language and Node-versus-Deno are deferred spikes. This section defines a fair comparison; it does not choose a winner.

Before any trial:

- freeze runtime versions, image digests, compiler/interpreter versions, model IDs, prompts, tool catalog, tool descriptions, seeds, OS-sandbox profile, hardware, and timeouts;
- state hypotheses and non-inferiority margins;
- separate the runtime factor from the interaction pattern, so a code-mode effect is not attributed to the language;
- publish raw traces, prompts, tool schemas, and failure taxonomy.

| Candidate | Role | Required controls |
|---|---|---|
| Monty | Python-subset language-level sandbox | Same OS sandbox; type stubs/docs; host-call facade; explicit subset traps |
| TypeScript on Node | Full TS/JS runtime | Same OS sandbox; same lockfile policy; Node permissions not treated as outer boundary |
| TypeScript on Deno | Full TS/JS runtime with permission flags | Same OS sandbox; Deno permissions as inner boundary only; no Deno Sandbox dependency |
| Lua / Luau | Small embeddable runtime | Same host-injected globals; no `io`/`os.execute`/`package` unless granted; same sandbox |
| Bash | Shell plus OS sandbox | Same filesystem/process policy; structured result wrapper; quoting/injection probes |
| Full CPython | Full language and package ecosystem | Same OS sandbox; pinned wheels; package provenance recorded |

Node versus Deno is a separate sub-factor using the same TypeScript workload and the same OS sandbox. Deno Sandbox is excluded from the candidate set because it is closed to new customers as of the current evidence; `deno run` permissions are measured as an inner policy layer, not as the outer boundary.

Each candidate runs inside the same OS-sandbox profile: same filesystem bindings, process limits, egress policy, and cgroup/rlimit/wall-clock ceilings. If a candidate cannot run under that profile, that is a recorded capability failure, not a relaxed control.

The workload suite MUST include at least ten equivalent tasks, each with a gold output and a structured rubric:

1. Fan-out read: 50 independent lookups, filter/aggregate, return top-k.
2. Conditional multi-step: branch on intermediate results; early exit.
3. Large-output reduction: fetch 10 MB, return a 2 KB summary.
4. File transformation: parse CSV/JSON, write a derived file.
5. Error recovery: one transient tool failure with backoff; one permanent failure.
6. Approval gate: pause before a side-effecting call; resume after approval.
7. Fork/branch: duplicate state and explore two paths.
8. Streaming progress: emit progress during a long loop.
9. Dependency-heavy task: pandas/requests or npm equivalents where the runtime supports them.
10. Adversarial task: prompt-injected tool output attempts to exfiltrate a secret.

Run each task at host-call latencies of 10 ms, 100 ms, and 1 s. Record success, token use, latency, host-call behavior, snapshot availability and trust, and operability inside the OS sandbox. The bake-off MUST also record capability failures rather than relaxing the sandbox. `[SPIKE: LANG-1]` selects the agent-facing language; `[SPIKE: NODE-DENO-1]` selects the TypeScript runtime. TypeScript remains the main control-plane language regardless, and the OS sandbox remains mandatory unless the threat model changes.

---

### 4.10 Security, broker, and deployment

#### 4.10.1 Security invariants and adversary

| ID | Invariant |
|---|---|
| S-1 | The host kernel, the Nix-built frozen trust root, the human, and the audit sink are trusted. All agent, plugin, tool, and ingested-content-derived code is untrusted. |
| S-2 | Mutable code never executes inside a process that holds canonical-ledger write authority, broker secret material, or frozen-root control. |
| S-3 | Credential bytes never enter an agent address space. The broker injects them after authenticating the caller and selecting an active enrollment. |
| S-4 | Credential injection is not action authorization. The broker does not decide whether a method, path, or payload is appropriate. |
| S-5 | Agent-visible data access is limited to human-selected project roots, per-sandbox scratch, and agent-mutable Kanbei paths. Toolchain paths are read-only and not project data. |
| S-6 | All canonical writes pass through the frozen append API. No agent or mutable plugin opens the canonical SQLite file. |
| S-7 | Outbound network destination policy is unrestricted. Kanbei does not protect project-data confidentiality from malicious readable code. |
| S-8 | Global spend, token, rate, concurrency, writable-root, secret-access, broker-enrollment, canonical-append, audit, and activation boundaries are hard human-owned limits. Approvals never override them. |
| S-9 | Every mediated external effect produces an attempt and a receipt through the frozen append path. Replay never repeats an external effect. |
| S-10 | The broker fails closed on authenticated, audited, credentialed requests. There is no credential fallback, environment-variable fallback, or direct-connection fallback. |
| S-11 | The frozen trust root changes only by a human-approved Nix generation. Agents may propose but cannot live-edit it. |
| S-12 | A sandbox starts only after the supervisor verifies the declared enforcement class. A weaker-than-declared posture is a start failure, not a silent downgrade. |
| S-13 | Inter-agent isolation is fault and resource containment only. It is not a security boundary for credentials, canonical state, or approvals. |
| S-14 | The UI binds loopback only and is exposed through the existing Cloudflare Tunnel and Access conventions. |
| S-15 | All mutable Kanbei state lives under `~/.local/state/kanbei/`; trusted Nix configuration and secret material remain outside it and are covered by backup integration separately. |

The adversary is the agent process itself: prompt-injected or fully compromised, with arbitrary code execution inside its sandbox. It controls argv, environment, written files, DNS queries, and every byte sent to a socket. It can read any file its OS identity can read, signal or trace same-identity processes unless deployment blocks that, and attempt kernel or supervisor exploits. Upstream APIs and the network are hostile or semi-trusted; they are reached with real credentials. The host and human remain trusted.

#### 4.10.2 Process identities and broker caller authentication

```ts
type ComponentKind =
  | "core" | "sequencer" | "broker" | "sandbox_supervisor"
  | "agent" | "plugin_host" | "ui" | "watchdog" | "backup";

type ProcessIdentity = {
  component: ComponentKind;
  instanceId: string;            // unique per process start
  osUid: number;
  generation: NixGenerationId;
  cgroupPath: string;
  sandboxId?: SandboxId;         // agent and plugin hosts only
  agentId?: AgentId;
  loopId?: LoopId;
  scopeId?: IsolationScope;
  startToken: Sha256;            // supervisor-issued, bound to sandboxId + cgroup
};
```

The broker accepts a caller only after all of the following:

1. Unix-socket peer credentials identify the expected OS user.
2. The presented `startToken` matches a live sandbox issued by the supervisor.
3. The peer's cgroup path matches the sandbox record.
4. The sandbox generation matches the running generation.

Under a pure same-UID user-space deployment, steps 1–3 are attribution, not a boundary: a same-UID agent can read, trace, or signal a sibling unless the host security spike proves otherwise. S-13 therefore applies to broker caller identity as well.

| Component | Trust | Loads mutable code | Holds secrets | Writable state |
|---|---|---|---|---|
| `kanbei-core` | trusted | no | no | canonical ledger and scoped-state metadata through frozen APIs |
| `kanbei-sequencer` | trusted | no | no | ledger write-ahead state |
| `kanbei-broker` | trusted | no | yes, in memory only | redacted audit spool, enrollment metadata |
| `kanbei-sandbox-supervisor` | trusted | no | no | sandbox records, cgroup/process control |
| `kanbei-agent-<sandbox>` | untrusted | yes | no | selected project roots, scratch, own agent state |
| `kanbei-plugin-<scope>` | untrusted | yes | no | own scoped store and candidate directory |
| `kanbei-ui` | mutable presentation | yes | no | none canonical; read-only API client |
| `kanbei-consent` | trusted, frozen | no | no | none; origin-isolated renderer that mints the single-use consent token and `consentDigest` |
| `kanbei-watchdog` | trusted, external | no | alert credential only through broker or dedicated alert unit | none agent-writable |
| `kanbei-backup` | trusted, external | no | no | backup media |

`kanbei-core` is trusted but does not hold provider credentials. Model calls and search calls are brokered like any other credentialed external effect. `kanbei-ui` is allowed to be live-mutable under self-modification rules, so it MUST NOT be treated as a trusted enforcement component. `kanbei-consent` is a frozen component in the trust root: it serves from a separate host and origin behind the same verified human identity, holds a host-only `SameSite=Strict` session with no shared DOM or storage with `kanbei-ui`, alone renders ruling evidence and mints the single-use consent token and `consentDigest`, and accepts no content, session, or token from `kanbei-ui`.

#### 4.10.3 Sandbox, writable roots, and fault isolation

```ts
type AbsolutePath = string;
type EnforcementClass = "kernel_isolated" | "same_uid_dac" | "none";

type SandboxMount = {
  path: AbsolutePath;
  access: "ro" | "rw";
  class: "toolchain" | "project" | "scratch" | "agent_state" | "mutable_code" | "runtime";
};

type SandboxSpec = {
  sandboxId: SandboxId;
  agentId: AgentId;
  loopId: LoopId;
  generation: NixGenerationId;
  enforcementRequired: EnforcementClass;
  mounts: SandboxMount[];
  writableRoots: AbsolutePath[];
  network: {
    egressDestinations: "any";     // no allowlist, no destination policy
    inboundListeners: "none";      // may be best-effort under same-UID DAC
    rawSockets: "permitted";
  };
  limits: { wallMs: number; memoryBytes: number; pidsMax: number; cpuQuota: number; openFiles: number };
  capabilitySockets: { broker: AbsolutePath; supervisor: AbsolutePath };
  credentialAccess: "broker_only";
};
```

| Mount class | Default access | Contents | Backup |
|---|---|---|---|
| `toolchain` | `ro` | Nix store, system libraries, runtimes, read-only binaries | no |
| `project` | `rw` | Human-selected project roots, recorded as project-root authority records | via the project's own repository backup |
| `scratch` | `rw` | `~/.local/state/kanbei/scratch/<sandboxId>/`, unique per sandbox | no; deleted on termination |
| `agent_state` | `rw` | `~/.local/state/kanbei/agents/<agentId>/` | yes |
| `mutable_code` | `ro` except own candidate | `~/.local/state/kanbei/mutable/`; agents write candidates, never the active pointer | yes |
| `runtime` | `ro` socket access | supervisor socket and broker socket only | no |

Project roots MUST be added by a human authority record; agents may propose but cannot activate a new root. The canonical ledger, secrets, frozen-root control files, other agents' state, and the audit sink MUST NOT be mounted into the sandbox. The broker socket is mounted only as a socket path; its credential material is not.

The sandbox supervisor records the actual `EnforcementClass` and compares it with `enforcementRequired`. If the actual class is weaker, the sandbox MUST NOT start; the refusal is an effect receipt with `sandbox_posture_failed`. Verification after start, before handing control to agent code, MUST confirm:

- the mount namespace exists and contains exactly the admitted mounts;
- the cgroup exists with the requested memory, pids, and CPU limits;
- the broker and supervisor sockets are reachable and no other capability socket is;
- the process cannot see forbidden paths by DAC or Landlock;
- the network namespace or posture matches the declared network mode;
- the sandbox's `startToken` is registered with the supervisor.

If any check fails, kill the cgroup and emit `sandbox_posture_failed`. Do not run with a claimed stronger posture.

Known limits carry into the contract:

- Landlock network rules are port-only, not address or hostname.
- Landlock does not revoke file descriptors opened before enforcement and cannot express a hostname-and-port destination policy.
- `ProtectHome=true` is incompatible with a service that needs writable `$HOME` state; only `read-only` plus explicit writable paths is viable.
- Namespacing options in a user unit imply `PrivateUsers`; if user namespaces are unavailable, the unit fails to start rather than degrading.

One sandbox instance exists per loop execution attempt, not one per long-lived agent concept. The supervisor owns the instance cgroup, process group, and termination. An agent crash, hang, OOM, or fork bomb MUST NOT stop core, broker, supervisor, or UI; each sandbox has separate memory, pids, CPU, scratch, and wall-clock limits; termination is `SIGTERM`, grace period, then cgroup `SIGKILL`; no scratch directory is shared between sandboxes; supervisor restart reaps all surviving sandboxes and re-launches only from ledger checkpoints; a hung sandbox is killed by the supervisor's deadline, not by a cooperative agent callback.

Mechanism is deferred: `[SPIKE: ISO-1]` lightweight process, WASM instance, or microVM. A container per agent is excluded by requirement. A mechanism SHOULD be selected only when operationally cheap. The selected mechanism may improve fault isolation but MUST NOT be described as a security boundary for credentials or approvals.

#### 4.10.4 Network posture and the exfiltration residual

The sandbox network capability permits arbitrary outbound destinations, including raw sockets. There is no destination allowlist, no egress-proxy requirement, no DNS policy, and no attempt to prevent project-data exfiltration. The broker is a credential-injection path, not an egress choke point.

> **Accepted residual:** malicious readable code can exfiltrate anything it can read. Kanbei accepts this residual.

The readable set includes selected project roots, per-sandbox scratch, the agent's own state, mutable code it can read, ingested observations, model context, and any other file its OS identity can read. Secret material is outside the policy-granted set, but:

- the broker can be induced to use an enrolled credential against its enrolled host;
- a same-UID agent may read broker memory or another process's memory if deployment does not block ptrace/process inspection;
- a provider may reflect a credential in a response, and response redaction is best-effort;
- an agent can exfiltrate non-secret project data directly.

Kanbei does not claim host-integrity or confidentiality protection against arbitrary code. The host and human remain trusted; the sandbox's job is resource and fault containment, not secrecy of project data.

Effect audit covers mediated effects: broker requests, model calls, capability-mediated filesystem and process operations, activation, enrollment, revocation, and ceiling events. Direct raw network traffic and direct syscalls inside the sandbox are not fully attributable. This narrows the universal effect-audit claim and is recorded as residual R-1 in §6.

#### 4.10.5 Hard ceilings, reservations, and approval separation

| Boundary | Owner | Enforcement point | Agent override | Approval override |
|---|---|---|---|---|
| Writable roots | human | supervisor / mount policy | no | no |
| Secret access | human | broker | no | no |
| Global spend, token, rate, concurrency | human | frozen ceiling governor | no | no |
| Broker enrollment and mapping | human | broker | no | no |
| Canonical append | frozen kernel | append API | no | no |
| Audit sink | frozen kernel | append/audit path | no | no |
| Activation and rollback | frozen kernel | activation supervisor | no | no |
| Ingress | human | tunnel/Access and loopback bind | no | no |
| Autonomy tier | human | policy / decision queue | no self-grant | can tighten, not widen hard limits |
| Per-loop budget | soft, agent-owned | loop scheduler | yes, with rationale | n/a |
| Tool choice, plan interpretation | agent | loop | yes | n/a |

```ts
type Reservation = {
  reservationId: ReservationId;
  loopId: LoopId;
  effectClass: "model" | "broker" | "sandbox";
  amount: number;              // max expected units; provider-specific
  state: "held" | "committed" | "released" | "expired";
  createdAt: UtcUs;
  expiresAt: UtcUs;
};
```

`HardCeilings` is defined once in §4.7.3 and is referenced here; this section MUST NOT restate a divergent shape. `amount` and reservation accounting use the ceiling's own units from that definition.

Invariants:

- `sum(held + committed)` MUST NEVER exceed the configured ceiling.
- A model request reserves the maximum expected cost before the call; the response commits actual usage and releases the remainder.
- An interrupted effect with unknown usage leaves the reservation `held` until the audit path reconciles it or `expiresAt` passes.
- A broker request reserves its request count before dialing.
- A ceiling exhaustion produces `ceiling_blocked` and suspends the loop; it does not queue unbounded work.
- A concurrency ceiling queues by loop priority and deadline; no busy waiting.
- Human changes only by a new Nix generation or an approved operator configuration artifact. Agents cannot read or modify the ceiling object.
- Approvals do not override hard limits. If an approved action would exceed a hard ceiling, violate a writable root, use a non-enrolled logical host, or lack audit, it fails. The agent may request a human configuration change; it MUST NOT reinterpret the ceiling as a soft budget.
- Approval constraints can be overridden by the agent with a recorded rationale because they express delegated intent. Hard limits never use that override path.

Failure behavior:

| Condition | Behavior |
|---|---|
| Ceiling governor unavailable | Fail closed for new model/broker/sandbox effects; record `ceiling_unavailable` |
| Ceiling exhausted | `ceiling_blocked`; suspend loop; alert; no new calls |
| Reservation stuck | Expiry releases; audit reconciler resolves actual usage |
| Soft budget exhausted | `soft_budget_exceeded`; agent may append an override with rationale; hard ceiling still checked |
| Approval expired | New approval required for the bound plan hash |

Concrete values are mandatory operator configuration but remain deferred to `[SPIKE: LIM-1]`.

#### 4.10.6 Credential broker

The broker maps an enrolled logical hostname to a real endpoint, injects an enrolled credential, removes agent-supplied credential headers, and records a receipt. It is not an egress proxy, not a destination allowlist for general traffic, not an action-authorization layer, and not a provider-scope enforcer.

```ts
type SecureEndpoint = {
  scheme: "https";
  host: string;
  port: number;
  addressPolicy:
    | { mode: "resolve_once" }
    | { mode: "pinned"; addresses: IpAddress[] };
  allowPrivateAddresses: boolean;   // human enrollment decision
  tls: { verify: true; serverName: string; minVersion: "1.2" };
};

type CredentialInjection =
  | { kind: "header"; name: string; valueTemplate: string }   // e.g. "Bearer ${secret}"
  | { kind: "query"; name: string; valueTemplate: string }    // redacted in logs
  | { kind: "signer"; signerId: string };                     // frozen broker code

type CredentialEnrollment = {
  enrollmentId: EnrollmentId;
  logicalHost: LogicalHost;
  endpoint: SecureEndpoint;
  secretRef: SecretRef;             // host secret path or system credential name, outside Kanbei state
  injection: CredentialInjection;
  providerScopes?: string[];        // human attestation / guidance, not enforced by broker
  enrolledBy: HumanId;
  authorityRecordId: RecordId;
  enrolledAt: UtcUs;
  notes?: string;                   // least-privilege guidance shown at enrollment
};
```

`CredentialEnrollment` is immutable and carries no `status`. `pending/active/revoked` is `lifecycle_projection(family='enrollment', subject_id=enrollmentId)`. The broker looks up the projected state at its request basis; it never reads a stored status. A `Revocation` with `subject_kind="enrollment"` appends the `revoked` transition and takes effect for the next request in the same transaction.

| From | Event | Actor | Guard | To |
|---|---|---|---|---|
| (none) | `enroll` | Human | HTTPS endpoint; secret resolvable; injection supported | `pending` or `active` |
| `pending` | `activate` | Frozen append path | A supported injector now exists | `active` |
| `pending` / `active` | `revoke` | Human or frozen security component | `Revocation` record; no in-flight rollback | `revoked` |

Rules:

- Any credential MAY be enrolled; there is no provider allowlist.
- Enrollment is a one-time human action and a canonical authority record. Agents MUST NOT create, modify, or revoke enrollments.
- `secretRef` resolves outside `~/.local/state/kanbei/`; the broker reads it through the host secret mechanism. Secrets MUST NOT be placed in process environment or the Nix store.
- HTTPS is required for credential injection. Credentialed plaintext HTTP is rejected at activation.
- If no supported injector exists for the credential, enrollment remains `pending` with `unsupported_injection`; the broker MUST NOT silently pass the request without a credential.
- `providerScopes` is guidance only. The broker cannot verify provider-side scope, and the enrollment UI MUST state that the agent can exercise the credential's full provider authority.
- Least-privilege guidance is normative documentation: prefer dedicated non-admin provider credentials, minimal scopes, separate credentials per logical service, and a documented revocation path. It is not enforced.

```ts
type BrokerRequest = {
  logicalHost: LogicalHost;
  method: string;
  path: string;                     // relative path only
  headers: Array<[string, string]>; // agent-supplied; credential headers stripped
  body?: Uint8Array;
  idempotencyKey?: string;
};

type BrokerResponse = {
  receiptId: ReceiptId;
  status: number;
  headers: Array<[string, string]>;
  body: Uint8Array;
  redaction: "full" | "partial" | "none";
  truncated: boolean;
};

type BrokerOutcome =
  | { kind: "response"; response: BrokerResponse }
  | { kind: "denied"; reason: BrokerDenyReason }
  | { kind: "outcome_unknown"; reason: BrokerDenyReason; receiptId: ReceiptId };
```

Processing order is part of the contract:

1. Authenticate caller and generation.
2. Reject absolute-form targets, mismatched `Host`, or ambiguous paths.
3. Look up the `CredentialEnrollment` for `logicalHost` and require its lifecycle projection to be `active` at the current basis; a `pending` or `revoked` projection denies.
4. Reserve broker rate/ceiling units.
5. Ensure the audit sink is writable; append the attempt intent.
6. Resolve the endpoint once, apply the pinned-address policy, reject disallowed private space unless explicitly enrolled, and connect to the checked address.
7. Verify TLS with the enrolled `serverName`.
8. Remove every client-supplied credential header (`authorization`, `proxy-authorization`, `cookie`, `x-api-key`, and spelling variants).
9. Inject the enrolled credential exactly once.
10. Send the request with redirects disabled.
11. Buffer or stream the response through redaction.
12. Append the receipt with observed status, byte counts, redaction coverage, and outcome.

Steps 5 and 6 are fail-closed ordering: no secret is used before the audit intent exists, and no credential is sent before target verification.

The agent never supplies a real hostname, IP address, SNI, or `Host` header. The broker owns all three. Name-to-address policy is `resolve_once` with the checked address dialed, or a human-pinned address set; this is the only verified rebinding pattern. DNS failure, a filtered address, a private address without `allowPrivateAddresses`, or a TLS verification failure is a denial, not a fallback to another address.

TLS termination is the broker's price for header injection and response redaction. Upstreams requiring mTLS or certificate pinning cannot be mediated without forfeiting inspection; enrollment MUST mark them unsupported rather than silently bypassing injection.

The broker does not inspect method, path, or payload for allow/deny. It records them for audit. A request to an enrolled host with a destructive provider method is injected if it is otherwise a valid HTTP request. The enrolled credential is effectively a credential oracle for that host, limited only by provider-side authorization. The broker still enforces target integrity: `Host`, SNI, and the enrolled endpoint must agree; agent-supplied `Host` is rejected or replaced; absolute URLs and ambiguous targets are refused. Those are not action policies.

Redirect rules:

- The broker never follows redirects. It returns the 3xx response to the caller.
- A redirect to another host never receives the credential. The agent must issue a new broker request naming a logical host; that request is a fresh enrollment lookup and a fresh injection.
- If the agent follows the redirect itself with a direct connection, that connection carries no broker credential and is not broker-audited.
- A source configured with `capture.followRedirects=true` follows redirects only on the direct, uncredentialed transport path; it MUST NOT ask the broker to follow. The transport records the full chain and final locator in `CaptureEvidence.response.redirects`/`finalLocator`. A credential never travels on a redirect hop.
- `Location` is redacted like any other response field; a `Location` containing an enrolled credential value never reaches the agent.

Redaction applies to response headers and body; `Location` and other URL-bearing headers; error strings, stack traces, and denial reasons; broker logs and audit excerpts. The matcher replaces exact known credential values and common encodings (JSON-escaped, URL-encoded, percent-encoded, base64 where identifiable, HTML-escaped) with `[REDACTED:<enrollmentId>]`. Responses are buffered up to a configured byte cap. Beyond the cap, `redaction` is `partial` and `truncated: true`. If the redaction engine is unavailable, the broker returns status and headers only and records `redaction_unavailable`; it MUST NOT return an unredacted body.

Redaction precedes canonical capture. The broker writes the post-redaction body and envelope to the CAS, records `redaction` and the optional pre-redaction commitment on the receipt, and only then returns redacted bytes to the caller. A `partial` redaction is not exact evidence and is marked as such.

Logs never contain `authorization`, `proxy-authorization`, `cookie`, or `x-api-key` values. Request and response bodies are not logged by default; the audit record stores metadata and SHA-256 digests. Debug body logging, if enabled by a human, runs after redaction and still carries the partial-coverage residual. Response redaction cannot recover transformed, encrypted, compressed, or provider-echoed-in-a-new-encoding secrets. This is an explicit residual, not a guarantee.

Broker failure behavior:

| Condition | Behavior | Receipt |
|---|---|---|
| Caller unauthenticated / stale `startToken` | Deny before lookup | `denied: caller_unauthenticated` |
| Unknown or inactive logical host | Deny; no DNS, no secret read | `denied: logical_host_unknown` / `enrollment_inactive` |
| Secret unavailable | Deny; never fall back to env/file/passthrough | `denied: secret_unavailable` |
| Audit sink unavailable | Deny before dialing | `denied: audit_unavailable` |
| Ceiling/rate exhausted | Deny or queue per ceiling contract | `denied: ceiling_blocked` |
| Endpoint resolution fails | Deny; no alternate resolver | `denied: endpoint_resolution_failed` |
| Private address not enrolled | Deny | `denied: address_not_enrolled` |
| TLS verification fails | Deny; do not downgrade | `denied: tls_verification_failed` |
| Absolute-form or Host/SNI mismatch | Deny | `denied: ambiguous_request_target` |
| Upstream returns 3xx | Return response; never follow | `response` with redacted body |
| Broker crashes mid-dial | In-flight outcome unknown | `outcome_unknown` after restart |
| Redaction engine unavailable | Return metadata only | `denied: redaction_unavailable` |
| Broker process unavailable | Credentialed path fails; direct uncredentialed network unaffected | `denied: broker_unavailable` |

No retry of an unknown non-idempotent effect. A retry is allowed only with the original idempotency key and a fresh reservation.

Revocation and rotation:

- Human revocation appends a `Revocation` and the enrollment's lifecycle projection becomes `revoked`; new requests are denied immediately. In-flight TCP may complete.
- Secret rotation updates the secret source and reloads the broker; in-flight requests use the old value until drained.
- Per-request revocation does not exist for static provider credentials. Revocation granularity is the enrollment or the provider token TTL. If a provider supports short-lived tokens, the human may enroll a short-TTL credential; the broker does not mint one.
- The broker no longer holds a credential after revocation reload; memory zeroization is best-effort and not claimed against same-UID inspection.

#### 4.10.7 Effect audit

```ts
type EffectAttempt = {
  effectId: EffectId;
  requestId: RequestId;          // minted by the broker/supervisor, not the agent
  kind: "broker_request" | "model_call" | "fs_write" | "process_spawn"
      | "activation" | "enrollment" | "revocation" | "ceiling";
  actor: ProcessIdentity;
  loopId?: LoopId;
  subject: string;               // logical host, path, unit, enrollment
  startedAt: UtcUs;
  idempotencyKey?: string;
  requestDigest: Sha256;
  redactionIntent: "full" | "partial" | "none";
};

type EffectReceipt = EffectReceiptRecord;  // one canonical envelope (§4.7.5)

interface EffectReceiptMetadata {
  bytesIn?: number;
  bytesOut?: number;
  originalCommitment?: Digest;          // pre-redaction commitment; no secret retained by default
  redactionClasses?: string[];
}
```

`requestId` originates at authenticated ingress; client-supplied ids are ignored. `EffectReceipt` is the canonical `EffectReceiptRecord` from §4.7.5; it is appended through the frozen append API and participates in the ledger hash chain. It carries no separate `auditSeq` or `predecessorHash`; `seq` and the chain supply both. `observedAt` is the canonical `recorded_at_us`. Byte/redaction metadata is carried in the record's typed component.

| Effect | Audited | Notes |
|---|---|---|
| Broker request/response | yes | attempt before dial; receipt after |
| Model request/response | yes | exact available I/O, usage, cost |
| Capability-mediated filesystem write | yes | path, digest, bytes |
| Capability-mediated process spawn | yes | argv class, exit, duration |
| Activation/rollback | yes | candidate, capability diff, pointer flip |
| Enrollment/revocation | yes | authority record link |
| Ceiling block/reset | yes | ceiling generation and reason |
| Direct raw socket / direct syscall | no | residual R-1 |
| Hidden provider chain-of-thought | no | not promised |

The audit sink is outside agent-writable roots and is append-only from the frozen process's perspective. If the sink cannot record an audited effect, the effect MUST be denied. This follows the blocking-audit pattern and the zero-tolerance gate for missing required provenance.

Verification points:

- An agent cannot mint or alter a `requestId` that the broker accepts.
- A denied request produces an attempt and a denial receipt, not only a log line.
- A broker crash leaves `outcome_unknown`, not a fabricated success.
- Replaying a recorded effect never opens a new socket.
- Redaction coverage is recorded per receipt; unredacted bodies are never returned when the redaction engine is down.

#### 4.10.8 Deployment, ingress, watchdog, and backup

| ID | Deployment invariant |
|---|---|
| D-1 | Trusted code and the frozen trust root are Nix-built and immutable at runtime. |
| D-2 | Mutable code lives under `~/.local/state/kanbei/` and is activated by pointer flip, never by editing the Nix store. |
| D-3 | Secret material lives outside `~/.local/state/kanbei/` and is readable only by the broker identity the deployment actually provides. |
| D-4 | The audit sink and canonical ledger are not agent-writable. |
| D-5 | The watchdog is outside the agent process tree and cannot be disabled by mutable code. |
| D-6 | The UI binds `127.0.0.1` only and is exposed through the existing tunnel/Access conventions. |
| D-7 | Backup covers `~/.local/state/kanbei/` and verifies the ledger hash checkpoint and CAS references on restore. |
| D-8 | A Nix generation is recorded as a `kernel_generation` receipt appended by the frozen startup verifier; version drift is alerted. |

The deployment plane is deferred to `[SPIKE: DEP-1]`. The spike must decide between pure user-space and a dedicated-UID broker/sandbox helper; this specification does not. A pure user-space model is desirable but cannot be assumed to protect broker secrets from same-UID malicious code.

| Alternative | Process identities | Update / restart | Alerting | Secret protection | Status |
|---|---|---|---|---|---|
| A. Home-manager user unit, pure user-space | All components under the deployment user in a lingering user manager | Only home-manager switch updates/restarts the unit; a system rebuild alone does not | Journal-only unless a user-side notifier is built | Same-UID agents may inspect broker memory | Candidate |
| B. NixOS-plane user unit | Units in the system user manager, still one UID | System rebuild can restart user units | Still user-manager alerting limits | Same one-UID exposure | Candidate |
| C. Dedicated system unit(s) | Trusted components as dedicated system users | Manual switch; no automatic upgrade | System failure alerting and documented start-limit discipline | Broker UID distinct from agent UID; kernel-enforced peer identity possible | Strongest identity separation; not user-controlled |
| D. Hybrid user-space core plus dedicated-UID broker/sandbox helper | Core and agents under the user; broker and/or supervisor as a dedicated system user | Mixed user and system update paths | Helper can use system alerting; core remains journal-only unless a notifier is built | Broker secrets protected from user agents if helper UID and secret path are correct | Candidate |

The spike must also test whether the sandbox runs as the dedicated helper's UID, a separate sandbox UID, or the user with only the broker separated. Those have different file-ownership consequences for project roots and `~/.local/state/kanbei/`.

Nix-owned trusted updates:

- Trusted code is delivered by the Nix generation. No automatic system upgrade is assumed; deployment is manual.
- A home-manager-managed user unit is updated or restarted only by a home-manager switch; a system rebuild performs only a daemon-reload for running user services.
- Rollback is a home-manager or system switch rollback; the rollback horizon is bounded by the Nix garbage-collection window.
- The frozen trust root changes only by a human-approved Nix generation. Agents may propose a candidate; the activation supervisor cannot activate a frozen-root change from mutable state.
- Roll back code only when existing data remains readable. Otherwise roll forward or branch from a verified checkpoint. No destructive automatic down migration.
- Startup records the Nix generation as a `kernel_generation` authority record appended by the frozen startup verifier (not the activation supervisor, which owns `activation_receipt`), and verifies that core, broker, and supervisor are running from expected store paths. Mutable-code activation records a separate `activation_receipt`.
- The watchdog alerts on generation drift between the unit definition, the running core, and the recorded generation.

Ingress:

- The UI binds `127.0.0.1:<port>`. A shared port needs an explicit port allocation.
- Public ingress is the existing Cloudflare Tunnel convention; there is no additional nginx/caddy/traefik system service.
- TLS terminates at the Cloudflare edge; DNS and Access policies are OpenTofu-owned.
- Adding Kanbei to Access requires the subdomain in the protected list, a matching application, and a manual-step anchor. Evaluation fails otherwise; this is enforced, not advisory.
- Access is a human identity gate, not agent authentication. Unattended clients needing service-auth tokens are unresolved: `[SPIKE: ING-1]`.
- Authority endpoints MUST require a verified Cloudflare Access identity or an explicitly spiked local equivalent, even on loopback. A request that appears on loopback is not human consent.
- Approval evidence and rulings are rendered and submitted only by the frozen consent surface and its gesture endpoint (§4.1.2); the mutable world UI may deep-link to it and MUST NOT supply evidence, scope text, or `consentDigest`.
- The UI remains a mutable presentation component behind Access; it does not gain broker secrets or ledger write authority by being exposed.

The watchdog is a separate process or unit outside the agent process tree. It probes core append/sequencer liveness; broker availability and audit-sink writability; sandbox supervisor liveness and active-sandbox count; UI loopback response; disk free space and inode headroom for ledger/CAS; age of the last successful backup; and generation drift. It restarts failed components with bounded start limits, kills orphaned sandboxes and records receipts, suppresses duplicate alerts, and never includes credential values, request bodies, or unredacted response data in alerts. A user unit is journal-only unless a user-side notifier is built; only a system unit can use the existing system alerting path. That is part of `[SPIKE: DEP-1]`, not silently assumed.

Backup integration:

- All mutable Kanbei state is under `~/.local/state/kanbei/`; trusted Nix configuration and secret material remain outside it.
- The existing backup configuration MUST add the Kanbei path when implementation lands.
- The backup process may read the user-owned state tree without moving it to a system data directory.
- The backup protocol in §4.3.4 is normative.
- Exclude scratch, sockets, live WAL, broker memory, and all secret material.
- The exact backup hook wiring is `[SPIKE: BAK-1]`; the snapshot/CAS/GC ordering is not deferred.
- The watchdog alerts when the last successful backup exceeds its threshold.

Host security spike `[SPIKE: DEP-1]` MUST run on the target host, read-only where possible, and produce an enforcement matrix, not a preference statement:

| Probe | Method | Pass / decision evidence |
|---|---|---|
| User namespaces | `sysctl user.max_user_namespaces`, the NixOS option, start a namespaced user unit | If unavailable, namespaced user units fail to start; pure user-space cannot claim filesystem/network isolation |
| Same-UID process inspection | Can an agent `PTRACE_ATTACH`, read `/proc/<pid>/mem`, `/proc/<pid>/environ`, fds, and signal core/broker/sibling? | If yes, broker secrets and hard ceilings are not protected against same-UID code; pure user-space residual must be accepted or a helper chosen |
| Landlock | Filesystem restrictions, pre-opened fds, port-only network rules, ABI availability | Record which paths and ports are actually enforced |
| systemd user-manager directives | `ProtectHome=read-only` plus writable `$HOME` state and `$XDG_RUNTIME_DIR`; `PrivateNetwork=` fail-open behavior; `MemoryMax`, `CPUQuota`, `TasksMax`; `IOWeight` absence | Record the exact working set; detect "proceeding without" and fail closed |
| Broker identity | Unix-socket peer credentials under same UID versus dedicated UID; token theft; socket file permissions; cgroup check | Determines whether S-3/S-6 are kernel-enforced or conventional |
| Resource controls | cgroup v2 memory/pids/CPU limits, OOM victim, kill semantics, nested delegations | Hard ceilings are only "hard" if cgroup controls are enforceable |
| Recovery | Core crash, broker crash, supervisor crash, host reboot, orphan sandbox, ledger writer recovery, backup/restore | No orphan survives; interrupted effects become `outcome_unknown`; restore verifies references |
| Network and audit | Raw sockets, UDP/QUIC, loopback reachability to UI/ledger/broker, audit-sink write attempts | Records the actual audit gap and loopback exposure |
| Secret reachability | Can the agent read sops secrets, credential directories, broker memory, or `/proc`? | Determines whether secret isolation is kernel-enforced or residual |

---

### 4.11 Ingestion, interface, and multimodal state

#### 4.11.1 Ingestion invariants

1. **Observation is not truth.** A capture records what a source appeared to contain at capture time. It never creates approval, intent, or human authority. Human curation is the only path to authority records.
2. **No bytes, no capture.** Every capture observation references durable blob digests published before the observation becomes visible. The safe order is bytes → manifest → observation/coverage → checkpoint.
3. **Scans are idempotent.** Retrying a scan with the same logical attempt never duplicates captures, coverage receipts, or checkpoints.
4. **A scan is not a world snapshot.** Each observation carries its own capture time. A full scan produces a coverage receipt over a declared scope; it does not assert that all resources were observed at one instant.
5. **External changes never clobber speculative work.** They are recorded immediately, reconciled autonomously where non-conflicting, and surfaced as decisions where they change interpretation, acceptance, or prior decisions.
6. **One structured view specification serves agents and renderers.** Screenshots and accessibility trees are derivatives of a specific view revision, never substitutes for the specification.
7. **Retracted content is never presented as current.** Stale, pending, unknown, and conflicted state is visible next to the value, not hidden behind an unqualified "last updated" label.
8. **Canonical events and graph semantics stay internal.** AG-UI, A2UI, and MCP Apps are adapters and transport surfaces, not domain authorities.
9. **Plugin artifacts are untrusted.** They receive no credentials, no canonical append access, and no ambient network beyond their declared sandbox policy.
10. **Human authority is authenticated human authority.** A request that appears on loopback is not human consent. Authority endpoints require a verified Cloudflare Access identity or an explicitly spiked local equivalent.

#### 4.11.2 Closed source model and adapters

The vertical slice has a closed adapter set: Git, HTTP resource, web search, and local filesystem. There is no arbitrary source onboarding. A source is configured by a human and has a stable identity.

```ts
type SourceId = string; // stable, human-assigned

type SourceScope =
  | { kind: "git"; repoRoot: string; refs: string[]; includeWorktree: boolean }
  | { kind: "http"; seeds: string[]; allowedHosts: string[] }
  | { kind: "search"; queries: SearchQuery[]; allowedHosts?: string[] }
  | { kind: "fs"; roots: string[]; ignore: string[] };

type SourceDefinition = {
  sourceId: SourceId;
  adapter: SourceScope["kind"];
  scope: SourceScope;
  triggers: Array<"event" | "full" | "manual">;
  fullScanIntervalSec: number; // operator configuration
  missingGraceScans: number;   // default 2; tuning deferred
  capture: {
    maxBytes: number;
    mediaTypes: string[];
    followRedirects: boolean;
    renderDynamicPages: boolean;
    screenshot: boolean;
    accessibilityTree: boolean;
    ocr: boolean;
    pdf: boolean;
    audioVideo: boolean;
  };
  services: string[]; // broker logical-service names, never raw credentials
  enabled: boolean;
};

interface SourceAdapter {
  readonly kind: SourceScope["kind"];
  readonly version: string;

  capabilities(scope: SourceScope): {
    eventDriven: boolean;
    conditionalFetch: boolean;
    fullEnumeration: boolean;
    pagination?: { maxPage?: number; maxItems?: number };
    mediaTypes: string[];
  };

  scan(request: ScanRequest): Promise<ScanBatch>;
}
```

Adapters are producers, not writers. They return drafts and evidence. The host publishes blobs, appends observations, appends one coverage receipt, and only then advances the checkpoint. Adapters never open canonical storage.

#### 4.11.3 Observation, coverage, and scan state

```ts
type ObservationKind =
  | "capture"
  | "missing"
  | "git.ref"
  | "git.commit"
  | "git.rewrite"
  | "search.results";

type ObservationDraft = {
  idempotencyKey: string;
  kind: ObservationKind;
  sourceId: SourceId;
  resourceKey: string;        // stable logical key within the source
  locator: string;            // URL, path, ref, query, or commit locator
  capturedAt: Iso;            // host-assigned observation time
  sourceTime?: { value: Iso; precision: "instant" | "day" | "estimate" };
  evidence: CaptureEvidence;
  parentObservationId?: string;
  native?: unknown;           // provider-native payload, preserved opaquely
};

type CaptureEvidence = {
  request: {
    method: "GET" | "HEAD" | "SEARCH" | "GIT";
    locator: string;
    paramsHash: Sha256;
    conditional?: { etag?: string; lastModified?: string };
    providerRequestId?: string;
  };
  response: {
    status: string;           // "ok", "not-modified", "not-found", "forbidden", ...
    statusCode?: number;
    finalLocator: string;
    mediaType?: string;
    body?: Sha256;            // exact post-redaction response bytes
    rawEnvelope?: Sha256;     // exact provider envelope after redaction, when applicable
    size?: number;
    etag?: string;
    lastModified?: string;
    redirects?: string[];
  };
  capturedAt: Iso;
};

type ScanCoverage = {
  scanId: string;
  sourceId: SourceId;
  mode: ScanRequest["mode"];
  startedAt: Iso;
  completedAt: Iso;
  scopeHash: Sha256;
  manifestDigest: Sha256;     // hash of sorted (resourceKey, digest, status)
  counts: { seen: number; captured: number; unchanged: number; missing: number; errored: number };
  gaps: Array<{ resourceKey: string; reason: string; retryable: boolean }>;
  complete: boolean;
};

type ScanRequest = {
  sourceId: SourceId;
  scope: SourceScope;
  mode: "event" | "full" | "targeted";
  cursor: string | null;       // adapter-opaque; never interpreted by the host
  scopeHash: Sha256;
  scanId: string;              // logical scan identity
  attempt: number;             // retry identity for the same scan
  budget: { maxResources: number; maxBytes: number; deadline: Iso };
};

type ScanBatch = {
  observations: ObservationDraft[];
  coverage: ScanCoverage;
  nextCursor: string | null;
  errors: ScanError[];
  complete: boolean;
};

type ScanRunStatus =
  | "idle" | "enumerating" | "staging" | "appending" | "advancing"
  | "complete" | "partial" | "failed";
```

A coverage receipt is itself the evidence that a full scan ran and what it covered. Unchanged resources do not produce per-resource capture records; they are represented in `manifestDigest`. A resource transitioning to absent produces one `missing` observation after `missingGraceScans` consecutive successful full scans, or immediately on an explicit `404`/`410` plus policy. A `403` is `inaccessible`, not `missing`.

The four clocks stay distinct: capture time, source publication/update time, derivation time, and validity interval. File `mtime` MUST NOT be used as capture time.

Every source has two independent scan paths, plus targeted repair:

| Path | Trigger | Purpose | Checkpoint effect |
|---|---|---|---|
| Event | Git ref/worktree change, filesystem notification, provider notification, completed remote fetch | Low-latency capture of a targeted resource | Advances `eventCursor` only |
| Full | Periodic timer, mandatory for every source | Completeness: discovers missed events, deletions, permission changes, and scan gaps | Advances `full.cursor` and records coverage |
| Targeted | Human "scan now" or gap repair | Re-read one resource or scope | No full-coverage claim |

Normative behavior:

1. There MUST be at most one active full scan per source. Event scans for that source are queued or coalesced; they do not run concurrently with the full scan's enumeration phase.
2. Event delivery is an optimization. A dropped event is corrected by the next full scan. A full scan never relies on event history for completeness.
3. A full scan enumerates the declared scope, compares the enumerated set with the previous coverage manifest, captures new or changed resources, and emits one coverage receipt.
4. If a Git ref moves during enumeration, the scan finishes but marks coverage `partial` and schedules another scan; it does not pretend to have observed a single ref state.
5. If the budget expires, the scan returns `complete: false`; the checkpoint advances only to the adapter's `nextCursor`, and the next invocation continues. The coverage receipt records the partial scope.
6. A full scan that observes a changed resource updates the observed representation. It never mutates prior observations or prior speculative history.

#### 4.11.4 Checkpoints, idempotency, and evidence ordering

```ts
type SourceCheckpoint = {
  sourceId: SourceId;
  scopeHash: Sha256;
  adapterVersion: string;
  eventCursor: string | null;
  full: { cursor: string | null; lastCoverageId: string | null; lastCompletedAt: Iso | null };
  validators: Record<string, { etag?: string; lastModified?: string; digest?: Sha256 }>;
  updatedAt: Iso;
};
```

Checkpoints are host-owned operational state, not canonical truth. Losing a checkpoint forces a full scan; it does not lose observations.

Idempotency keys:

```text
observationKey = hash(sourceId, resourceKey, representationDigest, causeId)
coverageKey    = hash(scanId, attempt)
```

`causeId` is the event identity or the logical `scanId`, not the retry attempt. A retry of the same scan therefore deduplicates captures, while a later independent scan that discovers the same bytes does not create a second capture unless the representation changed from the last observed value. A revert to previously seen bytes is a change from the current representation and is recorded.

Ordering is mandatory:

1. Publish the exact post-redaction capture bytes and redacted provider envelope to the CAS, recording redaction metadata and the original commitment when computed. Unredacted secret-bearing bytes are not persisted by default.
2. Publish the manifest that references those digests.
3. Append capture observations referencing the digests.
4. Append the coverage receipt.
5. Advance the checkpoint.

A crash after step 1 or 2 leaves an orphan blob, reclaimed by safe orphan reclamation. A crash after step 3 but before step 5 causes a repeat scan; idempotency keys deduplicate. Advancing the checkpoint before the append is forbidden, because it converts a crash into a permanent missed observation.

#### 4.11.5 Failure and absence behavior

```ts
type ScanError = {
  code:
    | "auth" | "quota" | "rate_limit" | "upstream_forbidden"
    | "transient" | "invalid_request" | "unsupported_media"
    | "not_found" | "gone" | "too_large" | "schema_unsupported";
  resourceKey?: string;
  message: string;
  retryable: boolean;
  retryAfter?: Iso;
  providerRequestId?: string;
};
```

- A failed capture appends an error receipt, not an observation asserting content. The checkpoint does not advance past an ordered failure.
- A permanently unsupported resource is marked `unsupported` in coverage and the scan advances with a gap, so one bad resource cannot wedge a source.
- Provider quota exhaustion pauses the source and preserves the cursor. The UI displays `pending` and the last successful coverage, never a fabricated "current" state.
- An unavailable source is `unknown`, not empty. `unknown` is rendered distinctly from zero and from stale.
- Redirects are captured with the chain and final locator. Broker-mediated requests never forward credentials across hosts or redirects. Non-HTTP(S) schemes are rejected by the shared scanner.
- Source-supplied times are preserved separately. Clock skew never reorders canonical recording or rewrites capture time.

#### 4.11.6 Git and external commit handling

Git is an adapter and review surface, not the world database.

```ts
type GitCheckpoint = {
  refs: Record<string, string>;         // ref -> sha at last observation
  commitCursorByRef: Record<string, string>;
  worktreeDigest?: Sha256;
};

type GitObservation =
  | { kind: "git.ref"; ref: string; oldSha: string | null; newSha: string; forced: boolean }
  | { kind: "git.commit"; sha: string; parents: string[]; tree: string;
      authorTime: Iso; committerTime: Iso; messageDigest: Sha256;
      changedPaths: Array<{ path: string; status: "added"|"modified"|"deleted"|"renamed";
                            oldPath?: string; blobDigest?: Sha256 }> }
  | { kind: "git.rewrite"; ref: string; oldSha: string; newSha: string;
      commonAncestor: string | null; abandonedShas: string[] };

type ConflictAssessment = {
  externalChangeId: string;
  affectedPaths: string[];
  interests: Array<{
    kind: "interpretation" | "approval" | "plan" | "acceptance" | "speculative-work";
    id: string;
    paths: string[];
  }>;
  severity: "none" | "informational" | "decision-required";
  reason: string;
};
```

Behavior:

1. **New commits are captured individually.** A push is not one opaque event. Each commit records parents, tree, author/committer times, message digest, changed paths, and changed blob digests. Raw commit and blob bytes are captured; a range bundle MAY be captured as a supplementary artifact. The exact bundle format is deferred.
2. **Ref changes are observations.** `forced` is true when `oldSha` is not an ancestor of `newSha`. A force-push or history rewrite appends `git.rewrite` and retains every prior commit observation. Prior observations are never deleted or declared invalid merely because the ref moved.
3. **Local worktree changes are optional observations.** If enabled, dirty files and their digests are captured. The scanner never stages, commits, rebases, or resets. Agent writes are effects with receipts; external human edits are observations. Neither implies the other.
4. **Remote changes require a fetch effect.** A configured remote fetch is a capability-mediated external read with its own receipt. The adapter observes the resulting refs; it does not assume a network push notification exists.
5. **Conflict assessment is explicit.** `decision-required` applies when an external change touches a path cited by an interpretation, plan, approval scope, acceptance criterion, or in-flight speculative branch, or when a rewrite abandons a commit cited by an approval. Otherwise the change is reconciled autonomously: update the observed base, mark dependent projections stale, and schedule re-derivation. An approval whose basis hash no longer matches is marked `basis-stale`; it is not silently re-bound. The human may re-approve.
6. **No silent clobber.** Any write through a capability to a project path checks the observed digest or commit against the expected basis. A mismatch refuses the write and appends a `clobber_refused` receipt. If the mismatch is consequential, it also creates a conflict decision. This is a zero-tolerance gate.
7. **External commits are not approvals.** Commit authorship, messages, and merge actions are observations. They never amend intent or authorize a previously approval-required branch.

#### 4.11.7 Stable web shell and interface adapters

The interface is a stable web shell with coordinated graph, delta timeline, and global decision views. Agents choose or generate contextual visualizations and chat inside that shell. No TUI is built in the vertical slice; projections and catalogs remain renderer-neutral.

Stability is a contract, not an implementation accident:

- fixed routes for graph, delta timeline, and decision queue;
- fixed anchors that let agents and renderers address the same objects;
- a fixed action vocabulary for curation gestures and decisions;
- presentation, themes, and contextual components MAY change through candidate activation;
- removing an anchor or a decision affordance fails candidate health checks.

The canonical event and graph semantics stay internal. The presentation stack has four layers:

| Layer | Contract | Kanbei use |
|---|---|---|
| Event/run protocol | AG-UI | Adapt runs to AG-UI; canonical events remain internal. |
| Declarative UI payload | A2UI catalog | Render ordinary catalogs; graph topology lives in the A2UI data model consumed by custom `kanbei.graph` components. |
| Renderer registry | Host-owned catalog and component registry | Resolve only declared components; no arbitrary code in a catalog. |
| Sandboxed artifact | MCP Apps-compatible boundary | Isolate arbitrary plugin artifacts; no credentials, no canonical access. |

Interface contract resolutions:

| Issue | Resolution |
|---|---|
| A2UI specification site says v1.0 is current while the repository describes v1.0 as a release candidate. | Pin the exact A2UI specification commit and catalog version. The adapter declares the version it emits and never silently downgrades. Exact pin is `[SPIKE: UI-1]`. |
| A2UI is a tree of components while Kanbei's primary object is an arbitrary graph. | Graph topology lives in the A2UI data model consumed by custom `kanbei.graph` components. The component tree is not the graph model. |
| AG-UI requires one `resume` to cover every open interrupt and forbids partial resumes, while the product needs individual rulings. | Canonical rulings are independent records. A run opens at most one interrupt group; individual rulings inside the group leave the run interrupted until the group is terminal. Multi-decision groups are `[SPIKE: UI-2]`. |
| The web shell must be stable while UI is live-mutable. | Stability is the fixed-route/anchor/action contract; themes and contextual components activate through candidates; health checks reject anchor removal. |
| Views own presentation but human interactions become records. | Judgment gestures append canonical human-authority records. Passive navigation, expansion, and filtering append presentation events only, usable for replay but not authority. |
| TinyFish Fetch rejects images. | Search/fetch is capability-based. TinyFish is the search provider and a fetch transport; image, PDF, audio, video, and screenshot capture may use another broker-mediated transport, recorded explicitly. Transport selection is `[SPIKE: NET-1]`. |
| MCP Apps defers state persistence and cross-view communication. | MCP App view state is ephemeral. Durable plugin state uses a scoped store behind host-mediated tools. Live server push into an open view is not assumed; polling or tool-result delivery is the baseline. |
| The UI binds loopback, but Cloudflare Access only gates ingress. | Authority endpoints require a verified Access JWT even on loopback. Local access without an Access session is read-only unless a local human-auth mechanism is spiked. |
| External changes in a code-generation host are sometimes handled by "do not push during generation." | Kanbei records external commits immediately and reconciles concurrently; conflicts that change interpretation, acceptance, or prior decisions surface as decisions. |

Human graph edits are curation gestures that append merge, split, link, scope, correction, retraction, and promotion records. The mutable UI renders the graph and timeline; approval evidence and rulings are displayed and submitted only by the frozen minimal consent surface, which is independent of the world UI. Agents receive the same versioned structured view specifications as renderers, plus derived screenshots and accessibility trees when useful. A screenshot or accessibility tree MUST be tied to the exact view revision it depicts and MUST NOT substitute for the specification.

#### 4.11.8 Multimodal artifacts

- Arbitrary files are content-addressed in the CAS. Provenance, previews, and derived OCR/text/metadata are provided for text, code, images, screenshots, and PDFs.
- Audio and video are stored without requiring full semantic indexing initially.
- Canonical raw objects remain distinct from derived transcripts, OCR, embeddings, and thumbnails.
- Derived artifacts are separate entities with typed derivation edges; they never overwrite or replace the raw object.
- Capture time, source publication/update time, derivation time, and validity interval remain distinct and are never conflated.
- Selectors MUST be redundant where feasible: a text quote plus position, a bounding box plus structural selector, a media timestamp plus transcript offset.
- A derived artifact whose source is retracted becomes `suspect` immediately at read time and is excluded from current presentation until rebuilt.
- A view artifact carries its view revision, catalog version, basis, and coverage.

---

## 5. Alternatives considered

### 5.1 Architectural center

| Direction | Canonical center | Best property | Structural weakness | Why not chosen as the center |
|---|---|---|---|---|
| Truth-first adjudicated ledger | Observations, human intent, decisions, claims, derivation history | Can answer why the system believes something and what changed without erasing dissent | Reconciliation can become an expensive, unreadable second program | Chosen, with a deliberately narrow authority model. Reconciliation is a supporting projection, not a universal truth engine. |
| Change-first world repository | Git/JJ commits, branches, merges, generation diffs | Review, external changes, nested work, and self-modification use one familiar mechanism | VCS is not a temporal, query, or memory engine; progress noise can destroy reviewability | Coding would be easier, but semantic memory is weighted equally and cannot become thin. Git remains an adapter. |
| Convergence-first desired-state reconciler | Accepted desired state plus observed state and generation cursors | Directly automates movement from current world to intended world | Free-form intent does not naturally become contracts; green probes can certify the wrong goal | Desired/observed is a supporting projection. Formal contracts are not required for every goal. |
| Attention-first decision console | Decision requests, rulings, precedents, review scheduling | Optimizes the scarce human resource and concurrent steering | World state and memory become supporting views; self-modification lacks an authority model | The interface cannot be optimized before its rulings have meaning. Attention is a projection. |
| Boundary-first observation plane | Raw captures, capability warrants, effect budgets, broker receipts | Strongest answer to prompt-injected arbitrary code and secret containment | Custom broker/cache/sandbox work can become the product; intent and judgment remain under-specified | Security is cross-cutting and must be strong, but it is not the product's organizing center. |

### 5.2 State substrate

- **Pure event sourcing.** Good generic fit for heterogeneous classes and history, but has no native blobs, bitemporality, or taxonomy. Kanbei uses an append-only ledger but adds typed components, blobs, causal links, and explicit projections.
- **Bitemporal immutable rows (XTDB).** Strong native valid time, but single-writer and operationally immature for this purpose; Kanbei keeps valid time optional rather than making it a storage requirement.
- **Transaction-time datoms (Datomic).** Strong audit and graph references, but no native valid time, large-value limits, and non-transactional excision. Not chosen.
- **Content-addressed commit/op DAG (Git/JJ/Irmin/lakeFS).** Good integrity, dedup, and lineage, but no query, transactions, or semantic taxonomy. The blob layer borrows the content-addressing idea; the repository does not become the ledger.
- **CRDT operation DAG.** Converges concurrent edits deterministically, but the winner is causal-ID-based, not intent-aware, and there is no as-of semantic query. Not used as truth.
- **Kubernetes-shaped level-triggered objects.** Closest native intent/observation split, but status history is explicitly best-effort. Kanbei borrows `desired`/`observed`/generation vocabulary without adopting the object shape.
- **Row-level changesets.** Compact heterogeneous row deltas, but primary-key-bound, JSON-opaque, and coalescing. A merge primitive, not a history.
- **Kafka/Materialize/Feldera/RisingWave/Flink as state authority.** Projections without authority, retention deletes history, and operational weight is high. Kept only as projection-engine references.
- **Iceberg/Delta/lakeFS as world state.** File or snapshot granularity with no row or agent attribution. lakeFS remains only a blob/version reference.
- **Full RDF/OWL ontology.** High modeling cost; PROV/RDF-star are provenance vocabularies, not the memory model.
- **Fine-tuning facts into weights, blockchain ledgers, pure vector RAG, full-context replay.** Not queryable, not provenance-bearing, or not entity-aware.

### 5.3 Durable execution

- **Temporal and similar deterministic workflow engines.** Strong replay, child lifecycle, and HITL primitives, but event-history caps, rotation/Continue-As-New loss of pending signals and children, and retention limits make them a second workflow authority. Kanbei implements native ledger durability.
- **Restate.** Journaled RPC and durable promises are attractive, but the journal is not model context, retention is bounded, and it would duplicate ledger authority.
- **DBOS.** Simple step checkpointing, but deterministic discipline and external-effect keys remain application work; a second control plane for multi-process operation.
- **LangGraph.** Integrated HITL and time travel, but node restarts from the beginning, checkpoints grow, and durable cancel is absent.
- **Inngest/Cloudflare step memoization.** Fine-grained flow control, but step limits, re-execution outside steps, and no full replay.
- **OpenAI SDK/OpenHands pause snapshots.** Good approval pause and mid-run injection, but snapshots are unauthenticated, approvals are not exactly-once, and there is no scheduler.

Kanbei's substitute cost is checkpoint volume and replay time. Checkpoint cadence is therefore a first-class setting, and the design accepts that a long-lived loop cannot rely on a fixed event-history cap.

### 5.4 Semantic memory

- **Graphiti/Zep.** Temporal fact graph with LLM contradiction resolution, but retrieval can return invalidated facts by default, invalidation has no inverse, and summaries are not rebuilt on episode deletion.
- **Mem0 Dream.** ADD-only writes with background supersede/merge/synthesis, but platform-only, no validity interval, unbounded growth, and no documented un-merge.
- **Letta MemFS.** Git-backed Markdown and dreaming subagents, but no typed facts, no temporal validity, and renames break identity.
- **A-MEM.** Zettelkasten notes with generated links, but memory evolution overwrites notes without recording cause.
- **PROV-O.** Useful provenance vocabulary for merge/split/retraction as activities over immutable nodes; not a complete memory store.
- **BoostER-style partition posteriors.** Mathematically interesting, but no accuracy results and a super-exponential space.
- **MEMOIR/SUMONE compaction.** Evidence-preserving compaction is attractive but unreplicated; aggressive compaction loses evidence. Kanbei keeps originals and makes compaction non-destructive.
- **Poisoning and scope research.** All memory systems are susceptible; extraction can filter but not eliminate. Kanbei enforces scope at read time and supports quarantine.

No surveyed system provides calibrated confidence or reversible entity split. Kanbei therefore treats agent confidence as uncalibrated metadata and makes split a non-destructive projection operation.

### 5.5 Self-modification and plugin runtime

- **Cordis and DeepSeek Harness.** Effect/coeffect runtime, scoped registry, monotonic guards, revisioned settings, and append-only session logs are strong patterns, but their APIs are unstable and HMR requires Node internals and is unit-atomic. Kanbei borrows patterns, not dependencies. Its own candidate protocol provides atomic scope activation and rollback.
- **Erlang/OTP hot swap.** Two-version code and `on_load` gates are strong, but BEAM-only, sequential rather than atomic, and purge kills lingering processes.
- **Home Assistant config entries.** Version contracts and lifecycle states are useful, but per-entry scope and runtime state persistence are insufficient.
- **Nix/Home Manager generations.** Immutable builds, atomic symlink, and whole-generation rollback are strong; activation side effects are not atomic, rebuild latency is high, and granularity is coarse. Nix remains the trust-root delivery mechanism.
- **Emacs/Macros live image.** Full-trust live eval and first-class keybinds/themes, but no transaction, migration, audit, or isolation.
- **VS Code contribution points.** JSON schemas for config, keybindings, themes, menus, and tools, but not embeddable, reload-bound, and no effect reversal.
- **Zed WASM extensions and pen.dev document/theme model.** Useful isolation and declarative UI references, but not a complete plugin/keybind transaction model.
- **Skills-as-plugin and MCP-as-plugin.** Prompt files and tool transport, not lifecycle or capability systems.

### 5.6 Programmable runtimes and interaction patterns

- **Monty.** Language-level Python subset with no ambient authority and interpreter snapshots, but excludes inheritance, generators, `async with`, `del`, and third-party packages; snapshots are trusted-only.
- **WASI CPython on Wasmtime.** Near-full CPython with capability preopens, but no sockets, subprocess, threads, or ctypes, and no mid-execution snapshot.
- **Full CPython in an OS sandbox.** Any package, but the OS sandbox is the boundary.
- **Deno runtime.** Permission flags are an inner boundary; Deno Sandbox is excluded as a candidate because it is closed to new customers as of current evidence.
- **Anthropic/OpenAI programmatic tool calling and Cloudflare Code Mode.** Interaction patterns, not interchangeable runtimes; no in-process interpreter is accepted as the outer boundary.
- **Pyodide, RestrictedPython, `node:vm`, `isolated-vm`, `rbash` as security boundaries.** Explicitly disclaimed by their own documentation; usable only as speed bumps.
- **WASM determinism.** Only fuel-based interruption is deterministic; epoch-based interruption is not.

The language and Node-versus-Deno choices are deferred to the fair bake-off in §4.9.5.

### 5.7 Interface and blob storage

- **AG-UI, A2UI, json-render, OpenUI, MCP Apps.** These are different layers, not competing protocols. AG-UI supplies the run event/interrupt contract; A2UI supplies declarative payloads; MCP Apps supplies the sandboxed artifact contract. None defines a graph primitive, so Kanbei adds custom graph components over the A2UI data model.
- **json-render.** The only declarative spec with a first-class TUI target, but v0.x Labs status, no runtime, and no HITL. TUI parity is out of slice scope.
- **CopilotKit, Vercel AI SDK UI, ChatKit.** Framework or hosted layers, not domain protocols. CopilotKit thread replay/locks are commercial; AI SDK UI approvals are client-supplied unless signed; ChatKit is OpenAI-hosted.
- **Chunk-level CAS (restic/casync).** Strong dedup, but backup-snapshot models, global prune, and chunker coupling complicate a local library. Kanbei starts with whole-object OCI-layout CAS behind `BlobStore` and defers chunk dedup.
- **Claim-check stores.** Keep ledger records small but make write and publish explicitly non-atomic. Kanbei accepts the orphan window and uses strict publication ordering plus reclamation.
- **S3/lakeFS versioning.** Mature but service-dependent; Kanbei's local CAS is simpler and matches the single-host target.

### 5.8 Security and deployment

- **bwrap + Landlock + seccomp + netns, gVisor, Firecracker, WASM/WASI, systemd hardening.** Confinement families differ materially. Kanbei requires an OS sandbox and defers the lightweight inter-agent mechanism; a container per agent is excluded.
- **Egress proxies and allowlists.** They cannot prevent exfiltration when outbound network is deliberately unrestricted. The broker is a credential-injection path, not an egress choke point.
- **Vault/OpenBao dynamic leases, SPIFFE/SPIRE, systemd credentials, SOPS, Sigstore.** Useful components, but no surveyed OSS component provides per-request revocation or approval quorum bound to one effect. Kanbei uses a custom logical-host broker and documents the residual.
- **Policy engines (OPA/Cedar/OpenFGA).** They decide but do not enforce. Kanbei keeps enforcement in the frozen append, broker, supervisor, and ceiling paths.
- **Prompt-on-every-action.** Contradicts the YOLO-with-boundaries requirement and causes approval fatigue.

---

## 6. Risks and accepted residuals

### 6.1 Risk register

| Risk | Impact | Mitigation | Residual |
|---|---|---|---|
| Malicious readable code exfiltrates project data | Confidentiality loss | Accepted unrestricted egress; audit and attribution only | **Accepted and explicit.** No confidentiality guarantee. |
| Same-UID agent inspects broker memory, ceilings, or sibling processes | Secret and boundary compromise under pure user-space deployment | `[SPIKE: DEP-1]` tests ptrace/process isolation and dedicated-UID helper; fail closed on weaker posture | Pure user-space cannot claim kernel-enforced secret isolation. |
| Enrolled credential is used as a credential oracle | Provider-side damage within the credential's authority | Least-privilege guidance, one-time human enrollment, revocation, short-TTL credentials where available | Broker does not authorize methods, paths, or payloads. |
| Prompt injection causes harmful tool composition | Unintended effects within granted capabilities | OS sandbox, capability scopes, approval tiers, effect receipts, hard ceilings | Agent has discretion within its grants. |
| Semantic reconciliation is wrong, over-merges, or under-splits | Misleading derived memory | Non-destructive merge/split, stable identities, projection versioning, contested alternatives, audits | No calibrated confidence or proven split correctness. |
| Poisoning through ingested content | Contaminated memory | Observations and candidate claims only; read-time scope; quarantine; rebuild | Detection is best-effort. |
| Approval fatigue or unreadable contested state | Unsafe auto-approval or ignored decisions | Global deduped queue, batch rulings without scope union, review-after, deadline grouping | Human attention remains the limiting resource. |
| Checkpoint and replay volume grows without bound | Storage and recovery cost | Native ledger durability, checkpoint cadence, non-destructive compaction, indefinite retention default | Long-loop replay cost is a build risk. |
| CAS orphan or dangling reference | Storage leak or missing evidence | Bytes → manifest → ledger ordering; pins; safe reclamation; restore verification | Crash between byte and manifest leaves an orphan. |
| Live arbitrary plugin breaks or is malicious | Service disruption or data loss | Candidate validation, shadow state, health checks, rollback, quarantine | External effects are not rolled back. |
| Nix rollback after forward-only migration | Old kernel cannot read new state | Compatibility gate, `roll_forward_only`, checkpoint-branch recovery | Rollback may be blocked. |
| UI or loopback request mistaken for human consent | Unauthorized authority record | Frozen consent surface mints `consentDigest`; gesture endpoint recomputes it; Access-verified endpoints; loopback is not consent | Local auth without Access is unresolved (`[SPIKE: ING-1]`). |
| Ambiguous external effect | Duplicate or unknown side effect | Idempotency keys, journal, `outcome_unknown`, read-only probe, human-authorized new attempt | Exactly-once is never claimed. |
| AG-UI multi-decision interrupt grouping | UI cannot express all concurrent rulings | One interrupt group per run; individual rulings; `[SPIKE: UI-2]` | Multi-decision groups are deferred. |
| Agent edits its own context to hide evidence | Self-deception or audit evasion | Canonical ledger and human views read outside the context scope; activation diff records visibility changes | Agent context can differ from full evidence. |
| Concrete ceilings or thresholds are set poorly | Cost overrun or excessive restriction | Must be operator configuration; baseline spikes inform values | Values are deferred. |

### 6.2 Accepted residuals

1. **Exfiltration.** Malicious readable code can exfiltrate anything it can read. Unrestricted outbound network is a deliberate requirement, not an implementation gap.
2. **Same-UID inspection.** Under a pure user-space deployment, same-UID agents may inspect broker memory or signal siblings unless the host security spike proves otherwise. Broker caller checks are attribution, not a boundary, in that mode.
3. **Credential authority.** Any credential may be enrolled; agents can exercise its full provider-side authority. The broker does not constrain method, path, or payload.
4. **Response redaction.** Redaction is best-effort and cannot recover transformed, encrypted, compressed, or re-encoded secrets.
5. **External effects.** There is no exactly-once delivery and no replay of effects. Ambiguous termination yields `outcome_unknown`.
6. **LLM fidelity.** Hidden chain-of-thought is not promised; replay uses recorded I/O and never promises bit-identical regeneration.
7. **Semantic truth.** Derived memory can be wrong. Human promotion records human attestation, not objective truth.
8. **Poisoning.** Quarantine and rebuild reduce blast radius but cannot guarantee detection or complete removal from derived artifacts.
9. **Erasure.** Physical erasure creates an epoch and covers derived artifacts and backups as far as possible; copies outside Kanbei's control cannot be guaranteed removed.
10. **Experimental plugin posture.** Live arbitrary plugins are allowed outside the trust root as an experiment. This MAY be tightened if practice shows unacceptable risk.

---

## 7. Validation

This section is the sole normative validation authority. Where any other document, harness description, or implementation plan differs, this section controls. `spec/06-validation-plan.md` and `spec/08-implementation-plan.md` may supply harness mechanics and implementation-task traceability (the plan owns its own `A0`–`I5` tasks); they MUST NOT define gate identifiers, datasets, protocols, or pass criteria that differ from this section.

### 7.0 Evidence contract

Validation is evidence-producing, not assertion-producing. Every run emits an immutable `EvidenceEnvelope` bound to the exact canonical head, fixture version, environment fingerprint, seeds, recordings, and artifacts. A run without an envelope is not a result.

```ts
type Outcome = "pass" | "fail" | "inconclusive" | "blocked";

interface EvidenceEnvelope {
  testId: string;                       // "ZT-01", "AS-03", "C-09", "V5", ...
  invariantIds: string[];               // I-*, or [] for pure gates
  fixtureVersion: string;
  canonicalHead: { seq: number; hash: string };  // hash in sha256:<hex> form
  sequenceRange: [number, number];
  environment: {
    hostFingerprint: string;
    kernel: string;
    runtime: string;
    sandboxPosture: string;
    brokerIdentity: string;
  };
  seeds: number[];
  modelRecordings: string[];            // post-redaction CAS content digests
  artifacts: string[];                  // CAS content digests
  outcome: Outcome;
  failureClass?: string;
  metrics?: Record<string, number>;
  acceptedResiduals?: string[];
}
```

**Datasets.** `fixtureVersion` MUST be `<dataset-id>@<semver>` from this registry; an unregistered value is not a result. `journey-v1` is the sealed end-to-end fixture covering two human-selected projects plus Kanbei itself.

| Dataset | Contents | Split / use |
|---|---|---|
| `journey-v1` | Sealed multi-project slice: two project repositories plus Kanbei, a structural approval, nested deltas, an external commit, a scoped live candidate, and memory events. | Acceptance only. |
| `append-v1` | Canonicalization, unknown-component, hash-chain, and lifecycle-transition corpus. | `unit`/`property` and `V13`. |
| `memory-v1` | Labelled claim, contradiction, merge, split, and retraction stream. | Development and tuning. |
| `memory-holdout-v1` | Sealed holdout split of `memory-v1`; labels frozen before tuning. | Final reconciliation metrics only. |
| `external-v1` | Git ref moves, rewrites, external commits, HTTP captures, and conflict cases. | Acceptance and conflict tests. |
| `security-v1` | Hostile code, prompt injection, broker abuse, trust-root probes, and credential canaries. | Red team and zero-tolerance gates. |
| `cost-v1` | Model, broker, and host-call workload with usage traces. | Ceilings and quality thresholds. |
| `ui-v1` | Ruling comprehension, contested state, stale state, and presentation-replay corpus. | UI tests and comprehension thresholds. |

Test classes:

| Class | Purpose | Gate behavior |
|---|---|---|
| `unit` / `property` | schema, hash chain, lifecycle transitions, parser and canonicalization | blocks merge on failure |
| `conformance` | acceptance scenarios and cross-contract behavior | blocks merge on failure |
| `chaos` | crash, fault injection, recovery | nightly and release |
| `redteam` | hostile code, prompt injection, broker, trust root | release-blocking and nightly |
| `spike` | deferred-decision measurements | produces gate input; never a silent choice |
| `bench` | cost, latency, freshness, UI comprehension | release-blocking against configured ceilings; quality thresholds set at gate |

Rules:

1. Zero-tolerance tests never retry. A flaky zero-tolerance test is itself a defect; the run is `blocked`, not `pass`.
2. Deterministic tests use a virtual clock, synthetic network, and recorded model I/O. Live-model tests are separate and assert structure and provenance, never bit-identical generation.
3. Fixtures are sealed and content-addressed. The reconciliation resolver is never tuned on a holdout split after labels are frozen.
4. Spike results are paired comparisons with pre-registered decision rules; an `inconclusive` result blocks the dependent decision gate and never becomes a default choice.
5. Every gate table below states its pass condition. A gate is decidable only from its listed test IDs and the `EvidenceEnvelope` outcome.
6. Exact-I/O evidence is post-redaction. `modelRecordings` and every canonical model/search artifact name post-redaction CAS digests; persisting pre-redaction credential-bearing bytes is a `ZT-01` failure. "Raw" in any harness or plan description means the exact post-redaction wire bytes, never pre-redaction content.

### 7.1 Zero-tolerance gates

| ID | Gate | Failure condition | Pass criterion | Required failure behavior |
|---|---|---|---|---|
| `ZT-01` | Secret exposure | Enrolled credential, broker secret, or trust-root secret appears in agent env, argv, files, model context, tool output, UI, logs, CAS, or canonical store; or a known enrolled value crosses to a non-enrolled host or redirect | Zero canary hits in every scanned surface; broker redirect and cross-host tests deny without secret | Fail closed; request rejected; evidence records location and class |
| `ZT-02` | Ledger bypass | Mutable agent/plugin writes canonical SQLite or bypasses the frozen append API | Every bypass attempt denied; no unauthorized record visible | Deny and record attempt; sandbox/supervisor alarm |
| `ZT-03` | Silent external clobber | External change overwrites speculative history or is not recorded immediately | External commit recorded; speculative branch preserved; conflict surfaced | External commit recorded; conflict escalated |
| `ZT-04` | Effects outside hard policy | Effect occurs outside sandbox, broker, global resource, frozen-root, or autonomy-tier policy | Every probe denied before effect; denial receipt present | Deny before effect; receipt records policy and decision |
| `ZT-05` | Missing required provenance | Claim or completion materializes without required source/derivation/scope/effect citation | Injected dangling claims rejected from materialization; completion without verified citation denied | Reject from materialization; defect surfaced |
| `ZT-06` | Retracted claim presented as current | Read returns a retracted or quarantined claim as current, even during rebuild | Zero stale-current reads across retract-then-read races | Return suspect/absent with coverage watermark |
| `ZT-07` | Replay repeats external effects | Replay invokes any external effect | Zero capability dispatches during replay | Effect blocked and recorded; replay aborts |
| `ZT-08` | Approval binding violation | Effect proceeds under a ruling with mismatched anchor, interpretation, plan, subject, evidence-set, scope, or expiry; or under a receipt revoked at the evaluation basis | Every mutated-field and stale-ruling case denies | Deny and append stale/blocked record |
| `ZT-09` | Integrity failure | Hash-chain break, tampered record, tampered blob, or unverifiable restore is accepted | Mutation, truncation, and tampered restore all refuse | Startup/read/restore refuses; integrity incident |
| `ZT-10` | Trust-root mutation | Agent or plugin edits the frozen trust root or activates a kernel change outside a human-approved generation | All trust-root write and activation probes denied | Deny; activation refused; audit record |
| `ZT-11` | Unrecorded human authority | Agent manufactures approval, veto, amendment, promotion, or a plugin appends `human_authority`/`activation_receipt`/`effect_receipt` | Injected authority-shaped payloads classified as observation/candidate; gesture endpoint rejects non-human principals; receipt appenders are frozen only | Reject; classify as observation/candidate only |
| `ZT-12` | Coverage dishonesty | Projection claims currency outside its watermark or hides pending work | Every read outside coverage reports `stale`/`building` and pending counts | Return coverage, staleness, and pending counts |
| `ZT-13` | Consent display, token, and origin | A ruling is minted for evidence, scope, or hashes the human did not see; gesture `consentDigest` differs from the frozen surface's rendering; a ruling commitment does not recompute from the persisted `ConsentDisplayManifest` preimage over `consentDigest`, ruling, and token hash; one token is used for more than one ruling; or a token is reused, expired, replayed from the mutable-UI origin, or submitted through a clickjacked frame | Every decoy-render, manifest, ruling-mismatch, cross-origin, replay, reused-token, and clickjack case rejects with `APPROVAL_DISPLAY_MISMATCH` or `APPROVAL_TOKEN_INVALID`; mutable UI cannot read, mint, or recompute a token or digest | Deny; append blocked-display record; no receipt |
| `ZT-14` | Unvalidated lifecycle transition | A transition outside its family table, with a forbidden actor, a stale `expect.from`, or a stale fencing token is applied | Every fabricated `reactivate`/promotion/claim case rejects | Deny with `TRANSITION_REJECTED`/`STATE_CONFLICT`/`FENCING_STALE` |

`ZT-01`–`ZT-06` are the six required gates from `req. 158`; `ZT-07`–`ZT-14` are derived prerequisites without which the six cannot be enforced. The secret-exposure scope is enrolled credentials, broker state, and trust-root secrets; project-data exfiltration by readable code is the accepted residual, not a `ZT-01` failure.

### 7.2 Invariants and mechanical checks

Canonical invariants. Each is asserted by its listed gates or scenario families and fails the release if violated.

| ID | Invariant | Asserted by |
|---|---|---|
| `I-01` | Canonical records are append-only; correction and retraction append records. | `ZT-09`, `AS-07`, `V5` |
| `I-02` | One sequencer assigns one global recording sequence; causal links carry partial order; committed sequence has no duplicates or gaps. | `C-01`, `C-02` |
| `I-03` | Unknown namespaced components survive round trips byte-identically at the component layer. | `V13`, `append-v1` corpus |
| `I-04` | Desired and observed state are separate optional linked components with distinct authority paths. | `ZT-04`, `V14` |
| `I-05` | Human authority records cannot be created by an agent channel; observations and candidate claims are distinguishable from authority. | `ZT-11`, `AS-02`, `AS-03` |
| `I-06` | Retracted or quarantined claims are excluded at read time before any asynchronous rebuild completes. | `ZT-06`, `AS-07`, `P-03`, `P-09` |
| `I-07` | Every materialized claim has required provenance: source observation, source span or selector, derivation, and scope. | `ZT-05`, `AS-01`, `AS-06` |
| `I-08` | Blob bytes are durable before manifest publication; the manifest is durable before ledger visibility. | `B-01`, `AS-01` |
| `I-09` | External effects are attempted at least once with idempotency keys where supported; ambiguous interruption yields `outcome_unknown` and no blind retry. | `C-14`, `R-07`, effect-state tests |
| `I-10` | Replay never performs an external effect. | `ZT-07`, `R-04` |
| `I-11` | Approval binds exact anchor, interpretation hash, resolved plan hash, ruling work-item ids and current WorkGeneration ids, completion subject, authority evidence-set hash, granted tier, max effect class, authorized scope, constraints, expiry, and single-use consent display digest/token. | `ZT-08`, `ZT-13`, `AS-03`, `C-08` |
| `I-12` | Sandbox, broker, and global resource policies are hard boundaries; delegated-intent constraints are not. | `ZT-04`, `C-10`, `C-11` |
| `I-13` | Mutable agents and plugins cannot write canonical storage or the frozen trust root. | `ZT-02`, `ZT-10`, `AS-08` |
| `I-14` | Activation stages, validates, flips one scope pointer atomically, health-checks, and retains the previous candidate; external effects remain. | `AS-08`, activation crash matrix |
| `I-15` | Backup captures a consistent ledger snapshot and manifest set with GC paused; restore verifies every reference and rebuilds derived indexes. | `B-05`–`B-09`, `AS-11` |
| `I-16` | Projections expose exact coverage and pending work; "current" is never claimed outside the watermark. | `ZT-12`, `AS-12`, `U-07` |
| `I-17` | Undo creates a new branch from a checkpoint and preserves exact available LLM I/O and all descendants. | `R-05`, `R-06`, `AS-10` |
| `I-18` | Every lifecycle transition is validated by the frozen append path against its per-family actor/event/guard table, with state CAS and fencing where the family defines a lease. | `ZT-14`, `V18`, `C-01`, `C-14` |
| `I-19` | Stable-domain writes are authorized by (authority class, appender role, component type); agents and plugins cannot append effect/activation receipts, promotions, or attestations, and enrollment revocation is a projection-visible transition. | `ZT-11`, `ZT-14`, `V19`, `V22`, `AS-08`, `AS-09` |

Mechanical checks run in CI and at startup:

| ID | Check | Failure behavior |
|---|---|---|
| V1 | Every work item has at most one ownership parent; ownership forest acyclic; causal DAG acyclic | Reject append with `OwnershipCycle` / `CausalCycle` |
| V2 | No `completed` without criterion evidence, required blockers, and absence of blocking `outcome_unknown` | Promotion denied |
| V3 | No `in_flight` effect without durable `admitted` row, key, and fencing token | Dispatch refused; incident |
| V4 | No automatic transition from `outcome_unknown` to any retry state | Alarm; zero-tolerance gate |
| V5 | Every coverage check recomputes anchor, interpretation hash, resolved plan hash, ruling work-item ids, current WorkGeneration ids, completion subject, authority evidence-set hash, granted tier, max effect class, constraints, scope, `consentDigest` from the persisted `ConsentDisplayManifest` preimage, the ruling commitment over that digest, the ruling, and the token hash, and token consumption at admission | Deny; log mismatch |
| V6 | Override never changes tier, scope, envelope, evidence hash, or a `reject` ruling | Append rejected |
| V7 | Undo creates a branch and leaves descendants intact | Undo rejected |
| V8 | Batch ruling emits one receipt per request with distinct bound hashes | Batch rejected |
| V9 | Replay performs zero capability dispatches | Abort replay; incident |
| V10 | Model I/O records are immutable and reasoning absence/redaction is explicit | Append rejected |
| V11 | Duplicate `(consumer_id, message_id)` cannot produce two processed inbox rows | Unique constraint |
| V12 | Missed timers fire at most once per timer ID after recovery | Deduped |
| V13 | Unknown components round-trip byte-identically through append, export, backup, restore, and read | Append/export rejected; incident |
| V14 | No record carries both desired and observed components; at most one `owns` parent | Append rejected |
| V15 | Every retraction target appears in `suspect_record` by `record_seq` and `record_id`, and the resolver join sees it | Read fails closed; incident |
| V16 | Every canonical `EffectReceiptRecord` names a minted `request_id`, attempt, fencing token, and `EffectId`; `subject_hash` is present iff `subject_kind="completion"` and then equals the current WorkGeneration's completion subject; a `reconciled_*` outcome names `reconciles_receipt_id` | Append rejected |
| V17 | No stale `state` field exists on a canonical payload type | Schema conformance fails |
| V18 | Every transition matches its family table, actor predicate, `expect.from`, and fencing token; no reactivation row exists for a receipt or override | Append rejected with `TRANSITION_REJECTED` / `STATE_CONFLICT` / `FENCING_STALE` |
| V19 | Every cited evidence outcome matches the cited record; `pass` requires `succeeded` or `reconciled_effective` and a matching completion subject | Promotion denied |
| V20 | `credential_enrollment` has no stored status; the broker reads only the `enrollment` lifecycle projection, and revocation changes that projection in the same transaction | Request denied; incident |
| V21 | Retraction closure uses the reverse indexes; no closure query performs a full scan, and a read that exceeds its closure budget returns suspect rather than current | Query rejected; incident |
| V22 | No agent or plugin appends `effect_receipt`, `activation_receipt`, `human_authority`, `delegated_authority`, `security_action`, `kernel_generation`, promotion, or attestation | Append rejected with `AUTHORITY_DENIED` |

### 7.3 Scenario tests

Acceptance scenarios (`AS-*`). Each passes only when its assertions hold against the sealed `journey-v1` fixture and the listed invariants.

| ID | Scenario | Pass criterion | Invariants |
|---|---|---|---|
| `AS-01` | Ingest | Pinned commits ingested; raw observations carry commit/tree/blob provenance; derived text is separate; coverage is per source; re-ingest is idempotent. | I-02, I-07, I-08, I-16 |
| `AS-02` | Explore intent | Anchor immutable; each interpretation cites spans and assumptions; a consequential branch is blocked without an accepted interpretation or autonomy policy. | I-05, I-11 |
| `AS-03` | Approve structural goal | Agent cannot grant its own tier; queue groups by intent and deadline; dedupe never merges different evidence sets; the frozen consent surface displays the exact evidence/scope/hashes and the ruling binds plan hash, work-item ids, completion subject, evidence-set hash, consent digest, scope, constraints, and expiry. | I-05, I-11, I-12 |
| `AS-04` | Execute nested deltas | Each delta has one ownership parent plus causal links; completion requires verified evidence and cleared blockers; compliant children proceed; out-of-scope children escalate. | I-02, I-11, I-12 |
| `AS-05` | Reconcile external commit | External commit recorded immediately; speculative history preserved; decision-required conflict surfaced. | I-01, I-06, `ZT-03` |
| `AS-06` | Reconcile memory | Contradicting and undercutting evidence distinct; human promotion binds canonical source ids; confidence metadata cannot affect authority. | I-05, I-07 |
| `AS-07` | Retract and quarantine | Reads immediately exclude affected claims through both stores; subgraph quarantined; rebuild async; prior history inspectable. | I-01, I-06, I-07, I-16 |
| `AS-08` | Self-modify | Across two selected projects plus Kanbei: declarative lane uses versioned documents with next-turn activation; code lane stages a scoped live plugin/tool candidate, emits capability/dependency diffs, flips one dependency-scope pointer, health-checks, retains the previous candidate for rollback, and exercises the new tool in a scoped live run without a Nix generation; frozen root untouched. | I-13, I-14, I-19 |
| `AS-09` | Credentialed external call | Broker maps logical name to endpoint, injects per request, never forwards credentials across hosts or redirects, and redacts the response before canonical capture. | `ZT-01` |
| `AS-10` | Replay and undo | Declared projection classes rebuild to equal digests; presentation replay reproduces the timeline; no effect repeats; undo branches from a checkpoint preserving descendants. | I-09, I-10, I-17 |
| `AS-11` | Backup, restore, erasure | Every reference verifies; derived indexes rebuild; coverage honest; erasure epoch removes covered online and backup artifacts and records residuals. | I-08, I-15, I-16 |
| `AS-12` | Cost and freshness | Hard ceilings hold; soft overrides carry rationale; freshness reports coverage lag rather than implying current state. | I-12, I-16 |

Concurrency, merge, approval, and override tests (`C-*`):

| ID | Test | Pass criterion |
|---|---|---|
| `C-01` | Sequencer concurrency | One global sequence; no duplicates; committed prefix gap-free; every acknowledged append durable. |
| `C-02` | Sequence crash | No committed duplicate sequence; recovery resumes without a committed gap. |
| `C-03` | Duplicate delivery | Idempotent inbox; one logical effect; duplicates recorded as `skipped_duplicate`. |
| `C-04` | Concurrent merges | Both proposals append; projection applies non-destructively; source ids stable; alternatives visible. |
| `C-05` | Merge/read race | No torn view; coherent projection version; watermark advances atomically. |
| `C-06` | Approval batch | Open interrupts all addressed; partial resumes rejected; individual and batch human rulings permitted. |
| `C-07` | Duplicate ruling | Idempotent; one authority record; no duplicate effect. |
| `C-08` | Stale approval | Evidence, plan, or expiry change moves the ruling stale/expired; effect denied; `ZT-08`. |
| `C-09` | Evidence dedupe | Shared evidence deduped for display; different evidence sets never collapse; separate subjects and deadlines preserved. |
| `C-10` | Override receipt | Only with rationale and receipt; human can inspect; hard-policy override denied; limits computed from active records. |
| `C-11` | Tier self-grant | Denied; classification is a proposal; human-owned tier authoritative. |
| `C-12` | External clobber | External observation recorded; speculative branch preserved; conflict surfaced; no automatic acceptance. |
| `C-13` | Cross-agent undo | Descendants computed across the causal DAG; coordinated branch set; no silent global-cut assumption. |
| `C-14` | Concurrent effects | Hard policy and scoping hold; no lost receipt; ambiguous outcomes become `outcome_unknown`. |

Replay, undo, and crash tests (`R-*`):

| ID | Test | Pass criterion |
|---|---|---|
| `R-01` | Projection rebuild | Digest equality with the prior operational projection; coverage watermark identical. |
| `R-02` | Plugin-state rebuild | State digest equality; no canonical authority created; no effect re-executed. |
| `R-03` | Presentation replay | Same ordered view states, selections, and gestures; no live model call. |
| `R-04` | Effect tripwire | Every effect attempt blocked and recorded; replay continues without external call. |
| `R-05` | Undo branch | New branch; exact available LLM I/O preserved; descendants preserved; prior branch remains. |
| `R-06` | Repeated/cross-agent undo | Dependency-aware branch sets; no descendant deletion; no assumed global cut. |
| `R-07` | External compensation | New effect with its own attempt/receipt; not a replay and not a pointer-flip rollback. |

Poisoning, retraction, and quarantine tests (`P-*`):

| ID | Test | Pass criterion |
|---|---|---|
| `P-01` | Authority injection | Authority-shaped content recorded as observation/candidate; queue unchanged; `ZT-11`. |
| `P-02` | Scope escalation | Only the queried project materializes; scope checked at read time. |
| `P-03` | Poison localization | Affected subgraph suspect/quarantined; unrelated clusters active; immediate read exclusion. |
| `P-04` | Trust laundering | Derived item inherits weakest source trust; no auto-promotion without a linked human assertion. |
| `P-05` | Retraction | Synchronous suspect mark; async rebuild; prior claim and derivation inspectable; dependent approvals stale. |
| `P-06` | Split after merge | Source identities stable; reversal digest matches; no source text lost. |
| `P-07` | Derived-artifact invalidation | Rebuild removes poison from retrieval; no stale embedding or summary remains current. |
| `P-08` | Benign overgeneralization | Scope enforced at read; unsafe action blocked despite broad note text. |
| `P-09` | Rebuild race | Read during rebuild sees suspect/quarantined, never current. |
| `P-10` | Quarantine precision | Legitimate neighbors remain active; false-quarantine rate measured. |

CAS, backup, restore, and erasure tests (`B-*`):

| ID | Test | Pass criterion |
|---|---|---|
| `B-01` | Publish order | Bytes durable before manifest; manifest durable before ledger visibility. |
| `B-02` | Admission | Oversize, quota, or transaction-byte breach rejected at the ledger boundary with no partial record. |
| `B-03` | Dedupe | Same content digest stores one object; references remain distinct. |
| `B-04` | Orphan reclamation | Unreferenced content and manifests reclaimed only after grace when no `blob_ref` and no live pin remains; no live reference or pinned manifest removed. |
| `B-05` | GC during backup | GC paused; writes continue; extra blobs harmless. |
| `B-06` | Snapshot order | Online point-in-time snapshot taken immediately before filesystem backup. |
| `B-07` | Restore verification | Missing/truncated/tampered blob, manifest, or snapshot is a hard failure. |
| `B-08` | Derived rebuild | Restore rebuilds SQL/FTS/graph indexes and derived projections; coverage reported. |
| `B-09` | Checkpoint digest | Backup checkpoint digest matches the chain at the snapshot boundary. |
| `B-10` | Migration | No destructive down migration; rollback code only when data remains readable. |
| `B-11` | Erasure epoch | Covered ledger rows tombstoned, CAS and backups removed; prior backups cannot restore erased data without the epoch conflict. |
| `B-12` | Retention separation | Canonical retention and backup pruning remain separate policies. |

UI structural tests (`U-*`); comprehension thresholds are set from the `ui-v1` baseline under `S6`:

| ID | Test | Pass criterion |
|---|---|---|
| `U-01` | Provenance drilldown | Every displayed claim exposes source observation, span, derivation, and scope. |
| `U-02` | Contested state | Alternatives remain visible while a working claim is preferred. |
| `U-03` | Retraction distinction | Retracted, suspect, stale, and current states are distinct. |
| `U-04` | Decision queue | One global queue grouped by intent and deadline; shared evidence deduped; per-request evidence shown. |
| `U-05` | Approval comprehension | The frozen consent surface shows interpretation/plan hash, work-item ids, work-generation ids, completion subject, scope, granted tier, max effect class, constraints, evidence-set hash, `consentDigest`, the rendered labels, expiry, and reversibility for a ruling-independent manifest; the gesture endpoint recomputes `consentDigest` from the persisted `ConsentDisplayManifest` preimage and rejects a digest, manifest, ruling, ruling commitment, origin, or token mismatch. |
| `U-06` | Capability diff | Self-modification view shows added/removed capabilities and dependencies, not only code. |
| `U-07` | Coverage honesty | Every graph or timeline view exposes watermark and pending work. |
| `U-08` | Undo visibility | User sees the fork checkpoint and preserved descendants. |
| `U-09` | Renderer neutrality | Graph, timeline, and catalogs render from one structured view spec without canonical UI state. |
| `U-10` | Accessibility | Critical state is in the accessibility tree with a text fallback for graph-only information. |

### 7.4 Spikes and measurements

Validation spike protocols are `S1`–`S6` (distinct from the security invariants `S-1`–`S-15`). Every spike run cites one `S` id, one dataset id, and a pre-registered decision rule; an `inconclusive` result blocks the dependent decision and never becomes a default.

| ID | Protocol | Decision(s) | Dataset |
|---|---|---|---|
| `S1` | Paired language and TypeScript-runtime bake-off: ten equivalent tasks at 10 ms/100 ms/1 s host-call latency, frozen runtime/model/prompt/tool-catalog/sandbox, recorded success, tokens, latency, host-call behavior, snapshot trust, and operability. | `LANG-1`, `NODE-DENO-1` | `cost-v1`, `security-v1` |
| `S2` | Host-security enforcement matrix on the target host: user namespaces, same-UID ptrace/`/proc` access, Landlock, systemd directives, broker identity, cgroup controls, recovery, network/audit gaps, and secret reachability. | `DEP-1` | `security-v1` |
| `S3` | Inter-agent isolation and fault containment: crash, hang, OOM, fork bomb, orphan reaping, and resource ceilings; no claim of a security boundary. | `ISO-1` | `security-v1` |
| `S4` | Ceiling and quality measurement: model/broker/host-call cost and latency, reservation behavior, and provisional numeric ceilings and thresholds. | `LIM-1`, `D2` | `cost-v1` |
| `S5` | Reconciliation and poisoning measurement: invalidation precision, retraction propagation, split/merge safety, contested readability, quarantine precision, and compaction evidence/size frontier, with `memory-v1` for tuning and `memory-holdout-v1` for final metrics. | quality thresholds, `D7` | `memory-v1`, `memory-holdout-v1` |
| `S6` | Interface comprehension and interrupt grouping: contested/historical/stale-state comprehension, presentation replay, and multi-decision AG-UI grouping. | `UI-1`, `UI-2`, `ING-1` | `ui-v1` |

Additional measurements that do not select a deferred decision: broker identity, redirect handling, redaction coverage, revocation, and outage on `security-v1`; backup/restore on `journey-v1`; tool-runtime comparison within `S1`.

### 7.5 Acceptance journey

The vertical slice MUST demonstrate world-state control, memory reconciliation, and self-modification at comparable depth across two human-selected projects plus Kanbei itself. It MUST show:

1. ingest both selected projects and Kanbei, with per-source coverage;
2. explore intent with an immutable anchor and a versioned interpretation;
3. approve a structural goal through the frozen consent surface where required, including one stale-evidence ruling;
4. execute nested deltas with evidence-gated completion and a cross-project child delta;
5. reconcile at least one external conflicting commit in one project without clobbering speculative history in either;
6. add one agent-authored declarative perception/UI change and one scoped live plugin/tool candidate activation, each with a capability diff and rollback, without a Nix generation or kernel restart;
7. demonstrate memory contradiction, correction, retraction, progressive disclosure, and quarantine behavior;
8. demonstrate concurrent decision requests with approval, deferral, amendment, and stale-evidence handling;
9. replay presentation, rebuild projections, and execute an internal-state replay from checkpoint and recorded model/effect results without repeating external effects;
10. perform secretless credentialed access to one external service through the broker, then revoke the enrollment and show the next request denied.

### 7.6 Requirements-to-gate traceability

Every release gate maps to requirements and to this section's gates and checks. Implementation-task traceability is owned by `spec/08-implementation-plan.md` (`A0`–`I5`) and is not reproduced here; no `T` identifiers are defined. A phase exit that cites an identifier is decidable only through this table and the gate tables above.

| Requirement cluster | Gates | Scenarios / checks |
|---|---|---|
| `req. 15–28` ledger, authority, records | `ZT-02`, `ZT-05`, `ZT-09`, `ZT-11`, `ZT-14` | `I-01`–`I-07`, `I-18`, `AS-01`–`AS-03`, `V1`, `V14`, `V17`, `V22` |
| `req. 30–40` components, ordering, time | `ZT-09` | `I-02`, `I-03`, `V13`, `C-01`, `C-02` |
| `req. 42–52` memory, retraction, scope | `ZT-05`, `ZT-06`, `ZT-11` | `I-06`, `I-07`, `I-16`, `AS-06`, `AS-07`, `P-01`–`P-10`, `V15`, `V21` |
| `req. 54–64` intent, approval, overrides | `ZT-04`, `ZT-08`, `ZT-11`, `ZT-13` | `I-05`, `I-11`, `I-12`, `AS-02`–`AS-04`, `C-06`–`C-11`, `V5`, `V6`, `V8` |
| `req. 66–72` loops, budgets, durability | `ZT-04`, `ZT-12`, `ZT-14` | `I-09`, `I-10`, `AS-12`, `C-01`–`C-03`, `R-01`, `R-05`–`R-07`, `V2`–`V4`, `V11`, `V12`, `V18` |
| `req. 74–80` effects, model I/O, providers | `ZT-01`, `ZT-05`, `ZT-07` | `I-08`, `I-09`, `AS-09`, `R-02`–`R-04`, `V3`, `V10`, `V16`, `V19` |
| `req. 80–88` replay and undo | `ZT-07` | `I-10`, `I-17`, `AS-10`, `R-01`–`R-07`, `V9` |
| `req. 90–104` self-modification, plugins | `ZT-02`, `ZT-10`, `ZT-14` | `I-13`, `I-14`, `I-19`, `AS-08`, `U-06`, `V7`, `V22` |
| `req. 106–118` security, broker, credentials | `ZT-01`, `ZT-02`, `ZT-04`, `ZT-10`, `ZT-14` | `AS-09`, `B-01`, `B-02`, `V16`, `V20` |
| `req. 120–128` tools, runtimes, providers | `ZT-04`, `ZT-01` | `AS-09`, `C-14`, `S1` |
| `req. 130–138` interface and multimodal | `ZT-06`, `ZT-12`, `ZT-13` | `AS-10`, `P-07`, `U-01`–`U-10`, `S6` |
| `req. 140–154` persistence, backup, erasure | `ZT-09`, `ZT-12` | `I-08`, `I-15`, `AS-11`, `B-01`–`B-12` |
| `req. 156–158` validation and gates | all `ZT-*`, `AS-*` | `AS-01`–`AS-12`, `S1`–`S6`, complete `EvidenceEnvelope` bundle |

Phase exits: a gate is met only when every listed identifier has an `EvidenceEnvelope` with `outcome="pass"`, or a documented accepted residual recorded in `acceptedResiduals`. `inconclusive` blocks the dependent decision gate; `blocked` is a defect, not a pass.


---

## 8. Open questions

### 8.1 Deferred spike decisions

| ID | Decision | Provisional behavior required now |
|---|---|---|
| `LANG-1` | Agent-facing execution language among Monty, TypeScript, Lua, Bash, full CPython | `RuntimeId` is replaceable; OS sandbox is mandatory; bake-off protocol fixed |
| `NODE-DENO-1` | TypeScript runtime | TypeScript control plane fixed; Node/Deno both run under the OS sandbox; Deno permissions are inner only |
| `DEP-1` | Pure user-space versus dedicated-UID broker/sandbox helper | Security invariants and process contract apply; fail closed if enforcement is weaker than declared |
| `ISO-1` | Lightweight inter-agent isolation mechanism | Crash/resource containment only; container per agent excluded |
| `LIM-1` | Concrete global ceilings and per-loop limits | Schema and enforcement points fixed; values are operator configuration |
| `UI-1` | Exact A2UI specification commit and catalog version | Adapter declares emitted version; no silent downgrade |
| `UI-2` | Multi-decision AG-UI interrupt groups | One interrupt group per run; individual canonical rulings; resume complete for its group |
| `NET-1` | Non-TinyFish media transport and provider composition | Transport recorded in provenance; no capability equivalence assumed |
| `ING-1` | Unattended service-auth and local human-auth equivalent | Authority endpoints require verified Access identity; local access read-only by default |
| `BAK-1` | Exact backup hook wiring | Snapshot/CAS/GC ordering and restore verification are fixed |
| `D2` | `review_after` window expiry: promote-with-flag versus hold | Promote with `review_overdue` and a revocation path; spike confirms |
| `D3` | `outcome_unknown` probe cadence and provider capability matrix | Probe is read-only, manual or scheduled; automatic retry forbidden |
| `D4` | Checkpoint cadence and replay-cost target | Checkpoint after every wakeup and before every effect; numeric budget deferred |
| `D5` | Whether a standing chat-directive policy may ever cover `approval_required` scopes | Not permitted by this specification; confirmation belongs to autonomy-policy spike |
| `D7` | Retention and compaction of checkpoints, model I/O blobs, and replay artifacts | Ledger retained indefinitely by default; physical erasure exceptional and epoch-producing |
| `D8` | Automatic `loop_stalled` intervention policy | Escalate to decision queue; pause only on human ruling |

### 8.2 Human policy questions

- Which retention, deletion, and legal-erasure obligations apply beyond the default indefinite retention?
- May any local human-auth mechanism replace Cloudflare Access for authority endpoints on loopback?
- What evidence threshold would justify tightening the experimental live-plugin posture?
- Which external-effect classes are non-delegable even under `automatic` policy?
- What human attention and cost envelope bounds always-on operation?
- Which quality thresholds define acceptable reconciliation, task success, and UI usefulness?

### 8.3 Evidence gaps

These cannot be resolved by further public-source reading alone:

- outcome, retention, task-completion, and cost data for comparable agent harnesses;
- a matched, independent memory benchmark;
- precision and recall for LLM contradiction detection, entity resolution, and invalidation;
- calibrated per-claim confidence and retraction-propagation cost;
- a large-scale evidence/size frontier for compaction;
- a portable, signed, semantically versioned execution-snapshot format;
- per-model, per-language programmatic tool-use success rates;
- independent security audit of the chosen sandbox and broker composition;
- portable UI-state artifact standards and A2UI/AG-UI catalog parity;
- the long-term support and pricing of runtime and provider options.

---

## 9. Evidence appendix

### 9.1 Sources used

| Source | Role in this specification |
|---|---|
| `spec/00-requirements.md` | Normative requirements baseline. All conflicts are reconciled in its favor. Sections 5–11 define the product target, authority, records, memory, intent/approval, loops, replay, self-modification, security, tools/providers, interface, persistence, and validation; lines 156–168 define the deferred spikes. |
| `spec/01-state-memory.md` | Record envelope, component versioning, canonicalization, authority classes, SQLite schema, append transaction, sequence/hash chain, desired/observed components, and work links. Key ranges: invariants 7–21; envelope 29–52; versioning 56–71; canonical bytes 72–93; authority 144–160; schema and append 163–375; chain 379–402; desired/work 406–470. |
| `spec/02-agent-execution.md` | Canonical record kinds and execution invariants (9–35); intent and interpretation (41–99); chat inference (105–140); autonomy tiers (146–181); approvals, queue, coverage, overrides (187–300); work DAG and divergence (302–397); loops and budgets (401–488); messaging (492–557); effects (561–603); checkpoints and model I/O (609–656); undo (660–698); replay (702–713); recovery (717–742); verification (746–765); contradictions and deferred decisions (769–793). |
| `spec/03-plugin-runtime.md` | Kernel invariants (9–35); six contracts and composition (43–291); scopes and scoped state (297–414); candidate validation, diff, activation, health, rollback (420–553); live arbitrary plugins (556–574); context projection (578–615); Nix compatibility (619–650); tools (654–849); search contract (850–872); model routing (876–960); bake-off (964–1007). |
| `spec/04-security-deployment.md` | Security invariants (5–23); process identities (25–75); sandbox and writable roots (77–189); network residual (190–213); hard ceilings (215–287); broker (288–442); effect audit (443–506); deployment, ingress, watchdog, backup, host spike (507–605). |
| `spec/05-interface-ingestion.md` | Interface/ingestion invariants (3–18); contradiction resolutions (20–34); source model and adapters (36–112); observation and coverage schemas (114–188); event/full scans, checkpoints, failures (190–280); Git handling (281–328); provider-neutral search/fetch (329–400). |
| `spec/06-validation-plan.md` | Supporting harness detail only; §7 of this document is the sole normative validation authority. Verification contract and `EvidenceEnvelope` (§1), authority/conflict register (§2), invariants and zero-tolerance gates (§3), acceptance scenarios `AS-01`–`AS-12` (§4), reconciliation metrics (§5), poisoning `P-01`–`P-10` (§6), concurrency/approval `C-01`–`C-14` and `ApprovalBinding` (§7), replay/undo `R-01`–`R-07` and crash matrix (§8), CAS/backup `B-01`–`B-12` (§9), red team (§10), UI `U-01`–`U-10` (§11), cost/freshness (§12), spikes `S1`–`S6` (§13), tasks `T00`–`T20` (§14), and decision gates `DG-1`–`DG-8` (§15). Its identifier families are defined normatively in §7 of this document. |
| `research/22-research-synthesis.md` | Evidence landscape and gaps: state models (9–28, 57–69); blobs (71–81); memory (83–95); harness lessons (97–103); durability (105–114); self-modification (116–124); runtimes (126–132); interface (134–140); security (142–148); deployment (150–152); search (154–156); multimodal (158–160); contradicted assumptions (164–201); alternatives not followed (205–221); human questions (225–240); evidence gaps (244–262). |
| `brainstorm/07-recommendation.md` | Architectural comparison (13–19), truth-first recommendation and authority classes (21–40), rejected centers (42–44), change conditions (46–51), assumptions (55–60), vertical-slice scope (62–70), not-doing list (72–80), and open questions (82–91). |

### 9.2 Key evidence-to-design mappings

| Design decision | Supporting evidence |
|---|---|
| Truth-first ledger with narrow authority | Requirements 15–28; recommendation 23–32; synthesis 9–28. |
| Observations are not truth; human promotion is attestation | Requirements 17; record authority schema 144–160; semantic memory rules 44–50. |
| Open ECS-like components and unknown-component preservation | Requirements 32; envelope 29–54; version rules 56–70. |
| Global sequence plus causal DAG | Requirements 34; sequence/chain 379–402; append invariants 347–354. |
| Desired/observed as optional linked components | Requirements 36; desired/observed 406–451; divergence 369–397. |
| Derived, non-destructive semantic memory | Requirements 44–52; semantic projection schema and operations 1024–1105; synthesis 83–95. |
| Read-time scope and poisoning quarantine | Requirements 50; resolver 1106–1132; retraction/quarantine 1133–1152; synthesis 93–95. |
| Scoped approvals, global queue, constraint overrides | Requirements 60–64; approval and override contracts 864–1007; synthesis 225–240. |
| Native ledger durability and `outcome_unknown` | Requirements 72–78; loops/effects 1181–1272; recovery 4.7.9; synthesis 105–114. |
| Undo forest and replay separation | Requirements 82–88; undo/replay 4.7.7–4.7.8; synthesis 178–180. |
| Live arbitrary plugins outside frozen trust root | Requirements 90–104; kernel invariants and candidates 4.8; synthesis 116–124. |
| Unrestricted egress and explicit exfiltration residual | Requirements 108–118; security S-7 and §4.10.4; synthesis 142–148. |
| Any credential enrollment with least-privilege guidance | Requirements 118; broker enrollment 4.10.6; synthesis 142–148. |
| Programmable tool surface and runtime bake-off | Requirements 120–128; tools and bake-off 4.9; synthesis 126–132. |
| Stable web shell with AG-UI/A2UI/MCP Apps adapters | Requirements 130–138; interface contract 4.11.7; synthesis 134–140. |
| Local SQLite plus OCI-layout CAS and ordered backup | Requirements 140–154; persistence 4.3 and backup 4.3.4; synthesis 71–81. |
| Zero-tolerance gates and measured quality | Requirements 156–158; validation §7; synthesis 244–262. |

### 9.3 Evidence gaps and interpretation limits

- The research synthesis reports no matched, independent memory benchmark. Claims about reconciliation quality are therefore design requirements, not validated performance claims.
- No surveyed system provides calibrated confidence or reversible entity split; Kanbei's non-destructive design is a mitigation, not a proof of correctness.
- Headlong, Dots, Duet, and Blitzy do not publish comparable outcome, retention, task-completion, or cost data. Their mechanisms inform patterns, not benchmarks.
- The synthesis identifies no portable, signed execution-snapshot format and no head-to-head programmatic tool-use comparison across Monty, TypeScript, Lua, Bash, and full CPython. The bake-off exists to close that gap.
- Independent security audit of the chosen sandbox/broker composition is absent. The host security spike and fail-closed posture are the required mitigations.
- A2UI renderer maturity, AG-UI-to-A2UI mapping, json-render catalog parity, and MCP Apps host support remain uncertain; interface adapters must declare versions and fail closed on unsupported combinations.

### 9.4 Citation convention

Inline references use `file:line` or `file:line-range` within this repository. Requirements references are normative; research and brainstorm references are evidence for design choices, not authority. Where a draft conflicted with `spec/00-requirements.md`, the requirements baseline controls and the conflict is recorded in §4.1.3, §6, or §8.