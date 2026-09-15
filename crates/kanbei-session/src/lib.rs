//! kanbei-session — the M2 session actor: the serialized single-writer commit
//! path that orchestrates object installs + event frames through the shared
//! durability queue, with crash-injection fault points, plus the M2 module
//! subsystems: module activation (transactional config reload), effect
//! dispatch, module-state head updates, the retention gate, and the epoch
//! composition that pins into execution-snapshot manifests.
//!
//! Design inputs: docs/spikes/ratification-packet.md §3 (the actor ACKs after
//! write+enqueue; flush before consequential effects; object dirsync is
//! enqueued before the referencing frame's fsync) and §7 (inline ≤ 1 KB,
//! object ≥ 8 KB, middle band at kernel discretion — M1 inlines it);
//! docs/architecture.md R-08 (every canonical event references its pre-event
//! commit-snapshot digest; manifests materialize at event commit; genesis
//! uses the kernel bootstrap snapshot), R-10 (object installation precedes
//! event commit — crashes may orphan objects, never commit a dangling ref),
//! R-01/C-01 (activation is canonically recorded only when the session
//! observes it: mid-session reloads append one typed `composition_changed`
//! event with the epoch delta; startup activation is rebuilt from config and
//! on validation failure the kernel activates built-in safe mode — R-01/C-02)
//! and R-26/C-09 (staged sets publish atomically against the current epoch).
//!
//! M2 scope decision: `Session` is a SYNCHRONOUS single-writer struct, not a
//! spawned thread — the threaded actor with responder lanes ships at M2 with
//! outcomes. The only background threads here are the shared durability
//! queue's fsync worker and the wasm watchdog.
//!
//! M2 subsystem wiring:
//! - The shared `Arc<Mutex<ServiceRegistry>>` is owned here (the session IS
//!   the kernel): the `ModuleManager`'s host publishes into it during
//!   `kb_on_activate` (host op 6), the `ContributionRegistry` validates and
//!   applies against it, and the manager's `StateStore` currency callback is
//!   re-bound by `ModuleManager::new` to the manager's own token table (the
//!   session's placeholder closure is replaced — see `ModuleManager::new`).
//! - `activate_config` is the atomic config reload: activate → collect the
//!   registry delta → STAGE it (remove the delta from the shared registry so
//!   validate/apply run against the pre-activation state) → validate →
//!   OCC-publish → commit the canonical `composition_changed` event. Any
//!   failure deactivates the module; the last valid composition is retained.
//! - Effect dispatch (R-16/D-11) checks generation currency and routes
//!   through the host's `service_call` machinery (host op 3 — dependency
//!   version enforcement + provider `kb_hot`). Broker-gated dispatch-time
//!   re-verification is exercised in the testkit via host op 4; the session
//!   does not own the broker (the `ModuleHost` does).
//!
//! `SessionError` embeds `ModuleError`/`ScopeError`/`ServiceError` whose
//! variants carry unboxed `ServiceProvider`/`ServiceKey` values (a fixed
//! public contract, mirroring kanbei-services' own
//! `#![allow(clippy::result_large_err)]`).
#![allow(clippy::result_large_err)]

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use kanbei_core::digest::Digest;
use kanbei_core::envelope::{Envelope, EnvelopeError};
use kanbei_core::id::{BranchId, Id128};
use kanbei_core::queue::DurabilityQueue;
use kanbei_log::{AppendLog, Profile};
use kanbei_modules::{HeadFile, ModuleError, ModuleManager, PackageManifest};
use kanbei_objects::{ObjectError, ObjectStore};
use kanbei_policy::builtins::StoreAllPolicy;
use kanbei_policy::{PolicyPlugin, RetentionGate};
use kanbei_scopes::epoch::{Composition, CompositionStore};
use kanbei_scopes::registry::ContributionRegistry;
use kanbei_scopes::scope_tree::ScopeTree;
use kanbei_scopes::contrib::SettingsContribution;
use kanbei_services::ServiceRegistry;
use kanbei_snapshot::ExecutionManifest;
pub use kanbei_transcript::{CollapseOverrides, TranscriptProjection, TranscriptView};
use kanbei_vm::{GuestError, Vm};
use serde_json::json;
use thiserror::Error;

#[cfg(feature = "otel")]
use kanbei_telemetry::{SpanBuilder, Telemetry};

mod ui;
#[cfg(feature = "otel")]
mod telemetry;
mod branch;
mod builtin_config;
mod commit;
mod discovery;
mod elements;
mod settings_gate;
mod recovery;
mod switch;
mod transcript;
use recovery::{decode_record, recover_or_fresh, shutdown_queue};
pub use builtin_config::{
    BUILTIN_CONFIG_SOURCE, builtin_config_manifest, builtin_config_module_id, root_scope,
};
pub use discovery::{DiscoveryError, discover_config_layers};
pub use ui::{UiHost, UiIntent, UiOutcome, UI_INTENT_RESOURCE};

/// The bounded recent-event ring size: the trajectory render covers the
/// full canonical history, but the CONTENT is these most-recent events.
const RECENT_RING: usize = 64;

/// Checkpoint label length cap (M6): beyond this the label is rejected.
const CHECKPOINT_LABEL_MAX: usize = 200;

// ---------- config ----------

/// Envelope observer (UI seam): called after every commit with each resolved
/// envelope. Runs on the committing thread; the observer must not block the
/// commit path.
pub type CommitListener = Arc<dyn Fn(&Envelope) + Send + Sync>;

/// Streaming delta observer (UI seam): called with each content fragment as
/// the provider streams a model response. Runs on the calling thread; the
/// observer must not block. None = deltas are consumed (cancellation still
/// works) but not observed.
pub type DeltaListener = Arc<dyn Fn(&str) + Send + Sync>;

/// Transcript-view observer (UI seam): called with the session's current
/// projection view whenever the transcript changes (a commit applied, a turn
/// finalized or replayed, a provider stream ended). Runs on the calling
/// thread; the observer must not block. Gives a cross-thread UI live access to
/// the session-owned projection without holding the session. None = not
/// observed (read on demand with [`Session::transcript_view`]).
pub type TranscriptListener = Arc<dyn Fn(&TranscriptView) + Send + Sync>;

/// Desired-state settings seam (decision 28): resolves the running session's
/// wiring from the merged config-layer [`SettingsContribution`].
///
/// The kernel does not interpret settings itself — it hands the merged overlay
/// to the host's source, which owns the mapping from config fields to concrete
/// engines/brokers/resolvers. This keeps secret material and engine
/// construction out of the canonical layer while letting config drive the
/// session.
pub trait SettingsSource: Send + Sync {
    fn resolve(&self, settings: &kanbei_scopes::contrib::SettingsContribution) -> SessionSettings;
}

/// The runtime wiring a [`SettingsSource`] resolves from config settings.
///
/// When a source is configured, it FULLY determines the wiring (F10): the
/// session assigns `provider_engine`/`provider`/`broker`/`approval_resolver`
/// from this struct on every apply, `Some` or `None`, so a higher layer that
/// clears a field actually uninstalls the earlier wiring. `session_id` is the
/// exception — only overwritten when `Some` (the open-time identity pin; see
/// `Session::apply_settings`).
#[derive(Default)]
pub struct SessionSettings {
    /// Provider engine; `None` = storage-only (no model calls).
    pub provider_engine: Option<Box<dyn kanbei_provider::ProviderEngine>>,
    /// Provider config (also the manifest's `provider_config` pin).
    pub provider: Option<kanbei_provider::ProviderConfig>,
    /// Capability broker; `None` = the default (empty, default-deny) broker.
    pub broker: Option<kanbei_capabilities::Broker>,
    /// Approval resolver; `None` = park every gated intent.
    pub approval_resolver: Option<ApprovalResolver>,
    /// Session identity override; `None` = keep the current identity.
    pub session_id: Option<Id128>,
}

