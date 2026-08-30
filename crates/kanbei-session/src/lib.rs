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
use kanbei_core::envelope::{ENVELOPE_SCHEMA, Envelope, EnvelopeError};
use kanbei_core::id::{BranchId, Id128};
use kanbei_core::queue::DurabilityQueue;
use kanbei_log::{AppendLog, Profile, Recovered};
use kanbei_modules::{
    HeadFile, ModuleError, ModuleManager, PackageManifest, ReplacementOutcome, StateUpdate,
};
use kanbei_objects::{ObjectError, ObjectStore};
use kanbei_policy::builtins::StoreAllPolicy;
use kanbei_policy::{Admission, BoundaryKind, Candidate, PolicyPlugin, RetentionGate};
use kanbei_scopes::contrib::{Contribution, ContributionKind, ServiceContribution};
use kanbei_scopes::epoch::{Composition, CompositionStore};
use kanbei_scopes::registry::ContributionRegistry;
use kanbei_scopes::scope_tree::ScopeTree;
use kanbei_services::{ServiceKey, ServiceProvider, ServiceRegistry};
use kanbei_snapshot::ExecutionManifest;
use kanbei_vm::{GuestError, Host, Vm};
use serde_json::json;
use thiserror::Error;

mod ui;
pub use ui::{UiHost, UiIntent, UiOutcome, UI_INTENT_RESOURCE};

/// The bounded recent-event ring size: the trajectory render covers the
/// full canonical history, but the CONTENT is these most-recent events.
const RECENT_RING: usize = 64;

/// Checkpoint label length cap (M6): beyond this the label is rejected.
const CHECKPOINT_LABEL_MAX: usize = 200;

// ---------- config ----------

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
    /// Root config module to activate at open (R-01/C-02); None = no modules.
    pub config: Option<PackageManifest>,
    /// Module state-head size ceiling (R-07); default 1 MB.
    pub max_state_bytes: usize,
    /// Retention policy plugin; default [`StoreAllPolicy`].
    pub policy: Arc<dyn PolicyPlugin>,
    /// Wasm engine config; None = [`kanbei_vm::VmConfig::default`]. When the
    /// guest wasm is not built (`Vm::load` → `NotBuilt`), modules are
    /// disabled (a `config` then opens in safe mode).
    pub engine: Option<kanbei_vm::VmConfig>,
    // --- M3 agent spine ---
    /// Provider gateway config; None = no model calls (storage-only session).
    pub provider: Option<kanbei_provider::ProviderConfig>,
    /// The provider engine; None = build [`kanbei_provider::HttpEngine`]
    /// from `provider` (tests inject the fake engine).
    pub provider_engine: Option<Box<dyn kanbei_provider::ProviderEngine>>,
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
    /// The session's own identity (caller principal for kernel-originated
    /// tool calls, R-14); None = generate at open.
    pub session_id: Option<Id128>,
    // --- M4 memory substrate + context projection ---
    /// Memory substrate root (canonical XDG state). None = cfg.dir.join("memory").
    pub memory_root: Option<PathBuf>,
    /// ProjectId (pro_ brand) binding; None = no project memory scope.
    pub project: Option<Id128>,
    /// Kernel fault injector for the memory actors (transition/head points).
    pub memory_fault: Option<Arc<dyn kanbei_memory::MemoryFaultInjector>>,
    /// Factory producing a fresh CognitionProvider per spawned child run
    /// (R-09 child runs; None = child.spawn resolves to an error outcome).
    pub child_provider: Option<Box<dyn FnMut() -> Box<dyn kanbei_scheduler::CognitionProvider>>>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("."),
            stream: "default".into(),
            profile: Profile::Fast,
            inline_max: 1024,
            object_min: 8192,
            fault: None,
            config: None,
            max_state_bytes: 1024 * 1024,
            policy: Arc::new(StoreAllPolicy),
            engine: None,
            provider: None,
            provider_engine: None,
            budgets: kanbei_scheduler::Budgets::default(),
            breaker_floors: kanbei_scheduler::BreakerFloors::default(),
            tool_limits: kanbei_tools::ExecLimits::default(),
            fs_root: PathBuf::from("."),
            broker: kanbei_capabilities::Broker::new(),
            approval_bound: 64,
            session_id: None,
            memory_root: None,
            project: None,
            memory_fault: None,
            child_provider: None,
        }
    }
}

// ---------- fault injection ----------

/// Crash-injection points on the commit path and the M2 subsystem seams. The
/// testkit's injector aborts the process at a configured point; `None` (the
/// default) is a no-op.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultPoint {
    BeforeObjectInstall,
    AfterObjectInstall,
    BeforeFrameAppend,
    AfterFrameAppend,
    BeforeEffectDispatch,
    AfterEffectDispatch,
    BeforeConfigActivation,
    AfterConfigActivation,
    BeforeHeadUpdate,
    AfterHeadUpdate,
    // --- M3 agent spine points ---
    BeforeWakeAccept,
    AfterWakeAccept,
    BeforeRunStart,
    AfterRunStart,
    BeforeModelCall,
    AfterModelCall,
    BeforeToolIntentCommit,
    AfterToolIntentCommit,
    BeforeToolDispatch,
    AfterToolDispatch,
    BeforeToolOutcomeCommit,
    AfterToolOutcomeCommit,
    BeforeRunOutcome,
    AfterRunOutcome,
    // --- M4 memory proposal points ---
    BeforeMemoryProposal,
    AfterMemoryProposal,
    // --- M5 semantic workbench points ---
    BeforeUiReduce,
    AfterUiReduce,
    BeforeUiRender,
    AfterUiRender,
    // --- M6 historical-correction points ---
    BeforeCheckpointCommit,
    AfterCheckpointCommit,
    BeforeBranchTransition,
    AfterBranchTransition,
    BeforeSessionHeadAdvance,
    AfterSessionHeadAdvance,
}

pub trait FaultInjector: Send + Sync {
    fn inject(&self, point: FaultPoint);
}

// ---------- commit types ----------

/// One caller-authored event, not yet sequenced or validated.
pub struct NewEvent {
    pub kind: String,
    pub payload_schema: u32,
    pub payload: serde_json::Value,
    /// Installed as objects before the frame is appended; their digests are
    /// appended to `refs` (R-10).
    pub objects: Vec<Vec<u8>>,
    /// Must already exist in the store — a commit never creates a dangling
    /// reference.
    pub refs: Vec<Digest>,
}