/// Session configuration. `dir` is the session layout root: `<dir>/log.zst`
/// (append log), `<dir>/objects/` (object store), and `<dir>/state/` (module
/// state heads).
pub struct SessionConfig {
    pub dir: PathBuf,
    pub stream: String,
    pub profile: Profile,
    /// Serialized payloads larger than this are promoted to objects (§7).
    pub inline_max: usize,
    /// Payloads at/above this size may be promoted at kernel discretion by
    /// media type (§7); M1 inlines the 1–8 KB middle band, so the field is
    /// currently unused.
    pub object_min: usize,
    pub fault: Option<Arc<dyn FaultInjector>>,
    /// Root config layers to activate at open (R-01/C-02), ordered LOW→HIGH
    /// precedence (built-in defaults, then user, then project). Empty = no
    /// config generation. A failed non-builtin layer drops every non-builtin
    /// layer and keeps the built-in generation active (safe mode, R-01/C-02).
    pub config_layers: Vec<PackageManifest>,
    /// A config-discovery read failure the caller degraded to built-in-only
    /// layers (F14): `open` records it as a canonical `safe_mode_activated`
    /// fact. None = discovery succeeded (or was not attempted). The layers the
    /// caller passes are still the ones activated; this field only carries the
    /// reason for the canonical trace.
    pub config_discovery_error: Option<String>,
    /// Desired-state settings factory (decision 28); None = no settings seam
    /// (today's argv/env-derived behavior). Resolved AFTER the config layers
    /// activate and BEFORE the composition commit, so the settings-resolved
    /// `provider` is pinned in that event's post-manifest.
    pub settings: Option<Arc<dyn SettingsSource>>,
    /// Module state-head size ceiling (R-07); default 1 MB.
    pub max_state_bytes: usize,
    /// Retention policy plugin; default [`StoreAllPolicy`].
    pub policy: Arc<dyn PolicyPlugin>,
    /// Wasm engine config; None = [`kanbei_vm::VmConfig::default`]. When the
    /// guest wasm is not built (`Vm::load` → `NotBuilt`), modules are
    /// disabled (a config layer then opens in safe mode with no modules).
    pub engine: Option<kanbei_vm::VmConfig>,
    // --- M3 agent spine ---
    /// Provider gateway config; None = no model calls (storage-only session).
    pub provider: Option<kanbei_provider::ProviderConfig>,
    /// The provider engine; None = build the wire-protocol engine from
    /// `provider` via [`kanbei_provider::engine_for`] (driven by `protocol`;
    /// tests inject the fake engine).
    pub provider_engine: Option<Box<dyn kanbei_provider::ProviderEngine>>,
    /// Provider wire protocol (M9 wave 3): OpenAI-compatible Chat
    /// Completions by default (`HttpEngine`); `Anthropic` selects the
    /// Messages API engine.
    pub protocol: kanbei_provider::WireProtocol,
    /// Scheduler budgets (deadline/tokens/tools/children).
    pub budgets: kanbei_scheduler::Budgets,
    /// Kernel breaker floors (R-17/E-02).
    pub breaker_floors: kanbei_scheduler::BreakerFloors,
    /// Native tool execution limits.
    pub tool_limits: kanbei_tools::ExecLimits,
    /// Tool execution root (fs tools never escape it).
    pub fs_root: PathBuf,
    /// Capability broker (grants/templates); default = empty (default-deny).
    pub broker: kanbei_capabilities::Broker,
    /// Approval queue bound with eviction (R-17/H-05); 0 = no approvals.
    pub approval_bound: usize,
    /// Driver-side approval resolver: when the cognition loop parks an
    /// approval-gated intent, this seam decides it on the driver's behalf
    /// (an unattended battery plays the user; production wires the UI's
    /// approval queue). None = park until `resolve_approval`.
    pub approval_resolver: Option<ApprovalResolver>,
    /// The session's own identity (caller principal for kernel-originated
    /// tool calls, R-14); None = generate at open.
    pub session_id: Option<Id128>,
    // --- UI/driver observer seams ---
    /// Envelope observer (UI seam): called after every commit with each
    /// resolved envelope (a promoted `$object` marker dereferenced to the
    /// full payload). Runs on the committing thread; the observer must not
    /// block the commit path. None = no observer.
    pub commit_listener: Option<CommitListener>,
    /// External cancel flag (UI seam): when set, the cognition loop ends the
    /// active run at the next model-call stream boundary (or step boundary)
    /// with `Failed(UserCancelled)`. The flag is one-shot — the session
    /// clears it once the cancel is consumed, so it never cancels a later
    /// turn. Shared with the caller so the UI can set it without session
    /// access. None = no external cancel.
    pub cancel_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Streaming delta observer (UI seam): called per content fragment as the
    /// provider streams a model response. None = deltas are not observed.
    pub delta_listener: Option<DeltaListener>,
    /// The transcript projection service (decision 30). None = the built-in
    /// [`kanbei_transcript::ConversationProjection`]. The session owns and
    /// drives it: committed envelopes, provider-stream deltas, turn finalize,
    /// and replay all flow through this seam, so a replacement projection
    /// needs no changes to the session or the UI.
    pub transcript: Option<Box<dyn TranscriptProjection>>,
    /// Transcript-view observer (UI seam); called when the projection changes.
    /// None = read on demand with [`Session::transcript_view`].
    pub transcript_listener: Option<TranscriptListener>,
    // --- M4 memory substrate + context projection ---
    /// Memory substrate root (canonical XDG state). None = cfg.dir.join("memory").
    pub memory_root: Option<PathBuf>,
    /// ProjectId (pro_ brand) binding; None = no project memory scope.
    pub project: Option<Id128>,
    /// Kernel fault injector for the memory actors (transition/head points).
    pub memory_fault: Option<Arc<dyn kanbei_memory::MemoryFaultInjector>>,
    /// Factory producing a fresh CognitionProvider per spawned child run
    /// (R-09 child runs; None = child.spawn resolves to an error outcome).
    pub child_provider: Option<Box<dyn FnMut() -> Box<dyn kanbei_scheduler::CognitionProvider> + Send>>,
    // --- M8 wave 1 telemetry (optional; feature `otel`) ---
    /// Optional OTel-compatible telemetry handle (M8 wave 1); None = no
    /// telemetry. The `otel` feature only.
    #[cfg(feature = "otel")]
    pub telemetry: Option<Telemetry>,
    // --- M8 wave 2 GC (R-20) ---
    /// Automatic GC at open: None = no automatic pass (the explicit
    /// [`Session::run_gc`] stays available). When set, open runs the
    /// quarantine pass (always) and the sweep (only when `sweep` is true),
    /// best-effort — a GC failure never fails open.
    pub gc: Option<kanbei_gc::GcConfig>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("."),
            stream: "default".into(),
            // Balanced is the design default (architecture.md:406); Fast is
            // the caller's opt-in.
            profile: Profile::Balanced,
            inline_max: 1024,
            object_min: 8192,
            fault: None,
            config_layers: Vec::new(),
            config_discovery_error: None,
            settings: None,
            max_state_bytes: 1024 * 1024,
            policy: Arc::new(StoreAllPolicy),
            engine: None,
            provider: None,
            provider_engine: None,
            protocol: kanbei_provider::WireProtocol::OpenAI,
            budgets: kanbei_scheduler::Budgets::default(),
            breaker_floors: kanbei_scheduler::BreakerFloors::default(),
            tool_limits: kanbei_tools::ExecLimits::default(),
            fs_root: PathBuf::from("."),
            broker: kanbei_capabilities::Broker::new(),
            approval_bound: 64,
            approval_resolver: None,
            session_id: None,
            commit_listener: None,
            cancel_flag: None,
            delta_listener: None,
            transcript: None,
            transcript_listener: None,
            memory_root: None,
            project: None,
            memory_fault: None,
            child_provider: None,
            #[cfg(feature = "otel")]
            telemetry: None,
            gc: None,
        }
    }
}

// ---------- kernel types (tier-1 re-exports) ----------

pub use kanbei_kernel::commit::{CommitError, PostManifest};
pub use kanbei_kernel::event::{CommitReceipt, NewEvent};
pub use kanbei_kernel::fault::{FaultInjector, FaultPoint};

impl From<CommitError> for SessionError {
    fn from(e: CommitError) -> Self {
        match e {
            CommitError::InvalidInput(msg) => SessionError::InvalidInput(msg),
            CommitError::MissingObject { digest } => SessionError::MissingObject { digest },
            CommitError::Io(e) => SessionError::Io(e),
            CommitError::Object(e) => SessionError::Object(e),
        }
    }
}

/// The outcome of an atomic config activation (R-01/C-01): the module's
/// generation and the composition epoch the `composition_changed` event
/// records.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigActivation {
    pub module_id: Id128,
    pub generation: u64,
    pub epoch: u64,
    pub event_seq: u64,
}

/// The materialized projection of the last [`Session::project_context`]
/// call: the validated fragment-list digest, the lowering's cache plan, the
/// pinned memory roots, and the lowered provider messages (the model-call
/// request source).
#[derive(Debug, Clone, PartialEq)]
pub struct ProjectionState {
    pub projection_digest: Digest,
    pub cache_plan: kanbei_provider::CachePlan,
    /// [lifetime, project] flattened, lifetime first; empty when unpinned.
    pub memory_roots: Vec<Digest>,
    pub lowered: Vec<kanbei_provider::Message>,
}

/// One committed compaction selection (R-18/E-06): the covered event range,
/// the summary object digest, and the fragment ids folded into it. New
/// events whose payload carries one of the covered fragments are rejected by
/// the commit FSM (E-06).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CompactedRange {
    pub range: (u64, u64),
    pub summary_digest: Digest,
    pub covered_fragments: Vec<String>,
}

// ---------- M6 historical correction (branching) ----------

/// One committed checkpoint (M6): session + event seq identify the
/// `checkpoint_created` event the new branch continues from.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CheckpointRef {
    pub session_id: Id128,
    pub seq: u64,
}

// ---------- M9 wave 5a independent-session fork ----------

/// Options for [`Session::fork`]: where the forked session lives, its
/// identity and retention policy, and the configuration lane the fork does
/// not derive.
///
/// `config` carries everything [`SessionConfig`] needs that fork does not
/// derive from the checkpoint: stream/profile, budgets, engine, provider,
/// fs_root, tool limits, approval bound, fault injectors, GC, telemetry.
/// Fork overrides: `dir` (= `target_dir`), `session_id` (= the fresh id),
/// `policy` (= `policy`), `broker` (the fork-floor broker), `memory_root`
/// (None — the seeded memory lives at `<target_dir>/memory`), `config` (the
/// package manifest resolved from the checkpoint's config choice), and
/// `project` (the source's project id when the source has one).
///
/// `target_dir` must be absent or empty — fork refuses to seed into an
/// existing session dir, and on failure best-effort removes everything it
/// created there.
pub struct ForkOptions {
    /// Root dir of the forked session (`<target>/log.zst`, `<target>/objects/`,
    /// `<target>/memory/`, ...). Must not already hold a session.
    pub target_dir: PathBuf,
    /// The forked session's identity; a fresh one when None.
    pub session_id: Option<Id128>,
    /// Retention policy for the forked session; default [`StoreAllPolicy`].
    pub policy: Arc<dyn PolicyPlugin>,
    /// The remaining session configuration lane (overridden fields above).
    pub config: SessionConfig,
}

/// The outcome of [`Session::fork`]: the forked session plus the fact
/// coordinates (new identity, source checkpoint, branch, follow policy).
pub struct ForkReceipt {
    /// The forked session: opened, config-activated (when the checkpoint
    /// chose one), memory-seeded at the checkpoint roots, and carrying the
    /// canonical `forked` fact as its genesis record.
    pub session: Session,
    /// The new session's identity (never equal to the source's).
    pub session_id: Id128,
    /// The source checkpoint this fork derives from.
    pub checkpoint_seq: u64,
    /// The forked session's branch: a fresh root branch — the fork has no
    /// branch history, the `forked` fact is its genesis record.
    pub branch: BranchId,
    /// The memory follow policy recorded in the `forked` fact.
    pub follow: kanbei_memory::MemoryFollowPolicy,
}

/// The outcome of [`Session::adopt`]: the fork identity, the adopted head
/// seq, and the follow policy the `fork_adopted` fact records. The canonical
/// record is the `fork_adopted` event on the source log; the receipt is the
/// minimal in-memory mirror.
pub struct AdoptReceipt {
    /// The adopted fork session's identity.
    pub fork_session: Id128,
    /// The fork's head seq — its last committed event at adoption time.
    pub fork_seq: u64,
    /// The memory follow policy recorded in the `fork_adopted` fact.
    pub follow: kanbei_memory::MemoryFollowPolicy,
}

/// One intent event quiesced by a branch transition (M6): its seq, kind, and
/// event id (`evt`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QuiescedIntent {
    pub seq: u64,
    pub kind: String,
    pub id: String,
}

/// The intents a branch transition abandoned (M6): pending intents
/// (committed without an outcome) are cancelled; classified interrupted/
/// ambiguous intents in the abandoned tail are ambiguous.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QuiesceRecord {
    pub cancelled: Vec<QuiescedIntent>,
    pub ambiguous: Vec<QuiescedIntent>,
}

/// One committed branch (M6): its frontier and its `branch_transition` event.
/// `follow` is the memory-follow policy the transition recorded and
/// `config_choice` the config choice at the branch point (wave 2).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct BranchRecord {
    pub id: BranchId,
    pub from: Option<BranchId>,
    /// The checkpoint seq this branch continues from (== its frontier).
    pub frontier_seq: u64,
    /// The seq of the `branch_transition` event itself (== next_seq of the
    /// new branch's first event).
    pub transition_seq: u64,
    pub follow: kanbei_memory::MemoryFollowPolicy,
    pub config_choice: ConfigChoiceRecord,
    pub quiesce: QuiesceRecord,
}

/// The config choice a `branch_transition` recorded (M6 wave 2): which
/// config was live at the branch point (`current`), which config the
/// checkpoint manifest pinned (`historical` — its `provider_config` digest),
/// and the live epoch composition. Module-state/config restoration is out of
/// scope (architecture.md §M6) — the record is the deliverable.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConfigChoiceRecord {
    pub mode: String,
    pub current: Option<Digest>,
    pub historical: Option<Digest>,
    pub composition: Option<Digest>,
    /// The ORDERED (LOW→HIGH) config-layer package digests live at the branch
    /// point (F5). `current` stays the TOP layer for existing consumers; the
    /// full stack is the restore source of truth. Additive with `serde(default)`
    /// so wave-1/older records decode.
    #[serde(default)]
    pub layers: Vec<Digest>,
}

/// The wire shape of a `branch_transition` payload (as written by
/// `branch.rs`). Only the load-bearing fields D-F-Q names — branch identity,
/// `follow`, `config_choice`, `quiesce`, plus `from_branch`/`frontier_seq` —
/// are typed here; the rest (`checkpoint_event`, `memory_root`, …) are
/// ignored. Decoding through serde makes "malformed → `CorruptRecord`" fall
/// out of the type, and `Option` on the wave-1-nullable fields keeps their
/// documented defaults when absent.
#[derive(serde::Deserialize)]
struct BranchTransitionWire {
    branch: BranchId,
    from_branch: Option<BranchId>,
    frontier_seq: u64,
    follow: Option<kanbei_memory::MemoryFollowPolicy>,
    config_choice: Option<ConfigChoiceRecord>,
    quiesce: Option<QuiesceRecord>,
}

/// The memory roots pinned by the checkpoint a branch continues from (M6;
/// wave 2 consumes them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedRoots {
    pub lifetime: Digest,
    pub project: Option<Digest>,
}

/// Everything a checkpoint-validation pass (M6 `continue_from`, M9 wave 5a
/// `fork`) establishes about a committed `checkpoint_created` event: the
/// event envelope, the snapshot manifest, the pinned memory roots, and the
/// memory follow policy derived from them.
struct CheckpointFacts {
    env: Envelope,
    snapshot: Digest,
    manifest: ExecutionManifest,
    memory_root: Option<Digest>,
    project_memory_root: Option<Digest>,
    follow: kanbei_memory::MemoryFollowPolicy,
}

/// The M6 wave 4 bundle-export report: what an [`Session::export_bundle`]
/// produced. `missing` lists every referenced manifest or closure object that
/// was unreadable/absent — `verified` is exactly `missing.is_empty()`. The
/// report is written to `closure.json` even when partial (R-06 honest
/// availability).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExportReport {
    pub frames: u64,
    pub envelopes: u64,
    pub manifests: usize,
    pub objects: usize,
    pub missing: Vec<Digest>,
    /// Kernel-embedded build-time identity pins (engine/toolchain digests) —
    /// never store objects, recorded so a verifier knows the closure is
    /// complete without them.
    pub identity_pins: Vec<Digest>,
    pub verified: bool,
}

/// One committed intent-kind event awaiting its outcome-kind event (B-05/M6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingIntent {
    pub seq: u64,
    /// The intent event's id (`evt`).
    pub id: String,
    pub kind: String,
    /// `tool_intent` payloads pair by call_id; None for other kinds.
    pub call_id: Option<String>,
    /// The tool name for `tool_intent`; None for other kinds.
    pub tool: Option<String>,
    /// The intent event's pre-event snapshot (the origin world, B-05).
    pub origin_snapshot: Option<Digest>,
}

// ---------- session ----------

/// Driver-side approval resolver: decides the newest parked approval-gated
/// intent during the cognition loop (`true` = approve, `false` = leave
/// parked for `Session::resolve_approval`). The parked intent carries the
/// committed tool, arguments, and bindings the driver presents to the user
/// (R-16/D-12). Unattended batteries wire an auto-approve stand-in;
/// production wires the interactive approval queue.
pub type ApprovalResolver =
    std::sync::Arc<dyn Fn(&kanbei_tools::ApprovalParked) -> bool + Send + Sync>;

/// One activated config layer in LOW→HIGH precedence order (decision 28, F5):
/// its precedence rank, module identity, live generation, package digest, and
/// manifest. The ordered vector of these is the config-identity source of
/// truth for restore — `config_digest`/`config_manifest` are only its top.
#[derive(Debug, Clone)]
struct ConfigLayer {
    rank: u8,
    module_id: Id128,
    generation: u64,
    package: Digest,
    manifest: PackageManifest,
}