/// What a committed batch consumed: sequence span, frame size, installed
/// object digests, and the manifest digests bracketing the commit.
#[derive(Debug)]
pub struct CommitReceipt {
    pub first_seq: u64,
    pub last_seq: u64,
    pub count: u64,
    pub frame_len: u64,
    /// Digests installed by this commit's step-2 object phase (event objects
    /// + promoted payloads; the post-state manifest, if any, is excluded).
    pub objects: Vec<Digest>,
    /// The manifest digest every envelope in this commit references (R-08);
    /// None when the session resumed without manifest state (M1).
    pub pre_snapshot: Option<Digest>,
    /// The manifest pinned because this commit changed state; None for pure
    /// commits (unchanged manifests dedup via content addressing).
    pub post_snapshot: Option<Digest>,
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
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ConfigChoiceRecord {
    pub mode: String,
    pub current: Option<Digest>,
    pub historical: Option<Digest>,
    pub composition: Option<Digest>,
}

/// The memory roots pinned by the checkpoint a branch continues from (M6;
/// wave 2 consumes them).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinnedRoots {
    pub lifetime: Digest,
    pub project: Option<Digest>,
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
    /// The memory roots pinned by the checkpoint this branch continues from
    /// (wave 2 consumes them).
    pinned_roots: Option<PinnedRoots>,
    /// The memory actors' fault injector (cloned into both actors at open).
    /// Held here per the crash-test contract so the harness can arm it via
    /// its own shared Arc before the memory flow runs; the session itself
    /// never fires it.
    #[allow(dead_code)]
    memory_fault: Option<Arc<dyn kanbei_memory::MemoryFaultInjector>>,
    /// Child-run provider factory (R-09); None = child.spawn errors.
    child_provider: Option<Box<dyn FnMut() -> Box<dyn kanbei_scheduler::CognitionProvider>>>,
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
    /// state store over `<dir>/state`. A `cfg.config` manifest is then
    /// activated atomically; on any failure the module subsystem is dropped
    /// and a canonical `safe_mode_activated` event is committed — the session
    /// remains usable with storage only (R-01/C-02).
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
            cfg.provider.as_ref().map(|p| {
                Box::new(kanbei_provider::HttpEngine::new(p.clone()))
                    as Box<dyn kanbei_provider::ProviderEngine>
            })
        });
        let broker = std::mem::take(&mut cfg.broker);
        let fs_root = cfg.fs_root.clone();
        let tool_limits = cfg.tool_limits;
        let approval_bound = cfg.approval_bound;
        let budgets = cfg.budgets;
        let breaker_floors = cfg.breaker_floors;
        let provider_config = cfg.provider.clone();
        let session_id = cfg.session_id.unwrap_or_else(Id128::generate);

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
        kanbei_log::for_each_frame(&log_path, |info| {
            for line in &info.events {
                let Ok(env) = Envelope::from_line(line) else {
                    continue;
                };
                if env.kind != "branch_transition" {
                    continue;
                }
                let Some(branch) = env
                    .payload
                    .get("branch")
                    .and_then(|b| b.as_str())
                    .and_then(|b| b.parse::<BranchId>().ok())
                else {
                    continue;
                };
                let from = env
                    .payload
                    .get("from_branch")
                    .and_then(|f| f.as_str())
                    .and_then(|f| f.parse::<BranchId>().ok());
                let Some(frontier) = env.payload.get("frontier_seq").and_then(|f| f.as_u64()) else {
                    continue;
                };
                let follow = env
                    .payload
                    .get("follow")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                // Wave-1 transitions recorded follow/config_choice as null;
                // null follow means the branch could not pin (FollowHead) and
                // a null choice is the empty record.
                let follow = if follow.is_null() {
                    kanbei_memory::MemoryFollowPolicy::FollowHead
                } else {
                    serde_json::from_value(follow)
                        .unwrap_or(kanbei_memory::MemoryFollowPolicy::FollowHead)
                };
                let config_choice = env
                    .payload
                    .get("config_choice")
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                let config_choice = if config_choice.is_null() {
                    ConfigChoiceRecord {
                        mode: String::new(),
                        current: None,
                        historical: None,
                        composition: None,
                    }
                } else {
                    serde_json::from_value(config_choice)
                        .unwrap_or(ConfigChoiceRecord {
                            mode: String::new(),
                            current: None,
                            historical: None,
                            composition: None,
                        })
                };
                let quiesce = env
                    .payload
                    .get("quiesce")
                    .and_then(|q| serde_json::from_value(q.clone()).ok())
                    .unwrap_or_default();
                branch_records.push(BranchRecord {
                    id: branch,
                    from,
                    frontier_seq: frontier,
                    transition_seq: env.seq,
                    follow,
                    config_choice,
                    quiesce,
                });
            }
        })?;
        let branch = branch_records
            .last()
            .map(|r| r.id)
            .unwrap_or_else(BranchId::generate);

        let config_manifest = cfg.config.clone();
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
            fs_root,
            session_id,
            memory_lifetime,
            memory_project,
            memory_index,
            project_entry,
            projection_state: None,
            last_provider: None,
            last_cache: None,
            recent_events: std::collections::VecDeque::new(),
            compacted,
            branch,
            branch_records,
            config_digest: None,
            pinned_roots: None,
            memory_fault,
            child_provider,
        };

        // Root config module: atomic activation; failure → safe mode.
        if let Some(manifest) = config_manifest
            && let Err(e) = session.activate_config(manifest)
        {
            session.modules = None;
            session.vm_engine_digest = None;
            let reason = e.to_string();
            session.commit(
                vec![NewEvent {
                    kind: "safe_mode_activated".into(),
                    payload_schema: 1,
                    payload: json!({ "reason": reason }),
                    objects: Vec::new(),
                    refs: Vec::new(),
                }],
                None,
            )?;
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

    /// Serialized single-writer commit path: install objects (R-10), verify
    /// explicit refs, classify payloads (§7), build envelopes against the
    /// pre-event snapshot (R-08), append one frame, then pin a post-event
    /// manifest iff `state_head` is given. Ack = write + enqueue on the
    /// durability queue (§3); call [`Session::flush`] before consequential
    /// effects.
    ///
    /// M2: the post-event manifest is the schema-2 bootstrap extended with
    /// the module-generation pins (`ModuleManager::snapshot`; scope "/" — M2
    /// activates root-scope modules only), the current composition digest
    /// (R-01: EpochId = composition digest), the engine digest, and
    /// `module_abi = 1`. The toolchain digest stays None — M2 sessions do not
    /// track a toolchain. Content addressing keeps dedup semantics: identical
    /// manifests pin to the same digest.
    pub fn commit(
        &mut self,
        mut events: Vec<NewEvent>,
        state_head: Option<Digest>,
    ) -> Result<CommitReceipt, SessionError> {
        if events.is_empty() {
            return Err(SessionError::InvalidInput("empty commit".into()));
        }

        // R-18/E-06 compaction FSM: a new event whose payload carries a
        // fragment id folded into a committed compaction selection is
        // rejected — its causal parents live inside the compacted range.
        for ev in &events {
            if let Some(fragment) = ev.payload.get("fragment").and_then(|f| f.as_str())
                && self
                    .compacted
                    .iter()
                    .any(|c| c.covered_fragments.iter().any(|f| f == fragment))
            {
                return Err(SessionError::CompactionViolation(fragment.to_string()));
            }
        }

        // step 2 — objects first: the object dirsync is enqueued before the
        // referencing frame's fsync, so the object is durable before the
        // frame (ratification-packet §3, R-10)
        self.fault(FaultPoint::BeforeObjectInstall);
        let mut objects: Vec<Digest> = Vec::new();
        let mut payload_schemas: Vec<u32> = Vec::new();
        for ev in &mut events {
            for bytes in &ev.objects {
                let digest = self.store.install(bytes)?;
                self.fault(FaultPoint::AfterObjectInstall);
                ev.refs.push(digest);
                objects.push(digest);
            }
            // explicit refs must already exist — never commit a newly created
            // dangling reference (R-10)
            for r in &ev.refs {
                if !self.store.exists(r) {
                    return Err(SessionError::MissingObject { digest: *r });
                }
            }
            // payload classification (§7): > inline_max → object reference;
            // the 1–8 KB middle band stays inline (M1 default), so object_min
            // is not consulted
            let serialized = serde_json::to_string(&ev.payload)
                .map_err(|e| SessionError::InvalidInput(format!("payload serialization: {e}")))?;
            if serialized.len() > self.cfg.inline_max {
                let digest = self.store.install(serialized.as_bytes())?;
                self.fault(FaultPoint::AfterObjectInstall);
                ev.payload = json!({ "$object": digest.to_string() });
                ev.refs.push(digest);
                objects.push(digest);
            }
            payload_schemas.push(ev.payload_schema);
        }

        // step 3 — envelopes: every canonical event references its pre-event
        // commit-snapshot digest (R-08)
        let first_seq = self.next_seq;
        let pre_snapshot = self.current_snapshot;
        let envelopes: Vec<Envelope> = events
            .iter()
            .enumerate()
            .map(|(i, ev)| Envelope {
                env: ENVELOPE_SCHEMA,
                seq: first_seq + i as u64,
                evt: Id128::generate().to_string(),
                kind: ev.kind.clone(),
                payload_schema: ev.payload_schema,
                payload: ev.payload.clone(),
                refs: ev.refs.clone(),
                snapshot: pre_snapshot,
            })
            .collect();

        // step 4 — one frame through the durability queue
        self.fault(FaultPoint::BeforeFrameAppend);
        let plan = self.log.append(&envelopes, self.cfg.profile)?;
        self.fault(FaultPoint::AfterFrameAppend);
        self.fault(FaultPoint::BeforeSessionHeadAdvance);
        self.next_seq = plan.last_seq + 1;
        self.fault(FaultPoint::AfterSessionHeadAdvance);

        // The bounded recent-event ring (the trajectory render source):
        // every committed event enters it; the oldest fall off past
        // RECENT_RING entries.
        for (i, ev) in events.iter().enumerate() {
            self.recent_events.push_back((
                first_seq + i as u64,
                ev.kind.clone(),
                ev.payload.clone(),
            ));
        }
        while self.recent_events.len() > RECENT_RING {
            self.recent_events.pop_front();
        }
        // A committed compaction selection joins the FSM's covered set (the
        // check above rejects its covered fragments from then on).
        for ev in &events {
            if ev.kind != "compaction_selected" {
                continue;
            }
            if let Some(range) = ev.payload.get("range").and_then(|r| r.as_array())
                && range.len() == 2
                && let Some(start) = range[0].as_u64()
                && let Some(end) = range[1].as_u64()
                && let Some(summary) = ev.payload.get("summary_digest").and_then(|d| d.as_str())
                && let Ok(summary) = summary.parse::<Digest>()
            {
                let covered = ev
                    .payload
                    .get("covered_fragments")
                    .and_then(|f| f.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|f| f.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                self.compacted.push(CompactedRange {
                    range: (start, end),
                    summary_digest: summary,
                    covered_fragments: covered,
                });
            }
        }

        // step 5 — state-changing commits pin a post-event manifest; pure
        // commits leave the manifest unchanged (content addressing dedups
        // identical manifests)
        let post_snapshot = match state_head {
            Some(head) => {
                let manifest = self.build_manifest(Some(head), &payload_schemas);
                // the composition's canonical bytes must exist as an object
                // for the manifest's composition ref to be closure-valid
                // (R-10); install dedups when the publish already pinned them
                let comp_bytes = self.composition.current().to_canonical_bytes();
                self.store.install(&comp_bytes)?;
                // the tool-registry/provider-config objects the manifest pins
                // (M6 wave 2) — installed before the pin, same as the
                // composition object.
                for bytes in self.manifest_config_objects() {
                    self.store.install(&bytes)?;
                }
                let (digest, _deduped) = kanbei_snapshot::pin(&mut self.store, &manifest)?;
                self.current_snapshot = Some(digest);
                Some(digest)
            }
            None => None,
        };

        Ok(CommitReceipt {
            first_seq: plan.first_seq,
            last_seq: plan.last_seq,
            count: plan.count,
            frame_len: plan.frame_len,
            objects,
            pre_snapshot,
            post_snapshot,
        })
    }

    // ---------- M6 historical correction (branching) ----------

    /// Commit one canonical `checkpoint_created` event (M6): a record event
    /// freezing the current frontier — its own seq — with the post-event
    /// manifest digest, the pinned memory roots, the composition digest, and
    /// the current branch. The manifest is built with the same
    /// [`Session::build_manifest`] helper commit step 5 uses, so its digest
    /// (computed before the commit) is byte-exact with the receipt's
    /// post_snapshot.
    pub fn create_checkpoint(&mut self, label: Option<String>) -> Result<CheckpointRef, SessionError> {
        if label.as_ref().is_some_and(|l| l.chars().count() > CHECKPOINT_LABEL_MAX) {
            return Err(SessionError::InvalidInput(format!(
                "checkpoint label exceeds {CHECKPOINT_LABEL_MAX} characters"
            )));
        }
        let seq = self.next_seq;
        let state_head = Some(self.composition.current().digest);
        let manifest = self.build_manifest(state_head, &[1]);
        let snapshot = Digest::new(&manifest.to_bytes());
        // The manifest pins memory roots whose objects live in the memory
        // stores; install them into the session store so the checkpoint's
        // snapshot closure is verifiable from the session store alone (the
        // checkpoint event's refs then cover its pinned roots, R-10).
        let mut objects: Vec<Vec<u8>> = Vec::new();
        if let Some(root) = self.memory_lifetime.head() {
            let bytes = self
                .memory_lifetime
                .store()
                .get(&root)
                .map_err(|e| SessionError::Snapshot(format!("checkpoint lifetime root {root} unreadable: {e}")))?;
            objects.push(bytes);
        }
        if let Some(root) = self.memory_project.as_ref().and_then(|a| a.head()) {
            let bytes = self
                .memory_project
                .as_ref()
                .expect("project head implies actor")
                .store()
                .get(&root)
                .map_err(|e| SessionError::Snapshot(format!("checkpoint project root {root} unreadable: {e}")))?;
            objects.push(bytes);
        }
        self.fault(FaultPoint::BeforeCheckpointCommit);
        let receipt = self.commit(
            vec![NewEvent {
                kind: "checkpoint_created".into(),
                payload_schema: 1,
                payload: json!({
                    "label": label,
                    "frontier_seq": seq,
                    "snapshot": snapshot.to_string(),
                    "memory_root": self.memory_lifetime.head().map(|d| d.to_string()),
                    "project_memory_root": self
                        .memory_project
                        .as_ref()
                        .and_then(|a| a.head())
                        .map(|d| d.to_string()),
                    "composition": self.composition.current().digest.to_string(),
                    "branch": self.branch.to_string(),
                }),
                objects,
                refs: Vec::new(),
            }],
            state_head,
        )?;
        self.fault(FaultPoint::AfterCheckpointCommit);
        debug_assert_eq!(
            receipt.post_snapshot,
            Some(snapshot),
            "checkpoint manifest digest must match the pinned post-snapshot"
        );
        Ok(CheckpointRef {
            session_id: self.session_id,
            seq: receipt.last_seq,
        })
    }

    /// Branch off a committed checkpoint (M6): validate the checkpoint
    /// (session, committed seq, event kind, snapshot closure), quiesce
    /// (cancel the active run as `Failed(Quiesced)`; list pending and
    /// abandoned-tail intents), then commit one canonical
    /// `branch_transition` event and switch the session to the new branch.
    /// History is never rewritten — the transition is appended and the new
    /// path is derived by the path filter.
    pub fn continue_from(&mut self, checkpoint: &CheckpointRef) -> Result<BranchRecord, SessionError> {
        if checkpoint.session_id != self.session_id {
            return Err(SessionError::InvalidInput(
                "checkpoint belongs to a different session".into(),
            ));
        }
        if checkpoint.seq == 0 || checkpoint.seq >= self.next_seq {
            return Err(SessionError::InvalidInput(format!(
                "checkpoint seq {} is not a committed event",
                checkpoint.seq
            )));
        }
        let env = self.envelope_at(checkpoint.seq)?;
        if env.kind != "checkpoint_created"
            || env.payload.get("frontier_seq").and_then(|f| f.as_u64()) != Some(checkpoint.seq)
        {
            return Err(SessionError::InvalidInput(format!(
                "event at seq {} is not a checkpoint",
                checkpoint.seq
            )));
        }
        let snapshot: Digest = env
            .payload
            .get("snapshot")
            .and_then(|s| s.as_str())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| {
                SessionError::InvalidInput(format!(
                    "checkpoint at seq {} pins no snapshot",
                    checkpoint.seq
                ))
            })?;
        let bytes = self
            .store
            .get(&snapshot)
            .map_err(|e| SessionError::Snapshot(format!("checkpoint snapshot {snapshot} unreadable: {e}")))?;
        let manifest: ExecutionManifest = serde_json::from_slice(&bytes).map_err(|e| {
            SessionError::Snapshot(format!("checkpoint snapshot {snapshot} is not a manifest: {e}"))
        })?;
        // Full closure walk (M6 wave 2): every digest field the manifest
        // pins must resolve in the session store — modules' packages,
        // composition, memory roots, and the tool-registry/provider-config
        // objects (all installed before the pin). The engine/toolchain
        // digests are kernel-embedded build-time artifacts (the guest wasm
        // is a kanbei-vm `include_bytes!` constant that never enters the
        // object store), so they are the only digest fields excepted from
        // the store verification.
        let mut closure = kanbei_snapshot::manifest_closure(&manifest);
        if let Some(d) = manifest.engine_digest {
            closure.remove(&d);
        }
        if let Some(d) = manifest.toolchain_digest {
            closure.remove(&d);
        }
        kanbei_snapshot::verify_closure(&self.store, &closure)
            .map_err(|e| SessionError::Snapshot(format!("checkpoint snapshot closure failed: {e}")))?;
        let memory_root: Option<Digest> = env
            .payload
            .get("memory_root")
            .and_then(|r| r.as_str())
            .and_then(|r| r.parse().ok());
        let project_memory_root: Option<Digest> = env
            .payload
            .get("project_memory_root")
            .and_then(|r| r.as_str())
            .and_then(|r| r.parse().ok());

        // The follow policy before the transition commits: the checkpoint's
        // pinned roots must be roots the memory actors know — a corrupted
        // checkpoint event is rejected explicitly, with no branch. A
        // checkpoint without a pinned lifetime root cannot pin (the policy's
        // lifetime_root is required) → FollowHead.
        let follow = match memory_root {
            Some(lifetime_root) => {
                if !self.memory_lifetime.contains_root(&lifetime_root) {
                    return Err(SessionError::InvalidInput(format!(
                        "checkpoint pins memory root {lifetime_root} unknown to the lifetime actor"
                    )));
                }
                if let Some(project_root) = project_memory_root
                    && !self
                        .memory_project
                        .as_ref()
                        .is_some_and(|a| a.contains_root(&project_root))
                {
                    return Err(SessionError::InvalidInput(format!(
                        "checkpoint pins project memory root {project_root} unknown to the project actor"
                    )));
                }
                kanbei_memory::MemoryFollowPolicy::PinnedAt {
                    lifetime_root,
                    project_root: project_memory_root,
                }
            }
            None => kanbei_memory::MemoryFollowPolicy::FollowHead,
        };

        // Quiesce BEFORE the transition commits: an active run is cancelled
        // (its `run_outcome Failed(Quiesced)` records the termination), then
        // the pending intents (any committed intent-kind event without its
        // outcome-kind event) become the cancelled list and tail intents with
        // an interrupted/ambiguous classification become the ambiguous list.
        // No `intent_classified` facts are committed here — the transition
        // event's listing is the record; a crash before the transition leaves
        // open()'s classification to handle it.
        if let Some(run_id) = self.scheduler.active_run() {
            let usage = self.scheduler.current_usage(run_id);
            let (record, _) = self.scheduler.record_outcome(
                run_id,
                kanbei_scheduler::TerminalOutcome::Failed(
                    kanbei_scheduler::FailureKind::Quiesced,
                ),
                usage,
                &[],
            )?;
            self.commit(
                vec![NewEvent {
                    kind: "run_outcome".into(),
                    payload_schema: 1,
                    payload: serde_json::to_value(&record).map_err(|e| {
                        SessionError::InvalidInput(format!("run outcome payload: {e}"))
                    })?,
                    objects: Vec::new(),
                    refs: Vec::new(),
                }],
                None,
            )?;
        }
        let cancelled: Vec<QuiescedIntent> = self
            .scan_pending_intents()?
            .into_iter()
            .map(|i| QuiescedIntent {
                seq: i.seq,
                kind: i.kind,
                id: i.id,
            })
            .collect();
        let transition_seq = self.next_seq;
        let ambiguous: Vec<QuiescedIntent> = self
            .scan_classified_intents()?
            .into_iter()
            .filter(|i| i.seq > checkpoint.seq && i.seq < transition_seq)
            .collect();
        let quiesce = QuiesceRecord { cancelled, ambiguous };

        let new_branch = BranchId::generate();
        // The config choice at the branch point: the live config manifest
        // digest (the package digest `activate_config` retained — the
        // canonical content digest), the checkpoint manifest's
        // `provider_config` pin (the historical choice), and the live epoch
        // composition. Config restoration is out of scope — the record is
        // the deliverable.
        let config_choice = ConfigChoiceRecord {
            mode: "Current".into(),
            current: self.config_digest,
            historical: manifest.provider_config,
            composition: Some(self.composition.current().digest),
        };
        self.fault(FaultPoint::BeforeBranchTransition);
        let receipt = self.commit(
            vec![NewEvent {
                kind: "branch_transition".into(),
                payload_schema: 1,
                payload: json!({
                    "branch": new_branch.to_string(),
                    "from_branch": self.branch.to_string(),
                    "frontier_seq": checkpoint.seq,
                    "checkpoint_event": env.evt,
                    "checkpoint_snapshot": snapshot.to_string(),
                    "follow": serde_json::to_value(&follow)
                        .expect("follow serialization cannot fail"),
                    "config_choice": serde_json::to_value(&config_choice)
                        .expect("config choice serialization cannot fail"),
                    "quiesce": serde_json::to_value(&quiesce)
                        .expect("quiesce serialization cannot fail"),
                    "memory_root": memory_root.map(|d| d.to_string()),
                    "project_memory_root": project_memory_root.map(|d| d.to_string()),
                }),
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            Some(self.composition.current().digest),
        )?;
        self.fault(FaultPoint::AfterBranchTransition);
        let record = BranchRecord {
            id: new_branch,
            from: Some(self.branch),
            frontier_seq: checkpoint.seq,
            transition_seq: receipt.last_seq,
            follow,
            config_choice,
            quiesce,
        };
        self.branch = new_branch;
        self.branch_records.push(record.clone());
        // Wave 2 consumes the pinned roots; None when the checkpoint pinned
        // no lifetime root.
        self.pinned_roots = memory_root.map(|lifetime| PinnedRoots {
            lifetime,
            project: project_memory_root,
        });
        Ok(record)
    }

    /// Switch the memory-follow policy (M6 wave 2): `FollowHead` releases the
    /// pinned roots (the projection resolves against the live actor heads
    /// again); `PinnedAt` pins the projection to the given roots — which must
    /// be roots the memory actors committed ([`MemoryRootActor::contains_root`]),
    /// else `InvalidInput` and no event. Commits one canonical
    /// `memory_follow_changed` record event (schema 1, `state_head` None).
    pub fn memory_follow(&mut self, policy: kanbei_memory::MemoryFollowPolicy) -> Result<(), SessionError> {
        match &policy {
            kanbei_memory::MemoryFollowPolicy::FollowHead => {}
            kanbei_memory::MemoryFollowPolicy::PinnedAt {
                lifetime_root,
                project_root,
            } => {
                if !self.memory_lifetime.contains_root(lifetime_root) {
                    return Err(SessionError::InvalidInput(format!(
                        "pinned lifetime root {lifetime_root} is not a committed root"
                    )));
                }
                if let Some(project_root) = project_root
                    && !self
                        .memory_project
                        .as_ref()
                        .is_some_and(|a| a.contains_root(project_root))
                {
                    return Err(SessionError::InvalidInput(format!(
                        "pinned project root {project_root} is not a committed root"
                    )));
                }
            }
        }
        let at = self.next_seq;
        self.commit(
            vec![NewEvent {
                kind: "memory_follow_changed".into(),
                payload_schema: 1,
                payload: json!({
                    "policy": serde_json::to_value(&policy)
                        .expect("follow policy serialization cannot fail"),
                    "at": at,
                }),
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            None,
        )?;
        self.pinned_roots = match policy {
            kanbei_memory::MemoryFollowPolicy::FollowHead => None,
            kanbei_memory::MemoryFollowPolicy::PinnedAt {
                lifetime_root,
                project_root,
            } => Some(PinnedRoots {
                lifetime: lifetime_root,
                project: project_root,
            }),
        };
        Ok(())
    }

    /// Whether `seq` is on the current branch's path: false exactly for the
    /// abandoned tails `(frontier_seq, transition_seq]` of every committed
    /// branch record. The checkpoint event at the frontier stays on-path; the
    /// `branch_transition` event itself is excluded from the new path.
    pub fn on_path(&self, seq: u64) -> bool {
        !self
            .branch_records
            .iter()
            .any(|r| r.frontier_seq < seq && seq <= r.transition_seq)
    }

    /// The current branch's on-path ranges (inclusive on both ends):
    /// `[1..=first.frontier]`, then per record
    /// `[records[i].transition + 1 ..= records[i+1].frontier]`, and
    /// `[last.transition + 1 ..= u64::MAX]` for the last — the transition
    /// event itself is off-path. No records → the whole seq space.
    pub fn path_ranges(&self) -> Vec<(u64, u64)> {
        if self.branch_records.is_empty() {
            return vec![(1, u64::MAX)];
        }
        let mut ranges = vec![(1, self.branch_records[0].frontier_seq)];
        for pair in self.branch_records.windows(2) {
            ranges.push((pair[0].transition_seq + 1, pair[1].frontier_seq));
        }
        let last = self.branch_records.last().expect("non-empty records");
        ranges.push((last.transition_seq + 1, u64::MAX));
        ranges
    }

    /// The post-event execution manifest for `state_head` and the committed
    /// payload schemas — the exact byte layout commit step 5 pins (and
    /// [`Session::create_checkpoint`] pre-computes for its payload).
    fn build_manifest(&self, state_head: Option<Digest>, payload_schemas: &[u32]) -> ExecutionManifest {
        let mut manifest = ExecutionManifest::bootstrap();
        manifest.state_head = state_head;
        manifest.modules = self
            .modules
            .as_ref()
            .map(|m| {
                m.snapshot()
                    .into_iter()
                    .map(
                        |(module_id, generation, package)| kanbei_snapshot::ModulePin {
                            module_id,
                            generation,
                            package,
                            // M2 activates root-scope modules only.
                            scope: "/".into(),
                        },
                    )
                    .collect()
            })
            .unwrap_or_default();
        manifest.composition = Some(self.composition.current().digest);
        manifest.engine_digest = self.vm_engine_digest;
        // R-11: model calls and consequential events pin the exact memory
        // roots at commit time.
        manifest.memory_root = self.memory_lifetime.head();
        manifest.project_memory_root = self.memory_project.as_ref().and_then(|a| a.head());
        // M6 wave 2: the tool-registry and provider-config pins are content
        // digests over the canonical bytes; the caller installs those bytes
        // before pinning (closure-valid, R-10). The scheduler policy name is
        // the canonical R-09/E-09 surface. `provider`/`policy`/`projection`
        // versions stay None — no versioned surfaces exist yet.
        manifest.tool_registry = Some(Digest::new(&self.tool_registry.to_canonical_bytes()));
        manifest.provider_config = self
            .provider_config
            .as_ref()
            .map(|cfg| Digest::new(&cfg.to_canonical_bytes()));
        manifest.scheduler_policy = Some(self.scheduler.policy_name().to_string());
        let mut schema_versions = payload_schemas.to_vec();
        schema_versions.push(kanbei_snapshot::MANIFEST_SCHEMA);
        schema_versions.sort_unstable();
        schema_versions.dedup();
        manifest.schema_versions = schema_versions;
        manifest
    }

    /// The canonical config-object bytes the manifest's `tool_registry` and
    /// `provider_config` digests reference — installed into the session store
    /// before the manifest is pinned so the snapshot closure verifies from
    /// the session store alone (R-10; content addressing dedups).
    fn manifest_config_objects(&self) -> Vec<Vec<u8>> {
        let mut objects = vec![self.tool_registry.to_canonical_bytes()];
        if let Some(cfg) = &self.provider_config {
            objects.push(cfg.to_canonical_bytes());
        }
        objects
    }

    /// The envelope at `seq`, scanning the log (M6 checkpoint validation).
    fn envelope_at(&self, seq: u64) -> Result<Envelope, SessionError> {
        let log_path = self.log_path.clone();
        let mut found: Option<Envelope> = None;
        kanbei_log::for_each_frame(&log_path, |info| {
            for line in &info.events {
                let Ok(env) = Envelope::from_line(line) else {
                    continue;
                };
                if env.seq == seq {
                    found = Some(env);
                    return;
                }
            }
        })?;
        found.ok_or_else(|| SessionError::InvalidInput(format!("no event at seq {seq}")))
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
                match env.kind.as_str() {
                    "tool_intent" => {
                        if let (Some(call), Some(tool)) = (
                            env.payload.get("call_id").and_then(|c| c.as_str()),
                            env.payload.get("tool").and_then(|t| t.as_str()),
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
                        if let Some(call) = env.payload.get("call_id").and_then(|c| c.as_str()) {
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
                match env.kind.as_str() {
                    "tool_intent" => {
                        if let Some(call) = env.payload.get("call_id").and_then(|c| c.as_str()) {
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
                            env.payload.get("call_id").and_then(|c| c.as_str()),
                            env.payload.get("classification").and_then(|c| c.as_str()),
                        ) {
                            if let Some(entry) = by_call.get_mut(call) {
                                entry.1 = Some(class.to_string());
                            }
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

    /// THE atomic config reload (R-01/C-02): activates the manifest's module
    /// (its `kb_on_activate` publishes services via host op 6 into the shared
    /// registry), collects the registry delta (services published by the new
    /// generation), stages it (removed from the shared registry so
    /// validate/apply run against the pre-activation state), validates the
    /// contribution set, OCC-publishes it against the epoch captured before
    /// activation, and commits one canonical `composition_changed` event
    /// (pre-event snapshot = old manifest; payload = epoch delta, scope,
    /// initiator; R-01/C-01). The post-event manifest pins the new
    /// composition (the state change), so the epoch digest enters the
    /// execution-snapshot manifest.
    ///
    /// On failure at any step nothing is committed and the last valid
    /// composition is retained; the module is deactivated (removing its
    /// registrations) whenever it had been activated. If the commit itself
    /// fails after the in-memory publish, the module is deactivated too and
    /// the in-memory composition is ahead of the log — M2 documents this
    /// divergence: the log is the authority at restart.
    /// Mark the UI stale (R-27 fault class 1: composition failure → the
    /// last-valid UI with a staleness banner). No-op without a bound UI.
    fn ui_mark_stale(&mut self, reason: &str) {
        if let Some(host) = self.ui_host.as_mut() {
            host.staleness = Some(reason.to_string());
        }
    }

    pub fn activate_config(
        &mut self,
        manifest: PackageManifest,
    ) -> Result<ConfigActivation, SessionError> {
        self.fault(FaultPoint::BeforeConfigActivation);
        // OCC: capture the current epoch before the (potentially long)
        // activation so a stale staged set can never publish.
        let staged = self.composition.stage(Vec::new());
        let Some(manager) = self.modules.as_mut() else {
            return Err(SessionError::ModulesDisabled);
        };
        // 4 — activate; kb_on_activate publishes into the shared registry.
        let generation = manager.activate(&manifest)?;
        // The delta: every service this generation published.
        let delta: Vec<(ServiceKey, ServiceProvider)> = self
            .services
            .lock()
            .expect("services lock poisoned")
            .snapshot()
            .into_iter()
            .filter(|(_, p, _)| p.generation == generation.generation)
            .map(|(k, p, _)| (k, p))
            .collect();
        // Stage: pull the delta back out of the shared registry so validate
        // and apply run against the pre-activation state (the delta IS the
        // staged set; without this, the module's own publications would
        // self-conflict on the re-publish).
        {
            let mut reg = self.services.lock().expect("services lock poisoned");
            for (key, provider) in &delta {
                if let Err(e) = reg.remove(key, provider.module_id) {
                    drop(reg);
                    let _ = manager.deactivate(manifest.module_id);
                    return Err(e.into());
                }
            }
        }
        let mut staged = staged;
        staged.contributions = delta
            .iter()
            .map(|(key, provider)| Contribution {
                scope: manifest.scope.clone(),
                kind: ContributionKind::Service(ServiceContribution {
                    key: key.clone(),
                    provider: provider.clone(),
                    deps: manifest.deps.clone(),
                }),
            })
            .collect();
        // M5: non-service contributions staged via `contribution_publish`
        // (UI mounts, theme overlays) join the same atomic publish.
        staged
            .contributions
            .extend(manager.published_contributions(generation.generation));
        // 6 — validate against the current composition; on conflict roll back.
        if let Err(e) = self.registry.validate(&staged.contributions) {
            let reason = e.to_string();
            let _ = manager.deactivate(manifest.module_id);
            self.ui_mark_stale(&reason);
            return Err(e.into());
        }
        // 7 — OCC publish; stale → roll back.
        if let Err(e) = self.composition.publish(&staged, &mut self.registry) {
            let reason = e.to_string();
            let _ = manager.deactivate(manifest.module_id);
            self.ui_mark_stale(&reason);
            return Err(e.into());
        }
        self.fault(FaultPoint::AfterConfigActivation);
        // 9 — commit the canonical event. The composition's canonical bytes
        // are pinned as an object (its digest = the epoch digest, so the ref
        // is closure-valid) and the event references the package + composition
        // digests. state_head = composition digest → the manifest pins it.
        let epoch = self.composition.current().epoch;
        let package = generation.package;
        let composition_digest = self.composition.current().digest;
        let comp_bytes = self.composition.current().to_canonical_bytes();
        self.store.install(&comp_bytes)?;
        let receipt = self.commit(
            vec![NewEvent {
                kind: "composition_changed".into(),
                payload_schema: 1,
                payload: json!({
                    "epoch": epoch,
                    "delta": {
                        "added": [{
                            "module_id": manifest.module_id.to_string(),
                            "generation": generation.generation,
                            "package": package.to_string(),
                        }],
                        "removed": [],
                    },
                    "scope": manifest.scope.to_string(),
                    "initiator": "config",
                }),
                objects: Vec::new(),
                refs: vec![package, composition_digest],
            }],
            Some(composition_digest),
        );
        if let Err(e) = receipt {
            if let Some(m) = self.modules.as_mut() {
                let _ = m.deactivate(manifest.module_id);
            }
            self.ui_mark_stale(&e.to_string());
            return Err(e);
        }
        self.config_digest = Some(package);
        Ok(ConfigActivation {
            module_id: manifest.module_id,
            generation: generation.generation,
            epoch,
            event_seq: receipt.unwrap().last_seq,
        })
    }

    /// Generation replacement through the session: captures the old
    /// generation's registry entries (M2: services published by its
    /// generation), replaces via the manager, stages the new generation's
    /// entries, validates + OCC-publishes the new contribution set, and
    /// commits a `composition_changed` event whose delta records the removed
    /// old generation and the added new one.
    ///
    /// The manager's `replace` is not rollback-atomic: on error the session
    /// returns unchanged (no event, epoch untouched). Best-effort rollback:
    /// when the swap had already happened (the old generation is no longer
    /// current), the new generation is deactivated if possible — a
    /// `RestartFailed` replacement leaves the new generation active with the
    /// composition listing the old providers (M2 documented divergence).
    pub fn replace_module(
        &mut self,
        module_id: Id128,
        new_manifest: PackageManifest,
    ) -> Result<ReplacementOutcome, SessionError> {
        let staged = self.composition.stage(Vec::new());
        let Some(manager) = self.modules.as_mut() else {
            return Err(SessionError::ModulesDisabled);
        };
        let (old_generation, old_package) = manager
            .snapshot()
            .into_iter()
            .find(|(id, _, _)| *id == module_id)
            .map(|(_, g, pkg)| (g, pkg))
            .ok_or(ModuleError::NotActivated { module_id })?;
        let old_entries: Vec<(ServiceKey, ServiceProvider)> = self
            .services
            .lock()
            .expect("services lock poisoned")
            .snapshot()
            .into_iter()
            .filter(|(_, p, _)| p.generation == old_generation)
            .map(|(k, p, _)| (k, p))
            .collect();
        let outcome = match manager.replace(module_id, &new_manifest) {
            Ok(outcome) => outcome,
            Err(e) => {
                // Best effort: pre-swap failures leave the old generation
                // current (activate rolls back its own registration) — keep
                // it. Post-swap failures (RestartFailed) leave the new
                // generation current; try to remove it.
                if !manager.generation_current(old_generation) {
                    let _ = manager.deactivate(module_id);
                }
                return Err(e.into());
            }
        };
        let new_generation = outcome.new.generation;
        let new_package = outcome.new.package;
        let new_entries: Vec<(
            ServiceKey,
            ServiceProvider,
            Vec<kanbei_services::ServiceDependency>,
        )> = self
            .services
            .lock()
            .expect("services lock poisoned")
            .snapshot()
            .into_iter()
            .filter(|(_, p, _)| p.generation == new_generation)
            .collect();
        // Stage: pull the new generation's publications out so validate/apply
        // run against the pre-replace state.
        {
            let mut reg = self.services.lock().expect("services lock poisoned");
            for (key, provider, _) in &new_entries {
                if let Err(e) = reg.remove(key, provider.module_id) {
                    drop(reg);
                    let _ = manager.deactivate(module_id);
                    return Err(e.into());
                }
            }
        }
        let mut staged = staged;
        staged.contributions = new_entries
            .iter()
            .map(|(key, provider, deps)| Contribution {
                scope: new_manifest.scope.clone(),
                kind: ContributionKind::Service(ServiceContribution {
                    key: key.clone(),
                    provider: provider.clone(),
                    deps: deps.clone(),
                }),
            })
            .collect();
        if let Err(e) = self.registry.validate(&staged.contributions) {
            let _ = manager.deactivate(module_id);
            return Err(e.into());
        }
        if let Err(e) = self.composition.publish(&staged, &mut self.registry) {
            let _ = manager.deactivate(module_id);
            return Err(e.into());
        }
        let epoch = self.composition.current().epoch;
        let composition_digest = self.composition.current().digest;
        let comp_bytes = self.composition.current().to_canonical_bytes();
        self.store.install(&comp_bytes)?;
        let receipt = self.commit(
            vec![NewEvent {
                kind: "composition_changed".into(),
                payload_schema: 1,
                payload: json!({
                    "epoch": epoch,
                    "delta": {
                        "added": [{
                            "module_id": module_id.to_string(),
                            "generation": new_generation,
                            "package": new_package.to_string(),
                            "keys": new_entries.iter().map(|(k, _, _)| k.to_string()).collect::<Vec<_>>(),
                        }],
                        "removed": [{
                            "module_id": module_id.to_string(),
                            "generation": old_generation,
                            "package": old_package.to_string(),
                            "keys": old_entries.iter().map(|(k, _)| k.to_string()).collect::<Vec<_>>(),
                        }],
                    },
                    "scope": "/",
                    "initiator": "config",
                }),
                objects: Vec::new(),
                refs: vec![old_package, new_package, composition_digest],
            }],
            Some(composition_digest),
        );
        if let Err(e) = receipt {
            if let Some(m) = self.modules.as_mut() {
                let _ = m.deactivate(module_id);
            }
            return Err(e);
        }
        Ok(outcome)
    }

    /// Kernel-side effect dispatch (R-16/D-11): checks the caller
    /// generation's currency, then routes the call through the module host's
    /// `service_call` machinery (host op 3 — resolves the key against the
    /// shared registry with the caller's declared dependency version and
    /// scope, then runs the provider generation's `kb_hot`). The session does
    /// not own the broker (the `ModuleHost` does); broker-gated dispatch-time
    /// re-verification is exercised in the testkit via host op 4 — M2
    /// scoping. `args` must be a JSON value.
    pub fn effect_dispatch(
        &mut self,
        key: &ServiceKey,
        args: &str,
        caller_generation: u64,
    ) -> Result<String, SessionError> {
        self.fault(FaultPoint::BeforeEffectDispatch);
        let Some(manager) = self.modules.as_ref() else {
            return Err(SessionError::ModulesDisabled);
        };
        // Displaced generations cannot dispatch effects (R-02/C-03).
        if !manager.generation_current(caller_generation) {
            return Err(SessionError::StaleGeneration {
                generation: caller_generation,
            });
        }
        let payload = json!({
            "key": key,
            "args": serde_json::from_str::<serde_json::Value>(args)
                .map_err(|e| SessionError::Effect(format!("args are not JSON: {e}")))?,
        })
        .to_string();
        let result = manager
            .host()
            .call(caller_generation, 3, &payload)
            .map_err(SessionError::Effect)?;
        self.fault(FaultPoint::AfterEffectDispatch);
        Ok(result)
    }

    /// Module-state head CAS through the session actor only (R-07/B-01/F2):
    /// the head update is a kernel command, never a direct store write.
    pub fn module_state_cas(
        &mut self,
        key: &str,
        schema: u32,
        bytes: Vec<u8>,
        generation: u64,
    ) -> Result<HeadFile, SessionError> {
        self.fault(FaultPoint::BeforeHeadUpdate);
        let Some(manager) = self.modules.as_ref() else {
            return Err(SessionError::ModulesDisabled);
        };
        let state = manager.state();
        let head = state
            .lock()
            .expect("state lock poisoned")
            .cas(StateUpdate {
                key: key.into(),
                schema,
                bytes,
                generation,
            })?;
        self.fault(FaultPoint::AfterHeadUpdate);
        Ok(head)
    }

    /// Retention admission (architecture.md line 604): the gate runs BEFORE
    /// storage receives any bytes — the candidate never touches the log or
    /// the object store. A non-resumable boundary or a rejection commits a
    /// canonical `retention_boundary` fact (pure event); stored/dropped
    /// candidates commit nothing.
    pub fn retain_candidate(&mut self, candidate: Candidate) -> Result<Admission, SessionError> {
        let admission = self.policy.admit(candidate)?;
        if let Some(fact) = self.policy.boundary_fact(&admission) {
            self.commit(
                vec![NewEvent {
                    kind: "retention_boundary".into(),
                    payload_schema: 1,
                    payload: json!({
                        "reason": fact.reason,
                        "replay_relevant": fact.replay_relevant,
                        "kind": match fact.kind {
                            BoundaryKind::NonResumable => "non_resumable",
                            BoundaryKind::Rejected => "rejected",
                        },
                    }),
                    objects: Vec::new(),
                    refs: Vec::new(),
                }],
                None,
            )?;
        }
        Ok(admission)
    }

    /// The module subsystem; None when modules are disabled (guest wasm not
    /// built or safe mode).
    pub fn modules(&self) -> Option<&ModuleManager> {
        self.modules.as_ref()
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

    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    pub fn current_snapshot(&self) -> Option<Digest> {
        self.current_snapshot
    }

    /// The session's own identity (caller principal for kernel-originated
    /// tool calls, R-14/D-02).
    pub fn session_id(&self) -> Id128 {
        self.session_id
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
}

// ---------- helpers ----------

/// `recover` errors on a missing file; a fresh dir is a valid genesis state.
fn recover_or_fresh(log_path: &Path) -> Result<Recovered, SessionError> {
    match std::fs::metadata(log_path) {
        Ok(m) if m.is_file() => Ok(kanbei_log::recover(log_path)?),
        Ok(_) => Err(SessionError::InvalidInput(format!(
            "log path is not a file: {}",
            log_path.display()
        ))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Recovered {
            events: 0,
            frames: 0,
            truncated: false,
            last_seq: 0,
        }),
        Err(e) => Err(e.into()),
    }
}

/// Best-effort worker cleanup on a failed open, when no other Arc clones
/// exist. Only reachable while returning an error, so a secondary shutdown
/// failure is not propagated.
fn shutdown_queue(queue: Arc<DurabilityQueue>) {
    if let Ok(queue) = Arc::try_unwrap(queue) {
        let _ = queue.shutdown();
    }
}

// M3 agent spine: run lifecycle, model/tool commit paths, approvals, breakers,
// and interrupted/ambiguous classification (spine.rs).
mod spine;