pub struct Session {
    log: AppendLog,
    store: ObjectStore,
    queue: Arc<DurabilityQueue>,
    next_seq: u64,
    current_snapshot: Option<Digest>,
    log_path: PathBuf,
    cfg: SessionConfig,
    // --- M2 subsystems ---
    /// The kernel-owned shared service registry: the module host publishes
    /// into it, the contribution registry validates/applies against it.
    services: Arc<Mutex<ServiceRegistry>>,
    scopes: ScopeTree,
    composition: CompositionStore,
    registry: ContributionRegistry,
    policy: RetentionGate,
    modules: Option<ModuleManager>,
    vm_engine_digest: Option<Digest>,
    // --- M5 semantic workbench ---
    /// The bound UI host (None until the built-in UI is activated).
    ui_host: Option<UiHost>,
    /// T9 lifecycle hook bindings (rebuilt on composition change).
    hooks: crate::hooks::HookSet,
    /// Modules that already consumed a hook-fault respawn since the last
    /// composition rebind (G backoff: at most one respawn per decision epoch).
    hook_respawned: std::collections::HashSet<Id128>,
    // --- M3 agent spine ---
    scheduler: kanbei_scheduler::Scheduler,
    provider: Option<Box<dyn kanbei_provider::ProviderEngine>>,
    provider_config: Option<kanbei_provider::ProviderConfig>,
    tool_registry: kanbei_tools::ToolRegistry,
    native_tools: kanbei_tools::NativeTools,
    broker: kanbei_capabilities::Broker,
    /// Bounded pending-approval queue (oldest evicted on overflow).
    approvals: std::collections::VecDeque<kanbei_tools::ApprovalParked>,
    approval_bound: usize,
    approval_resolver: Option<ApprovalResolver>,
    /// Envelope observer (UI seam); called per commit with resolved payloads.
    commit_listener: Option<CommitListener>,
    /// External cancel flag (UI seam); checked at each cognition step.
    cancel_flag: Option<Arc<std::sync::atomic::AtomicBool>>,
    /// Streaming delta observer (UI seam); called per streamed content fragment.
    delta_listener: Option<DeltaListener>,
    /// The session-owned transcript projection service (decision 30): applied
    /// on the commit path, driven by provider-stream deltas, and read through
    /// [`Session::transcript_view`].
    transcript: Box<dyn TranscriptProjection>,
    /// Transcript-view observer (UI seam); called when the projection changes.
    transcript_listener: Option<TranscriptListener>,
    /// Session-local manual collapse overrides (decision 9): ephemeral
    /// presentation, never part of the projection, applied when the transcript
    /// view is built for the render context. A resumed session recreates the
    /// view identically because the overrides are never persisted.
    transcript_overrides: CollapseOverrides,
    fs_root: PathBuf,
    session_id: Id128,
    // --- M4 memory substrate + context projection ---
    /// The lifetime-scope memory actor (always present; R-11).
    memory_lifetime: kanbei_memory::MemoryRootActor,
    /// The project-scope memory actor; None when no project is bound.
    memory_project: Option<kanbei_memory::MemoryRootActor>,
    /// Disposable per-session projection index over both scope folds.
    memory_index: kanbei_retrieval::MemoryIndex,
    /// The bound project's registry entry (None when unbounded).
    project_entry: Option<kanbei_memory::ProjectEntry>,
    /// The last materialized projection (M4 staged pipeline).
    projection_state: Option<ProjectionState>,
    /// Provider identity of the last model call (R-18/E-07 continuity).
    last_provider: Option<String>,
    /// (provider identity, opaque artifacts base64) of the last call that
    /// emitted artifacts (R-18/E-07 same-provider replay; kept even when an
    /// intervening call emitted none, still paired with its provider).
    last_opaque: Option<(String, String)>,
    /// (sent stable-prefix digest, memory roots) of the last model call.
    last_cache: Option<(Option<Digest>, Vec<Digest>)>,
    /// Bounded recent-event ring (seq, kind, payload) — the trajectory
    /// render source, capped at [`RECENT_RING`] entries.
    recent_events: std::collections::VecDeque<(u64, String, serde_json::Value)>,
    /// Covered compaction ranges (R-18/E-06) recovered from the log.
    compacted: Vec<CompactedRange>,
    // --- M6 historical correction ---
    /// The current branch; the root branch on a fresh session.
    branch: BranchId,
    /// Committed branch records, chronological (rebuilt from the log on
    /// open — the log is the authority for branch identity).
    branch_records: Vec<BranchRecord>,
    /// The live config manifest's package digest (== the canonical content
    /// digest `install_package` computes); None when no config activated
    /// (storage-only sessions). The config-choice record's `current` field.
    config_digest: Option<Digest>,
    /// The live config manifest, retained for `module reset-state`'s
    /// state-key binding (R-07/C-F1); None when no config activated.
    config_manifest: Option<PackageManifest>,
    /// The merged settings snapshot captured after open activated the config
    /// layers (decision 28). Stored, not resolved live, so it survives
    /// generation teardown (safe mode reflects the built-in layer).
    host_settings: SettingsContribution,
    /// The active config layers in LOW→HIGH precedence order (decision 28,
    /// F5). A higher-rank publish consults these to compute the
    /// lower-precedence contributions it implicitly replaces, and the ordered
    /// package digests are the restore source of truth for fork/continue.
    config_layers: Vec<ConfigLayer>,
    /// Whether this open already committed a canonical `safe_mode_activated`
    /// fact (F). Used to avoid double-committing when a discovery degradation
    /// accompanies an activation failure.
    safe_mode_committed: bool,
    /// The memory roots pinned by the checkpoint this branch continues from
    /// (wave 2 consumes them).
    pinned_roots: Option<PinnedRoots>,
    /// Child-run provider factory (R-09); None = child.spawn errors.
    child_provider: Option<Box<dyn FnMut() -> Box<dyn kanbei_scheduler::CognitionProvider> + Send>>,
    // --- M8 wave 1 telemetry (optional; feature `otel`) ---
    /// The optional OTel-compatible exporter handle (M8 wave 1).
    #[cfg(feature = "otel")]
    telemetry: Option<Telemetry>,
    /// The open run span, closed at run outcome with the terminal status
    /// + usage attrs; its id parents every commit span while active.
    #[cfg(feature = "otel")]
    open_run_span: Option<SpanBuilder>,
    // --- M8 wave 2 GC (R-20): writer pins ---
    /// Digests with an install in flight (or an external writer's in-flight
    /// reference): GC never quarantines or sweeps them.
    gc_pins: std::sync::Mutex<std::collections::HashSet<Digest>>,
}

impl Session {
    /// Opens `<dir>/log.zst` + `<dir>/objects/` + `<dir>/state/`. Runs
    /// [`kanbei_log::recover`] first — REQUIRED before open so a torn tail is
    /// truncated before the writer resumes. A fresh log pins the kernel
    /// bootstrap snapshot as the genesis manifest (R-08); a resumed log does
    /// NOT re-pin — M1 sessions resume without manifest state (current_snapshot
    /// is None; the audit reconstruction is the authority, not the resumed
    /// session).
    ///
    /// After the M1 flow the M2 subsystems are built: the shared service
    /// registry, the scope tree, the contribution registry, the composition
    /// store, the retention gate, and (when the guest wasm loads) the module
    /// manager with its own object-store handle over `<dir>/objects` and the
    /// state store over `<dir>/state`. The `cfg.config_layers` generations are
    /// then activated atomically LOW→HIGH; a failing non-builtin layer drops
    /// the non-builtin generations, keeps the built-in one active, and commits
    /// a canonical `safe_mode_activated` event — the session remains usable
    /// (R-01/C-02, decision 28).
    pub fn open(mut cfg: SessionConfig) -> Result<Self, SessionError> {
        std::fs::create_dir_all(&cfg.dir)?;
        let log_path = cfg.dir.join("log.zst");
        let recovered = recover_or_fresh(&log_path)?;
        let queue = Arc::new(DurabilityQueue::start(&format!(
            "kb-session-{}",
            cfg.stream
        )));
        let log = match AppendLog::open(&log_path, &cfg.stream, Arc::clone(&queue)) {
            Ok(log) => log,
            Err(e) => {
                shutdown_queue(queue);
                return Err(e.into());
            }
        };
        let mut store = match ObjectStore::open(&cfg.dir.join("objects"), Arc::clone(&queue)) {
            Ok(store) => store,
            Err(e) => {
                drop(log);
                shutdown_queue(queue);
                return Err(e.into());
            }
        };
        let next_seq = if recovered.events == 0 {
            1
        } else {
            recovered.last_seq + 1
        };
        // genesis: pin the kernel bootstrap snapshot as the pre-event
        // snapshot for the first commit (R-08)
        let current_snapshot = if recovered.events == 0 {
            let manifest = kanbei_snapshot::ExecutionManifest::bootstrap();
            match kanbei_snapshot::pin(&mut store, &manifest) {
                Ok((genesis, _deduped)) => Some(genesis),
                Err(e) => {
                    drop(log);
                    drop(store);
                    shutdown_queue(queue);
                    return Err(e.into());
                }
            }
        } else {
            None
        };

        // ---- M2 wiring ----
        let services = Arc::new(Mutex::new(ServiceRegistry::new()));
        let registry = ContributionRegistry::new(Arc::clone(&services));
        let composition = CompositionStore::new(&registry);
        let scopes = ScopeTree::new_root();
        let policy = RetentionGate::new(Arc::clone(&cfg.policy));

        // Engine: load the guest wasm; NotBuilt → modules disabled. The
        // StateStore currency callback is a placeholder — ModuleManager::new
        // re-binds it to the manager's token table (the session cannot
        // reference the manager before it exists).
        let (modules, vm_engine_digest) = match Vm::load(cfg.engine.clone().unwrap_or_default()) {
            Ok(vm) => {
                let vm_engine_digest = vm.engine_digest();
                let mut state = kanbei_modules::StateStore::open(
                    &cfg.dir.join("state"),
                    Arc::clone(&queue),
                    Arc::new(|_| false),
                );
                state.set_max_state_bytes(cfg.max_state_bytes);
                let manager = ModuleManager::new(
                    vm,
                    ObjectStore::open(&cfg.dir.join("objects"), Arc::clone(&queue))?,
                    state,
                    Arc::clone(&services),
                )?;
                // Bind the kernel session identity for capability principals.
                let mut manager = manager;
                manager.set_session(Id128::generate());
                (Some(manager), Some(vm_engine_digest))
            }
            Err(GuestError::NotBuilt) => (None, None),
            Err(e) => {
                drop(log);
                drop(store);
                shutdown_queue(queue);
                return Err(SessionError::Module(ModuleError::Vm(e)));
            }
        };

        let provider_engine = cfg.provider_engine.take().or_else(|| {
            cfg.provider
                .as_ref()
                .map(|p| kanbei_provider::engine_for(p, cfg.protocol))
        });
        let broker = std::mem::take(&mut cfg.broker);
        let fs_root = cfg.fs_root.clone();
        let tool_limits = cfg.tool_limits;
        let approval_bound = cfg.approval_bound;
        let approval_resolver = cfg.approval_resolver.clone();
        let commit_listener = cfg.commit_listener.clone();
        let cancel_flag = cfg.cancel_flag.clone();
        let delta_listener = cfg.delta_listener.clone();
        let transcript = cfg
            .transcript
            .take()
            .unwrap_or_else(|| Box::new(kanbei_transcript::ConversationProjection::new()));
        let transcript_listener = cfg.transcript_listener.clone();
        let budgets = cfg.budgets;
        let breaker_floors = cfg.breaker_floors;
        let provider_config = cfg.provider.clone();
        let session_id = cfg.session_id.unwrap_or_else(Id128::generate);
        #[cfg(feature = "otel")]
        let telemetry = cfg.telemetry.take();

        // ---- M4 memory substrate wiring (R-11) ----
        // Canonical memory is load-bearing: corrupt memory state is a hard
        // open error (safe mode is config-only, never memory).
        let memory_root = cfg
            .memory_root
            .clone()
            .unwrap_or_else(|| cfg.dir.join("memory"));
        std::fs::create_dir_all(&memory_root)?;
        let memory_fault = cfg.memory_fault.clone();
        let project_id = cfg.project;
        let child_provider = cfg.child_provider.take();
        let mut memory_lifetime = kanbei_memory::MemoryRootActor::open(
            &memory_root,
            kanbei_memory::MemoryScope::Lifetime,
        )
        .map_err(SessionError::Memory)?;
        memory_lifetime.set_fault(memory_fault.clone());
        let (memory_project, project_entry) = match project_id {
            Some(project_id) => {
                let mut registry =
                    kanbei_memory::ProjectRegistry::open(&memory_root.join("projects.jsonl"))
                        .map_err(SessionError::Memory)?;
                let entry = match registry.lookup(project_id).map_err(SessionError::Memory)? {
                    Some(entry) => entry,
                    None => {
                        let entry = kanbei_memory::ProjectEntry {
                            schema: kanbei_memory::PROJECT_ENTRY_SCHEMA,
                            project_id,
                            name: "default".into(),
                            // The scope dir name under <memory_root>/,
                            // matching MemoryScope::dir_name.
                            dir: format!("projects/{project_id}"),
                            created_session: session_id,
                            created_event: next_seq,
                        };
                        registry
                            .register(entry.clone())
                            .map_err(SessionError::Memory)?;
                        entry
                    }
                };
                let mut actor = kanbei_memory::MemoryRootActor::open(
                    &memory_root,
                    kanbei_memory::MemoryScope::Project(project_id),
                )
                .map_err(SessionError::Memory)?;
                actor.set_fault(memory_fault.clone());
                (Some(actor), Some(entry))
            }
            None => (None, None),
        };
        let mut memory_index =
            kanbei_retrieval::MemoryIndex::open(&memory_root.join("projection.sqlite"))
                .map_err(SessionError::Retrieval)?;
        {
            let lifetime_fold = memory_lifetime
                .fold(memory_lifetime.head())
                .map_err(SessionError::Memory)?;
            let mut inputs = vec![kanbei_retrieval::ScopeIndexInput {
                scope: kanbei_memory::MemoryScope::Lifetime,
                root: memory_lifetime.head(),
                fold: lifetime_fold,
            }];
            if let Some(actor) = &memory_project {
                let project_fold = actor.fold(actor.head()).map_err(SessionError::Memory)?;
                inputs.push(kanbei_retrieval::ScopeIndexInput {
                    scope: kanbei_memory::MemoryScope::Project(
                        project_id.expect("project actor implies a bound project"),
                    ),
                    root: actor.head(),
                    fold: project_fold,
                });
            }
            memory_index
                .build(&inputs, kanbei_retrieval::SALIENCE_VERSION)
                .map_err(SessionError::Retrieval)?;
        }

        // R-11 backlink recovery: transitions originating from this session
        // that lack a committed backlink are backed at open — idempotent by
        // TransitionId, so reopens never duplicate.
        let mut backed: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut pending_backlinks: Vec<(Id128, kanbei_memory::MemoryScope)> = Vec::new();
        kanbei_log::for_each_frame(&log_path, |info| {
            for line in &info.events {
                let Ok(env) = Envelope::from_line(line) else {
                    continue;
                };
                if env.kind == "memory_transition_backlink"
                    && let Some(tid) = env.payload.get("transition_id").and_then(|t| t.as_str())
                {
                    backed.insert(tid.to_string());
                }
            }
        })?;
        for tid in memory_lifetime.scan_backlink_candidates(session_id) {
            if !backed.contains(&tid.to_string()) {
                pending_backlinks.push((tid, kanbei_memory::MemoryScope::Lifetime));
            }
        }
        if let Some(actor) = &memory_project {
            for tid in actor.scan_backlink_candidates(session_id) {
                if !backed.contains(&tid.to_string()) {
                    pending_backlinks.push((
                        tid,
                        kanbei_memory::MemoryScope::Project(
                            project_id.expect("project actor implies a bound project"),
                        ),
                    ));
                }
            }
        }

        // R-18/E-06: recover the committed compaction selections (covered
        // fragment ids the commit FSM rejects afterwards).
        let mut compacted: Vec<CompactedRange> = Vec::new();
        kanbei_log::for_each_frame(&log_path, |info| {
            for line in &info.events {
                let Ok(env) = Envelope::from_line(line) else {
                    continue;
                };
                if env.kind != "compaction_selected" {
                    continue;
                }
                if let Some(range) = env.payload.get("range").and_then(|r| r.as_array())
                    && range.len() == 2
                    && let Some(start) = range[0].as_u64()
                    && let Some(end) = range[1].as_u64()
                    && let Some(summary) =
                        env.payload.get("summary_digest").and_then(|d| d.as_str())
                    && let Ok(summary) = summary.parse::<Digest>()
                {
                    let covered = env
                        .payload
                        .get("covered_fragments")
                        .and_then(|f| f.as_array())
                        .map(|arr| {
                            arr.iter()
                                .filter_map(|f| f.as_str().map(str::to_string))
                                .collect()
                        })
                        .unwrap_or_default();
                    compacted.push(CompactedRange {
                        range: (start, end),
                        summary_digest: summary,
                        covered_fragments: covered,
                    });
                }
            }
        })?;

        // M6: recover the committed branch transitions (chronological; the
        // log is the authority for branch identity). The current branch is
        // the last record's; a log without transitions gets a fresh root
        // branch — the M1/M2 genesis path commits no genesis event, so the
        // root id is session-lifetime state (wave 1).
        let mut branch_records: Vec<BranchRecord> = Vec::new();
        let mut branch_error: Option<SessionError> = None;
        kanbei_log::for_each_frame(&log_path, |info| {
            for line in &info.events {
                if branch_error.is_some() {
                    return;
                }
                let env = match decode_record(line, &["branch_transition"]) {
                    Ok(Some(env)) => env,
                    Ok(None) => continue,
                    Err(e) => {
                        branch_error = Some(e);
                        return;
                    }
                };
                let seq = env.seq;
                // Wave-1 transitions recorded follow/config_choice as null and
                // omitted from_branch on a root; null/absent take the field's
                // documented default. A present-but-malformed value is codec
                // drift and fails loud through the wire decode (decision 15).
                let wire: BranchTransitionWire = match serde_json::from_value(env.payload) {
                    Ok(wire) => wire,
                    Err(e) => {
                        branch_error = Some(SessionError::CorruptRecord(format!(
                            "branch_transition at seq {seq}: malformed payload: {e}"
                        )));
                        return;
                    }
                };
                branch_records.push(BranchRecord {
                    id: wire.branch,
                    from: wire.from_branch,
                    frontier_seq: wire.frontier_seq,
                    transition_seq: seq,
                    follow: wire
                        .follow
                        .unwrap_or(kanbei_memory::MemoryFollowPolicy::FollowHead),
                    config_choice: wire.config_choice.unwrap_or_default(),
                    quiesce: wire.quiesce.unwrap_or_default(),
                });
            }
        })?;
        if let Some(err) = branch_error {
            return Err(err);
        }
        let branch = branch_records
            .last()
            .map(|r| r.id)
            .unwrap_or_else(BranchId::generate);

        // D-F-Kb: a trip pauses cognition until an explicit user resume
        // (architecture.md:120), so a reopened session must stay paused
        // when the canonical `breaker_tripped` record has no later
        // `cognition_resumed`. The trip payload is the canonical event
        // payload, so it deserializes straight back into a BreakerTrip.
        let mut unresumed_trip: Option<kanbei_scheduler::BreakerTrip> = None;
        let mut trip_error: Option<SessionError> = None;
        kanbei_log::for_each_frame(&log_path, |info| {
            for line in &info.events {
                if trip_error.is_some() {
                    return;
                }
                let env = match decode_record(line, &["breaker_tripped", "cognition_resumed"]) {
                    Ok(Some(env)) => env,
                    Ok(None) => continue,
                    Err(e) => {
                        trip_error = Some(e);
                        return;
                    }
                };
                match env.kind.as_str() {
                    "breaker_tripped" => {
                        // The pause state is load-bearing: a corrupt record
                        // must not silently unpause the session (decision 15).
                        match serde_json::from_value(env.payload) {
                            Ok(trip) => unresumed_trip = Some(trip),
                            Err(e) => {
                                trip_error = Some(SessionError::CorruptRecord(format!(
                                    "breaker_tripped at seq {}: malformed payload: {e}",
                                    env.seq
                                )));
                                return;
                            }
                        }
                    }
                    "cognition_resumed" => unresumed_trip = None,
                    _ => {}
                }
            }
        })?;
        if let Some(err) = trip_error {
            return Err(err);
        }

        let config_layers = cfg.config_layers.clone();
        let config_discovery_error = cfg.config_discovery_error.clone();
        let mut session = Self {
            log,
            store,
            queue,
            next_seq,
            current_snapshot,
            log_path,
            cfg,
            services,
            scopes,
            composition,
            registry,
            policy,
            modules,
            vm_engine_digest,
            ui_host: None,
            hooks: crate::hooks::HookSet::default(),
            hook_respawned: std::collections::HashSet::new(),
            scheduler: kanbei_scheduler::Scheduler::new(budgets, breaker_floors),
            provider: provider_engine,
            provider_config,
            tool_registry: kanbei_tools::ToolRegistry::builtin(),
            native_tools: kanbei_tools::NativeTools {
                limits: tool_limits,
                ..Default::default()
            },
            broker,
            approvals: std::collections::VecDeque::new(),
            approval_bound,
            approval_resolver,
            commit_listener,
            cancel_flag,
            delta_listener,
            transcript,
            transcript_listener,
            transcript_overrides: CollapseOverrides::new(),
            fs_root,
            session_id,
            memory_lifetime,
            memory_project,
            memory_index,
            project_entry,
            projection_state: None,
            last_provider: None,
            last_opaque: None,
            last_cache: None,
            recent_events: std::collections::VecDeque::new(),
            compacted,
            branch,
            branch_records,
            config_digest: None,
            config_manifest: None,
            host_settings: SettingsContribution::default(),
            config_layers: Vec::new(),
            safe_mode_committed: false,
            pinned_roots: None,
            child_provider,
            #[cfg(feature = "otel")]
            telemetry,
            #[cfg(feature = "otel")]
            open_run_span: None,
            gc_pins: std::sync::Mutex::new(std::collections::HashSet::new()),
        };

        // Decision 30: the transcript projection rebuilds from the canonical
        // log on open (launch = resume, R-19). Replay applies every committed
        // envelope, then resolves a leftover active turn from its recorded
        // terminal outcome. Subsequent commits (config activation, recovery
        // facts) flow through the commit path.
        session.replay_transcript()?;

        // D-F-Kb: re-arm the pause the log still carries, so the reopened
        // session denies cognition until the user resumes it.
        if let Some(trip) = unresumed_trip {
            session.scheduler.pause(trip);
        }

        // M8 wave 2: best-effort automatic GC pass at open (quarantine
        // now, sweep per config; a GC failure must never fail open — the
        // explicit run_gc surfaces errors).
        if let Some(gc_cfg) = session.cfg.gc.clone() {
            session.run_auto_gc(&gc_cfg);
        }

        // Root config layers (decision 28): activated LOW→HIGH. A failing
        // non-builtin layer drops the non-builtin layers and keeps the
        // built-in generation active in safe mode (R-01/C-02).
        session.activate_config_layers(config_layers)?;
        // F14/F: a discovery read failure is recorded as a canonical fact, but
        // it is NOT an activation failure — give the reason a distinct prefix so
        // consumers can tell "degraded by discovery" from "activation failed".
        // When activation already entered safe mode, do not double-commit.
        if let Some(reason) = config_discovery_error
            && !session.safe_mode_committed
        {
            session.commit_safe_mode(&format!("config discovery degraded: {reason}"))?;
        }

        // M4 recovery facts: commit the pending backlinks (R-11), then the
        // one-time canonical project binding (fresh logs only — the log
        // already carries it on resume).
        if !pending_backlinks.is_empty() {
            session.commit(
                pending_backlinks
                    .into_iter()
                    .map(|(tid, scope)| NewEvent {
                        kind: "memory_transition_backlink".into(),
                        payload_schema: 1,
                        payload: json!({
                            "transition_id": tid.to_string(),
                            "scope": serde_json::to_value(&scope)
                                .expect("scope serialization cannot fail"),
                        }),
                        objects: Vec::new(),
                        refs: Vec::new(),
                    })
                    .collect(),
                None,
            )?;
        }
        if let Some(project_id) = project_id
            && recovered.events == 0
        {
            session.commit(
                vec![NewEvent {
                    kind: "project_bound".into(),
                    payload_schema: 1,
                    payload: json!({
                        "project_id": project_id.to_string(),
                        "memory_root": memory_root.to_string_lossy(),
                    }),
                    objects: Vec::new(),
                    refs: Vec::new(),
                }],
                None,
            )?;
        }

        // M3: classify committed intents without outcomes (B-05) before any
        // new work — recovery facts exist before the session is usable.
        session.classify_pending_intents()?;
        Ok(session)
    }

    /// M6 wave 4 bundle export: a portable, read-only snapshot of the
    /// session's canonical state — the plain JSONL log (`session.log.jsonl`),
    /// the raw frame file (`session.log.zst`), every referenced execution
    /// manifest (`manifests/<digest>.json`), every closure object of those
    /// manifests minus the kernel-embedded identity pins
    /// (`objects/<digest>.bin`), and the report itself (`closure.json`).
    /// Missing objects never fail the export — they are reported in
    /// `missing` and `verified` is false (R-06: honest partial availability).
    pub fn export_bundle(&mut self, dir: &Path) -> Result<ExportReport, SessionError> {
        use std::io::Write as _;
        std::fs::create_dir_all(dir)?;
        std::fs::create_dir_all(dir.join("manifests"))?;
        std::fs::create_dir_all(dir.join("objects"))?;

        // Plain JSONL log export (the session's own framing is dropped; the
        // raw frame copy below preserves it verbatim). Read-only — a torn
        // tail is never truncated here.
        let mut out = io::BufWriter::new(std::fs::File::create(dir.join("session.log.jsonl"))?);
        let mut n = 0u64;
        let mut first_err: Option<io::Error> = None;
        let rec = kanbei_log::for_each_frame(&self.log_path, |info| {
            for e in &info.events {
                if first_err.is_some() {
                    return;
                }
                match writeln!(out, "{e}") {
                    Ok(()) => n += 1,
                    Err(e) => first_err = Some(e),
                }
            }
        })?;
        if let Some(e) = first_err {
            return Err(e.into());
        }
        out.flush()?;
        debug_assert_eq!(n, rec.events);
        std::fs::copy(&self.log_path, dir.join("session.log.zst"))?;

        // The manifest set: every distinct snapshot the log pins, every
        // checkpoint's own payload snapshot, and the live current snapshot.
        let mut manifest_digests: std::collections::BTreeSet<Digest> =
            std::collections::BTreeSet::new();
        kanbei_log::for_each_frame(&self.log_path, |info| {
            for line in &info.events {
                let Ok(env) = Envelope::from_line(line) else {
                    continue;
                };
                if let Some(snap) = env.snapshot {
                    manifest_digests.insert(snap);
                }
                if env.kind == "checkpoint_created"
                    && let Some(snap) = env
                        .payload
                        .get("snapshot")
                        .and_then(|s| s.as_str())
                        .and_then(|s| s.parse().ok())
                {
                    manifest_digests.insert(snap);
                }
            }
        })?;
        if let Some(snap) = self.current_snapshot {
            manifest_digests.insert(snap);
        }

        let mut missing: Vec<Digest> = Vec::new();
        let mut identity_pins: std::collections::BTreeSet<Digest> =
            std::collections::BTreeSet::new();
        let mut exported_objects: std::collections::BTreeSet<Digest> =
            std::collections::BTreeSet::new();
        let mut manifests = 0usize;
        for digest in &manifest_digests {
            let bytes = match self.store.get(digest) {
                Ok(bytes) => bytes,
                // An unreadable referenced manifest is reported, never fatal
                // (the closure is unknowable without it).
                Err(_) => {
                    missing.push(*digest);
                    continue;
                }
            };
            std::fs::write(dir.join("manifests").join(format!("{digest}.json")), &bytes)?;
            manifests += 1;
            let manifest: ExecutionManifest = match serde_json::from_slice(&bytes) {
                Ok(manifest) => manifest,
                // Unreadable manifest bytes are copied as-is (honest bytes)
                // but reported — the closure cannot be derived.
                Err(_) => {
                    missing.push(*digest);
                    continue;
                }
            };
            // Engine/toolchain digests are kernel-embedded build-time
            // identity pins, not store objects — excluded from the closure,
            // recorded in the report (mirror of continue_from).
            let closure = kanbei_snapshot::store_closure(&manifest);
            for pin in [manifest.engine_digest, manifest.toolchain_digest]
                .into_iter()
                .flatten()
            {
                identity_pins.insert(pin);
            }
            for d in closure {
                if self.store.exists(&d) {
                    let bytes = self.store.get(&d)?;
                    std::fs::write(dir.join("objects").join(format!("{d}.bin")), &bytes)?;
                    exported_objects.insert(d);
                } else {
                    missing.push(d);
                }
            }
        }
        missing.sort_unstable();
        missing.dedup();
        let report = ExportReport {
            frames: rec.frames,
            envelopes: n,
            manifests,
            objects: exported_objects.len(),
            missing,
            identity_pins: identity_pins.into_iter().collect(),
            verified: false,
        };
        let verified = report.missing.is_empty();
        let report = ExportReport { verified, ..report };
        std::fs::write(
            dir.join("closure.json"),
            serde_json::to_vec_pretty(&report).expect("export report serialization cannot fail"),
        )?;
        Ok(report)
    }

    /// Scan the committed log for intent-kind events without their
    /// outcome-kind event (B-05/M6): `model_call`→`model_outcome`,
    /// `tool_intent`→`tool_outcome` (paired by call_id, with
    /// `intent_classified` counting as an outcome), `memory_proposal`→
    /// `memory_root_approved`. In seq order.
    fn scan_pending_intents(&self) -> Result<Vec<PendingIntent>, SessionError> {
        let log_path = self.log_path.clone();
        let mut intents: Vec<PendingIntent> = Vec::new();
        let mut resolved_calls: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        // The latest seq of each outcome kind: an intent is pending when no
        // outcome-kind event follows it (the spine commits serially).
        let mut outcome_seqs: std::collections::HashMap<&str, u64> =
            std::collections::HashMap::new();
        kanbei_log::for_each_frame(&log_path, |info| {
            for line in &info.events {
                let Ok(env) = Envelope::from_line(line) else {
                    continue;
                };
                let payload = self.resolved_payload(&env);
                match env.kind.as_str() {
                    "tool_intent" => {
                        if let (Some(call), Some(tool)) = (
                            payload.get("call_id").and_then(|c| c.as_str()),
                            payload.get("tool").and_then(|t| t.as_str()),
                        ) {
                            intents.push(PendingIntent {
                                seq: env.seq,
                                id: env.evt,
                                kind: "tool_intent".into(),
                                call_id: Some(call.to_string()),
                                tool: Some(tool.to_string()),
                                origin_snapshot: env.snapshot,
                            });
                        }
                    }
                    "model_call" | "memory_proposal" => {
                        intents.push(PendingIntent {
                            seq: env.seq,
                            id: env.evt,
                            kind: env.kind.clone(),
                            call_id: None,
                            tool: None,
                            origin_snapshot: env.snapshot,
                        });
                    }
                    "tool_outcome" | "intent_classified" => {
                        if let Some(call) = payload.get("call_id").and_then(|c| c.as_str()) {
                            resolved_calls.insert(call.to_string());
                        }
                    }
                    "model_outcome" => {
                        outcome_seqs.insert("model_call", env.seq);
                    }
                    "memory_root_approved" => {
                        outcome_seqs.insert("memory_proposal", env.seq);
                    }
                    _ => {}
                }
            }
        })?;
        Ok(intents
            .into_iter()
            .filter(|i| match i.kind.as_str() {
                "tool_intent" => !resolved_calls.contains(i.call_id.as_deref().unwrap_or_default()),
                kind => outcome_seqs.get(kind).is_none_or(|s| *s < i.seq),
            })
            .collect())
    }

    /// Tool intents with an interrupted/ambiguous classification, in seq
    /// order (M6: the abandoned-tail `ambiguous` quiesce list).
    fn scan_classified_intents(&self) -> Result<Vec<QuiescedIntent>, SessionError> {
        let log_path = self.log_path.clone();
        // call_id → (intent identity, classification)
        let mut by_call: std::collections::HashMap<String, (QuiescedIntent, Option<String>)> =
            std::collections::HashMap::new();
        kanbei_log::for_each_frame(&log_path, |info| {
            for line in &info.events {
                let Ok(env) = Envelope::from_line(line) else {
                    continue;
                };
                let payload = self.resolved_payload(&env);
                match env.kind.as_str() {
                    "tool_intent" => {
                        if let Some(call) = payload.get("call_id").and_then(|c| c.as_str()) {
                            by_call.entry(call.to_string()).or_insert_with(|| {
                                (
                                    QuiescedIntent {
                                        seq: env.seq,
                                        kind: "tool_intent".into(),
                                        id: env.evt,
                                    },
                                    None,
                                )
                            });
                        }
                    }
                    "intent_classified" => {
                        if let (Some(call), Some(class)) = (
                            payload.get("call_id").and_then(|c| c.as_str()),
                            env.payload.get("classification").and_then(|c| c.as_str()),
                        ) && let Some(entry) = by_call.get_mut(call)
                        {
                            entry.1 = Some(class.to_string());
                        }
                    }
                    _ => {}
                }
            }
        })?;
        Ok(by_call
            .into_iter()
            .filter(|(_, (_, class))| {
                matches!(class.as_deref(), Some("interrupted") | Some("ambiguous"))
            })
            .map(|(_, (intent, _))| intent)
            .collect())
    }

    /// The module subsystem; None when modules are disabled (guest wasm not
    /// built or safe mode).
    pub fn modules(&self) -> Option<&ModuleManager> {
        self.modules.as_ref()
    }

    /// The bound provider engine; None = storage-only session.
    pub fn provider_engine(&self) -> Option<&dyn kanbei_provider::ProviderEngine> {
        self.provider.as_deref()
    }

    /// The current epoch composition (R-01: EpochId = its digest).
    pub fn broker(&self) -> &kanbei_capabilities::Broker {
        &self.broker
    }

    pub fn composition(&self) -> &Composition {
        self.composition.current()
    }

    pub fn policy(&self) -> &RetentionGate {
        &self.policy
    }

    /// The scope tree (M2: root only; ephemeral child scopes are R-26/C-09).
    pub fn scopes(&self) -> &ScopeTree {
        &self.scopes
    }

    /// Module state heads via the module subsystem; `ModulesDisabled` when no
    /// modules are active.
    pub fn state_heads(&self) -> Result<Vec<(String, HeadFile)>, SessionError> {
        let Some(manager) = self.modules.as_ref() else {
            return Err(SessionError::ModulesDisabled);
        };
        Ok(manager
            .state()
            .lock()
            .expect("state lock poisoned")
            .heads()?)
    }

    /// The loaded guest wasm's digest (manifest `engine_digest`); None when
    /// modules are disabled.
    pub fn vm_engine_digest(&self) -> Option<Digest> {
        self.vm_engine_digest
    }

    /// fsync-before-consequential-effect contract (§3): waits until every
    /// enqueued durability op ran — the log frames and all pending object
    /// dirsyncs.
    pub fn flush(&self) -> Result<(), SessionError> {
        Ok(self.log.flush()?)
    }

    pub fn store(&self) -> &ObjectStore {
        &self.store
    }

    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    /// The optional OTel-compatible telemetry handle (M8 wave 1; feature
    /// `otel`).
    #[cfg(feature = "otel")]
    pub fn telemetry(&self) -> Option<&Telemetry> {
        self.telemetry.as_ref()
    }

    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    /// The memory roots pinned by the current follow policy (`None` =
    /// FollowHead): the checkpoint/fork roots the projection folds instead
    /// of the live actor heads.
    pub fn pinned_roots(&self) -> Option<&PinnedRoots> {
        self.pinned_roots.as_ref()
    }

    pub fn current_snapshot(&self) -> Option<Digest> {
        self.current_snapshot
    }

    /// The live config package digest (the `config_choice` record's `current`
    /// field); None for storage-only sessions and safe-mode opens.
    pub fn config_digest(&self) -> Option<Digest> {
        self.config_digest
    }

    /// The ORDERED (LOW→HIGH) package digests of the active config layers
    /// (F5) — the restore source of truth. `config_digest` is its top
    /// element (or `None` for a storage-only session).
    pub fn config_layer_digests(&self) -> Vec<Digest> {
        self.config_layers.iter().map(|l| l.package).collect()
    }

    /// The merged settings the config layers contributed at open (decision 28),
    /// snapshotted: it reflects the built-in layer in safe mode and is
    /// `SettingsContribution::default()` when no layer contributed settings
    /// (e.g. Wasm not built).
    pub fn host_settings(&self) -> &SettingsContribution {
        &self.host_settings
    }

    /// The session's own identity (caller principal for kernel-originated
    /// tool calls, R-14/D-02).
    pub fn session_id(&self) -> Id128 {
        self.session_id
    }
    /// The current branch id (M6): the last committed `branch_transition`'s
    /// branch, or a fresh id on a branchless session.
    pub fn branch(&self) -> BranchId {
        self.branch
    }
    /// Committed branch records, chronological (rebuilt from the log at
    /// open — the log is the authority for branch identity).
    pub fn branch_records(&self) -> &[BranchRecord] {
        &self.branch_records
    }

    /// The lifetime-scope memory actor (R-11).
    pub fn memory_lifetime(&self) -> &kanbei_memory::MemoryRootActor {
        &self.memory_lifetime
    }

    /// The project-scope memory actor; None when no project is bound.
    pub fn memory_project(&self) -> Option<&kanbei_memory::MemoryRootActor> {
        self.memory_project.as_ref()
    }

    /// The per-session projection index (disposable SQLite).
    pub fn memory_index(&self) -> &kanbei_retrieval::MemoryIndex {
        &self.memory_index
    }

    /// The bound project's registry entry.
    pub fn project_entry(&self) -> Option<&kanbei_memory::ProjectEntry> {
        self.project_entry.as_ref()
    }

    /// The last materialized projection state (M4 staged pipeline).
    pub fn projection_state(&self) -> Option<&ProjectionState> {
        self.projection_state.as_ref()
    }

    /// Flush, then stop the durability worker and join it. Drops the module
    /// subsystem (and with it the wasm watchdog) first, releasing its queue
    /// clones before the queue's final Arc is unwrapped. Fails while any
    /// `Generation` handle (e.g. a `ReplacementOutcome`) is still alive — a
    /// live instance keeps the host's state store (and its queue clones)
    /// alive; drop or `dispose` it first.
    pub fn close(self) -> Result<(), SessionError> {
        #[cfg(feature = "otel")]
        self.telemetry_flush()?;
        let Session {
            log,
            store,
            queue,
            modules,
            memory_lifetime,
            memory_project,
            ..
        } = self;
        log.flush()?;
        // The memory actors barrier their own durability queues before the
        // session queue shuts down (their workers exit when the last Arc
        // drops).
        memory_lifetime.flush().map_err(SessionError::Memory)?;
        if let Some(project) = &memory_project {
            project.flush().map_err(SessionError::Memory)?;
        }
        drop(log);
        drop(store);
        drop(modules);
        drop(memory_lifetime);
        drop(memory_project);
        let queue = Arc::try_unwrap(queue)
            .map_err(|_| SessionError::InvalidInput("durability queue still shared".into()))?;
        queue.shutdown()?;
        Ok(())
    }

    pub(crate) fn fault(&self, point: FaultPoint) {
        if let Some(f) = &self.cfg.fault {
            f.inject(point);
        }
    }
}

// ---------- errors ----------

#[derive(Debug, Error)]
pub enum SessionError {
    /// The user cancelled the in-flight run (Ctrl-C) at a model-call stream
    /// boundary; the cognition loop maps this to `Failed(UserCancelled)`.
    #[error("run cancelled by user")]
    Cancelled,
    /// The provider call failed (transport/HTTP/malformed response); the
    /// driver maps this to `Failed(Provider)`, never `UserCancelled`.
    #[error("provider error: {0}")]
    Provider(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Log(#[from] kanbei_log::RecoveryError),
    #[error(transparent)]
    Object(#[from] ObjectError),
    #[error("envelope: {0}")]
    Envelope(EnvelopeError),
    #[error("event references missing object: {digest}")]
    MissingObject { digest: Digest },
    #[error("config layer package {digest} is missing from the store; the fork cannot restore it")]
    MissingConfigLayer { digest: Digest },
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("snapshot: {0}")]
    Snapshot(String),
    #[error("modules are disabled (guest wasm not built or safe mode)")]
    ModulesDisabled,
    #[error("generation {generation} is stale")]
    StaleGeneration { generation: u64 },
    #[error(transparent)]
    Module(#[from] ModuleError),
    #[error(transparent)]
    Scope(#[from] kanbei_scopes::errors::ScopeError),
    #[error(transparent)]
    Policy(#[from] kanbei_policy::PolicyError),
    #[error(transparent)]
    State(#[from] kanbei_modules::StateError),
    #[error(transparent)]
    Service(#[from] kanbei_services::ServiceError),
    #[error("config activation failed: {0}")]
    ConfigActivation(String),
    #[error("effect dispatch failed: {0}")]
    Effect(String),
    #[error(transparent)]
    Scheduler(#[from] kanbei_scheduler::SchedulerError),
    #[error(transparent)]
    Memory(#[from] kanbei_memory::MemoryError),
    #[error(transparent)]
    Retrieval(#[from] kanbei_retrieval::RetrievalError),
    #[error(transparent)]
    Context(#[from] kanbei_context::ProjectionError),
    #[error("compaction violation: event references compacted fragment {0}")]
    CompactionViolation(String),
    #[error("corrupt canonical record: {0}")]
    CorruptRecord(String),
    #[error(transparent)]
    Gc(#[from] kanbei_gc::GcError),
    #[error(transparent)]
    Workspace(#[from] kanbei_workspace::WorkspaceError),
}

// M3 agent spine: run lifecycle, model/tool commit paths, approvals, breakers,
// and interrupted/ambiguous classification (spine.rs).
mod spine;

// T9 lifecycle hooks (decision parsing, ordered bindings, fault policy).
mod hooks;

// M8 wave 2: canonical-object GC (root capture, writer pins, quarantine +
// grace sweep) over the session and memory stores (gc.rs).
mod gc;

// M9 wave 4: content-addressed working-tree snapshots and restore over
// kanbei-workspace (workspace.rs).
mod workspace;
