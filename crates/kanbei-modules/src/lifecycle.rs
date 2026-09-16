//! The M2 unified module lifecycle (architecture.md "Unified module
//! lifecycle"): stable ModuleId + immutable package hash + ephemeral
//! GenerationId; activation/disposal; generation replacement with
//! stale-effect rejection; and the `StateStore` generation-currency callback
//! source.
//!
//! # Activation entry
//!
//! The guest caches exactly one callable entry (`kb_hot`, see kanbei-vm's
//! `call_json`), so the kernel cannot invoke a second source-defined global
//! directly. M2 runs the activation entry through `Instance::run_script` with
//! [`ACTIVATION_SHIM`] appended to the module source: the script executes in
//! the generation's sandbox (same store, same generation token), builds the
//! `ctx` handle over `kb_host_call`, and calls the module's
//! `kb_on_activate(ctx)`. Host calls made by the activation entry are routed
//! through the same dispatcher as any other call. This is a documented
//! deviation from `call_json("kb_on_activate", "{}")` (which kanbei-vm
//! rejects); the observable contract is unchanged. Because the shim re-runs
//! the source in a throwaway VM, module top-level code must be pure — it runs
//! once in the cached `kb_hot` VM and once in the activation VM.
//!
//! # Disposal drain (R-24/C-04)
//!
//! M2 has no cancellable effects, so the drain protocol is a documented stub:
//! quiesce (no-op) → bounded deadline (0 elapsed) → force-terminate (drop the
//! Wasm store). The `forced` flag on [`DisposalRecord`] (the `cleanup_forced`
//! fact shape) is always false in M2 — the routine drop IS the force, and it
//! cannot fail.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};
use std::time::{Duration, Instant};

use kanbei_capabilities::BrokerError;
use kanbei_core::id::Id128;
use kanbei_core::Digest;
use kanbei_objects::ObjectError;
use kanbei_scopes::contrib::HookKind;
use kanbei_services::{
    replacement, ReplaceIntent, ScopePath, ServiceDependency, ServiceError, ServiceKey,
    ServiceProvider, ServiceRegistry,
};
use kanbei_vm::{GuestError, Vm};
use thiserror::Error;

use crate::host::{ModuleHost, TokenInfo};
use crate::package::{install_package, ModuleOrigin, PackageManifest, PackageStore};
use crate::runtime::{DRAIN_DEADLINE, GenerationRuntime, REPLY_TIMEOUT, Scope};
use crate::state::{StateError, StateStore};

/// The Luau activation shim: builds the `ctx` handle over `kb_host_call` and
/// invokes the module's `kb_on_activate(ctx)`. Appended to the module source
/// and executed via `run_script` (see the module docs). Internal/unstable
/// ABI. `service_publish` is op 6; the ops are documented on
/// [`ModuleHost`].
///
/// T9: after activation it publishes one `hook` contribution per declared hook
/// function (`kb_on_turn_start` / `kb_on_tool_intent`). The functions
/// themselves are dispatched by the `hot_multiplexer` wrapper.
pub const ACTIVATION_SHIM: &str = r#"
-- kanbei-modules M2 activation shim (internal/unstable ABI).
local __kb_json = function(s)
  s = tostring(s)
  return '"' .. s:gsub('[\\"\n\r\t]', { ['\\'] = '\\\\', ['"'] = '\\"', ['\n'] = '\\n', ['\r'] = '\\r', ['\t'] = '\\t' }) .. '"'
end
local __ctx = {}
function __ctx.log(msg) return kb_host_call(0, '{"msg":' .. __kb_json(msg) .. '}') end
function __ctx.state_get(key) return kb_host_call(1, '{"key":' .. __kb_json(key) .. '}') end
function __ctx.state_set(key, schema, value)
  return kb_host_call(2, '{"key":' .. __kb_json(key) .. ',"schema":' .. tostring(schema) .. ',"value":' .. tostring(value) .. '}')
end
function __ctx.service_call(key, args)
  return kb_host_call(3, '{"key":' .. tostring(key) .. ',"args":' .. tostring(args) .. '}')
end
function __ctx.check(resource, verbs)
  return kb_host_call(4, '{"resource":' .. __kb_json(resource) .. ',"verbs":' .. tostring(verbs) .. '}')
end
function __ctx.require_approval(resource, verbs)
  return kb_host_call(5, '{"resource":' .. __kb_json(resource) .. ',"verbs":' .. tostring(verbs) .. '}')
end
function __ctx.service_publish(key, version, deps)
  return kb_host_call(6, '{"key":' .. tostring(key) .. ',"version":' .. tostring(version) .. ',"deps":' .. tostring(deps or "[]") .. '}')
end
-- M5: stage a contribution (UI mount / theme overlay); payload is the full
-- contribution JSON object (see kanbei-modules host op 7).
function __ctx.contribution_publish(payload)
  return kb_host_call(7, tostring(payload))
end
if type(kb_on_activate) ~= "function" then
  kb_host_call(0, '{"msg":"activation: module source does not define kb_on_activate(ctx)"}')
  error("kb_on_activate is not a function")
end
kb_on_activate(__ctx)
-- T9: declare each hook the module defined. `kb_name`, when the module sets
-- it, is the stable contribution name; otherwise the host fills the module id
-- (so two modules hooking the same kind never collide).
local function __kb_publish_hook(fn, hook)
  if type(fn) == "function" then
    local name = type(kb_name) == "string" and kb_name or ""
    __ctx.contribution_publish('{"kind":"hook","name":' .. __kb_json(name) .. ',"hook":"' .. hook .. '"}')
  end
end
__kb_publish_hook(kb_on_turn_start, "on_turn_start")
__kb_publish_hook(kb_on_tool_intent, "on_tool_intent")
"#;

/// T9 hook multiplexer: the guest caches exactly one callable entry (`kb_hot`)
/// and `Instance::call_json_inner` rejects every other name, so named hook
/// entry points are dispatched through a `kb_hot` wrapper. The wrapper is
/// installed ONLY when the module declares at least one hook function, so a
/// plain module's `kb_hot` is byte-identical to before. A kernel-initiated
/// hook call arrives on `kb_hot` as the envelope
/// `{"__kb_hook":"on_turn_start"|"on_tool_intent","__kb_nonce":<secret>,
/// "context":<value>}`; anything else — including a peer module's
/// `service_call` that guesses the shape but does not know the per-generation
/// secret — falls through to the module's original `kb_hot` (D).
///
/// The nonce is embedded per activation by [`hot_multiplexer`] into the
/// compiled source (both the kernel instance and the discovery VM shim), so
/// only the kernel and that generation's VM share it. Internal/unstable ABI.
const HOT_MULTIPLEXER_TEMPLATE: &str = r#"
-- kanbei-modules T9 hook multiplexer (internal/unstable ABI).
local __kb_orig_hot = kb_hot
local __kb_hook_nonce = "__KB_NONCE__"
if type(kb_on_turn_start) == "function" or type(kb_on_tool_intent) == "function" then
  kb_hot = function(x)
    if type(x) == "table" and x.__kb_nonce == __kb_hook_nonce then
      if x.__kb_hook == "on_turn_start" and type(kb_on_turn_start) == "function" then
        return kb_on_turn_start(x.context)
      elseif x.__kb_hook == "on_tool_intent" and type(kb_on_tool_intent) == "function" then
        return kb_on_tool_intent(x.context)
      end
    end
    return __kb_orig_hot(x)
  end
end
"#;

/// The hook multiplexer with this generation's secret nonce embedded (D). The
/// nonce is a fresh `Id128` string per activation, never reused.
fn hot_multiplexer(nonce: &str) -> String {
    HOT_MULTIPLEXER_TEMPLATE.replace("__KB_NONCE__", nonce)
}

/// Reply bound for a kernel-initiated hook call (T9). Deliberately short
/// (order 100–250ms) and NOT the 10s [`REPLY_TIMEOUT`]: hooks are advisory
/// lifecycle seams, so a wedged actor must not stall the kernel — the call
/// returns within the bound and the session classifies the loss.
pub const HOOK_WAIT: Duration = Duration::from_millis(200);

/// Respawn-time activation bound (NEW-3). A hook-fault respawn re-runs the
/// module's `kb_on_activate`; without this bound that wedge would wait the full
/// 10s [`REPLY_TIMEOUT`], so a hook whose activation wedges would stall the
/// decision for seconds and violate "never blocks the run". Bounded to the hook
/// budget, it degrades within the same per-decision window.
const RESPAWN_ACTIVATION_WAIT: Duration = HOOK_WAIT;

/// The manager's per-module bookkeeping, shared with [`Generation`] so a
/// direct `Generation::dispose` deregisters consistently (a disposed
/// generation must not appear in `current` or the manifest snapshot).
#[derive(Default)]
struct LifecycleTables {
    current: HashMap<Id128, u64>,
    /// generation → vm token. The vm token equals the generation id (both are
    /// fresh, never-reused counters).
    generation_token: HashMap<u64, u64>,
    packages: HashMap<u64, Digest>,
    /// generation → per-generation hook secret nonce (D). Embedded in the
    /// compiled multiplexer; the kernel presents it on every hook envelope so a
    /// peer module's guessed `service_call` cannot hijack a hook.
    hook_nonces: HashMap<u64, String>,
}

/// A live module generation. The generation's Wasmtime store is owned by a
/// dedicated actor thread ([`GenerationRuntime`]); the kernel's generation
/// table holds an `Arc` to the same actor, so service routing reaches it. The
/// store is dropped on that thread when the actor shuts down.
pub struct Generation {
    pub generation: u64,
    pub module_id: Id128,
    /// Immutable package digest.
    pub package: Digest,
    /// The generation's store-owning actor (custody boundary); `dispose` drains
    /// and joins it.
    pub runtime: Arc<GenerationRuntime>,
    pub scope: ScopePath,
    instances: Arc<Mutex<HashMap<u64, Arc<GenerationRuntime>>>>,
    tables: Arc<Mutex<LifecycleTables>>,
    /// The kernel host, so direct disposal can retire the generation's published
    /// effects through the same path as the vm's forced retirement.
    host: Arc<ModuleHost>,
}

/// The `cleanup_forced` fact shape (R-24/C-04).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisposalRecord {
    pub generation: u64,
    pub forced: bool,
    pub reason: String,
}

/// How a generation's actor ended during a drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Drain {
    /// The actor exited and was joined within the deadline.
    Joined,
    /// The actor was still executing past the deadline; detached and recorded
    /// in the shared abandoned-drain counter.
    Detached,
    /// The vm had already retired the generation (its actor was removed from
    /// the table and a non-blocking stop requested), so this drain could not
    /// join it.
    AlreadyRetired,
    /// The actor thread panicked; its store was dropped during unwinding, so
    /// this is not a leak, but it is not a clean quiesce either.
    Panicked,
}

impl Drain {
    /// Drain a generation's actor, if the table still holds it, within the
    /// standard [`DRAIN_DEADLINE`].
    fn of(runtime: Option<Arc<GenerationRuntime>>) -> Self {
        Self::of_with(runtime, DRAIN_DEADLINE)
    }

    /// As [`Self::of`], with an explicit drain deadline (the bounded decision
    /// path passes its remaining budget, G).
    fn of_with(runtime: Option<Arc<GenerationRuntime>>, deadline: Duration) -> Self {
        match runtime {
            None => Drain::AlreadyRetired,
            Some(runtime) => {
                let joined = runtime.shutdown(deadline);
                if runtime.panicked() {
                    Drain::Panicked
                } else if joined {
                    Drain::Joined
                } else {
                    Drain::Detached
                }
            }
        }
    }

    /// Build the disposal record for a drain of `generation` described by
    /// `verb` (e.g. "dispose", "deactivation", "replacement").
    fn record(self, generation: u64, verb: &str) -> DisposalRecord {
        match self {
            Drain::Joined => DisposalRecord {
                generation,
                forced: false,
                reason: format!("{verb}: actor quiesced and joined"),
            },
            Drain::Detached => DisposalRecord {
                generation,
                forced: true,
                reason: format!(
                    "{verb}: actor wedged past the drain deadline; thread detached (abandoned-drain counter incremented)"
                ),
            },
            Drain::AlreadyRetired => DisposalRecord {
                generation,
                forced: false,
                reason: format!(
                    "{verb}: the vm had already retired this generation (non-blocking stop requested; not joined by this drain)"
                ),
            },
            Drain::Panicked => DisposalRecord {
                generation,
                forced: false,
                reason: format!("{verb}: actor panicked during shutdown; its store was dropped"),
            },
        }
    }
}

impl std::fmt::Debug for Generation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Generation")
            .field("generation", &self.generation)
            .field("module_id", &self.module_id)
            .field("package", &self.package)
            .field("scope", &self.scope)
            .finish_non_exhaustive()
    }
}

impl Generation {
    /// Force-terminate: drain the generation's actor (quiesce → deadline →
    /// force), invalidate the generation token, unpublish its effects, drop the
    /// kernel's handle, and drop this handle. The store is dropped on the actor
    /// thread; a wedged actor is detached and reported as `forced`.
    pub fn dispose(self) -> DisposalRecord {
        // Canonical teardown (T7): invalidate the token, unpublish effects, then
        // drop the kernel's handles and drain the actor — never holding a kernel
        // lock across the mailbox send/join. Token-first is load-bearing (the
        // T18 fence rejects in-flight mutating ops once the token is gone).
        self.host.teardown_generation(self.generation, true);
        self.instances
            .lock()
            .expect("instances lock poisoned")
            .remove(&self.generation);
        {
            let mut tables = self.tables.lock().expect("lifecycle tables lock poisoned");
            tables.generation_token.remove(&self.generation);
            tables.packages.remove(&self.generation);
            tables.hook_nonces.remove(&self.generation);
            tables.current.retain(|_, g| *g != self.generation);
        }
        Drain::of(Some(Arc::clone(&self.runtime))).record(self.generation, "dispose")
    }
}

/// The result of a generation replacement (R-25/C-05): the old disposal
/// record, the new generation, and the version-compatible dependents that
/// rebind (`rebind`) vs. must restart (`restart`).
pub struct ReplacementOutcome {
    pub old: DisposalRecord,
    pub new: Generation,
    pub rebind: Vec<ServiceKey>,
    pub restart: Vec<ServiceKey>,
}

impl std::fmt::Debug for ReplacementOutcome {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReplacementOutcome")
            .field("old", &self.old)
            .field("new", &self.new)
            .field("rebind", &self.rebind)
            .field("restart", &self.restart)
            .finish()
    }
}

/// The M2 module lifecycle owner: owns the vm, the package store, the state
/// store, and the service registry, and shares the token/instance tables with
/// the kernel [`ModuleHost`]. Single-threaded (the session actor).
pub struct ModuleManager {
    vm: Vm,
    packages: PackageStore,
    state: Arc<Mutex<StateStore>>,
    services: Arc<Mutex<ServiceRegistry>>,
    host: Arc<ModuleHost>,
    next_generation: u64,
    tables: Arc<Mutex<LifecycleTables>>,
    tokens: Arc<RwLock<HashMap<u64, TokenInfo>>>,
    instances: Arc<Mutex<HashMap<u64, Arc<GenerationRuntime>>>>,
    rejected_stale_effects: Arc<AtomicU64>,
    /// Abandoned-drain counter: drains that gave up on an actor within the
    /// deadline and detached it (shared with every generation's actor).
    leaked_threads: Arc<AtomicU64>,
}

impl ModuleManager {
    pub fn new(
        vm: Vm,
        packages: PackageStore,
        state: StateStore,
        services: Arc<Mutex<ServiceRegistry>>,
    ) -> Result<Self, ModuleError> {
        // Rebind the state store's generation-currency callback to this
        // manager's token table: the session cannot reference the manager
        // before it exists, so any callback passed to `StateStore::open` is a
        // placeholder until here. The session dir, queue, and size limit are
        // preserved.
        let tokens: Arc<RwLock<HashMap<u64, TokenInfo>>> = Arc::new(RwLock::new(HashMap::new()));
        let currency: Arc<dyn Fn(u64) -> bool + Send + Sync> = {
            let tokens = Arc::clone(&tokens);
            Arc::new(move |g| tokens.read().expect("tokens lock poisoned").contains_key(&g))
        };
        let max_state_bytes = state.max_state_bytes();
        let mut state = StateStore::open(state.dir(), state.queue(), Arc::clone(&currency));
        state.set_max_state_bytes(max_state_bytes);
        let state = Arc::new(Mutex::new(state));
        let instances: Arc<Mutex<HashMap<u64, Arc<GenerationRuntime>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let rejected_stale_effects = Arc::new(AtomicU64::new(0));
        let leaked_threads = Arc::new(AtomicU64::new(0));
        let tables = Arc::new(Mutex::new(LifecycleTables::default()));
        let host = Arc::new(ModuleHost::new(
            // The session lane binds the real session id via
            // `ModuleManager::set_session`; until then capabilities use a
            // placeholder (M2 capability tests do not need a real session).
            Id128::generate(),
            Arc::clone(&tokens),
            Arc::downgrade(&instances),
            Arc::clone(&services),
            Arc::clone(&state),
            Arc::clone(&rejected_stale_effects),
            currency,
        ));
        Ok(Self {
            vm,
            packages,
            state,
            services,
            host,
            next_generation: 1,
            tables,
            tokens,
            instances,
            rejected_stale_effects,
            leaked_threads,
        })
    }

    /// Binds the kernel session identity used in capability principals. The
    /// session lane calls this once after construction.
    pub fn set_session(&mut self, session: Id128) {
        self.host.set_session(session);
    }

    pub fn host(&self) -> Arc<ModuleHost> {
        Arc::clone(&self.host)
    }

    /// blake3 digest of the embedded guest wasm (the session pins it as the
    /// manifest's `engine_digest`; R-08/E-12).
    pub fn engine_digest(&self) -> Digest {
        self.vm.engine_digest()
    }

    /// The shared state store (session-lane seam: head reads, `heads()` for
    /// manifests, pinning).
    pub fn state(&self) -> Arc<Mutex<StateStore>> {
        Arc::clone(&self.state)
    }

    /// The shared service registry (session-lane seam: manifest snapshots).
    pub fn services(&self) -> Arc<Mutex<ServiceRegistry>> {
        Arc::clone(&self.services)
    }

    /// Effects rejected because their generation token was stale
    /// (displacement rejection, R-02/C-03; the host collects them).
    pub fn rejected_stale_effects(&self) -> u64 {
        self.rejected_stale_effects.load(Ordering::Relaxed)
    }

    /// Count of drains that gave up on an actor past their deadline (the shared
    /// abandoned-drain counter). This is an event counter, not a live-leak
    /// gauge: an abandoned actor may still exit later, and the count is never
    /// decremented. A persistently rising value is the signal to investigate.
    pub fn leaked_threads(&self) -> u64 {
        self.leaked_threads.load(Ordering::Relaxed)
    }

    /// R-07/C-F1: activation-time state-schema validation. When the manifest
    /// binds a state key with a declared schema, the existing head for that key
    /// must match — a mismatch rejects activation atomically with a typed
    /// [`StateError::SchemaMismatch`] and the old head stays untouched.
    /// `state_key` and `state_schema` are set together (or neither).
    fn validate_state_schema(
        state: &Mutex<StateStore>,
        manifest: &PackageManifest,
    ) -> Result<(), ModuleError> {
        match (&manifest.state_key, manifest.state_schema) {
            (Some(key), Some(expected)) => {
                let store = state.lock().expect("state lock poisoned");
                if let Some((head, _)) = store.get(key)?
                    && head.schema != expected
                {
                    return Err(StateError::SchemaMismatch {
                        key: key.clone(),
                        expected,
                        actual: head.schema,
                    }
                    .into());
                }
                Ok(())
            }
            // No binding declared: nothing to validate (M2's per-write CAS still
            // enforces continuity against the head).
            (None, None) => Ok(()),
            _ => Err(ModuleError::InvalidInput(
                "state_key and state_schema must be set together (R-07/C-F1)".into(),
            )),
        }
    }

    /// Installs the package (content-deduped), compiles the source, assigns a
    /// fresh generation id (= vm token), instantiates, registers, and runs the
    /// activation entry. On any failure nothing is registered (fail-closed).
    pub fn activate(&mut self, manifest: &PackageManifest) -> Result<Generation, ModuleError> {
        self.activate_with_supersede(manifest, &HashSet::new())
    }

    /// As [`Self::activate`], with a precedence-driven supersede set: the
    /// generation is allowed to take over `supersede`'s service keys from a
    /// DIFFERENT, lower-precedence active config layer (decision 28). Callers
    /// that do not resolve precedence pass the empty set.
    pub fn activate_with_supersede(
        &mut self,
        manifest: &PackageManifest,
        supersede: &HashSet<ServiceKey>,
    ) -> Result<Generation, ModuleError> {
        self.activate_bounded(manifest, supersede, REPLY_TIMEOUT)
    }

    /// As [`Self::activate_with_supersede`], but the activation entry
    /// (`kb_on_activate`) waits at most `activation_wait` for the actor, and the
    /// rollback drain is bounded by the same span (NEW-3: a hook-fault respawn
    /// must not stall a decision for the full reply timeout).
    fn activate_bounded(
        &mut self,
        manifest: &PackageManifest,
        supersede: &HashSet<ServiceKey>,
        activation_wait: Duration,
    ) -> Result<Generation, ModuleError> {
        // R-07/C-F1: validate the declared state schema against the existing
        // head BEFORE any side effect, so an incompatible generation is rejected
        // atomically and the old head (and object store) stay untouched.
        Self::validate_state_schema(&self.state, manifest)?;
        let (package, _deduped) = install_package(&mut self.packages, manifest)?;
        // D: fresh per-generation hook secret, embedded into the compiled
        // multiplexer so only this generation's VM and the kernel know it.
        let hook_nonce = Id128::generate().to_string();
        let compiled = self
            .vm
            .compile(&format!("{}\n{}", manifest.source, hot_multiplexer(&hook_nonce)))?;
        let generation = self.next_generation;
        self.next_generation += 1;
        let dyn_host: Arc<dyn kanbei_vm::Host> = self.host.clone();
        let instance = self.vm.instantiate(&compiled, generation, dyn_host)?;
        // The generation's store now has a single owner: its actor thread.
        let runtime = GenerationRuntime::spawn(
            generation,
            instance,
            REPLY_TIMEOUT,
            Arc::clone(&self.leaked_threads),
        )
        .map_err(ModuleError::Io)?;
        let info = TokenInfo {
            generation,
            module_id: manifest.module_id,
            origin: manifest.origin,
            scope: manifest.scope.clone(),
            deps: manifest.deps.clone(),
            state_key: manifest.state_key.clone(),
            state_schema: manifest.state_schema,
            supersede: supersede.clone(),
        };
        self.tokens.write().expect("tokens lock poisoned").insert(generation, info);
        self.instances
            .lock()
            .expect("instances lock poisoned")
            .insert(generation, Arc::clone(&runtime));
        {
            let mut tables = self.tables.lock().expect("lifecycle tables lock poisoned");
            tables.current.insert(manifest.module_id, generation);
            tables.generation_token.insert(generation, generation);
            tables.packages.insert(generation, package);
            tables.hook_nonces.insert(generation, hook_nonce.clone());
        }
        if let Err(e) = self.run_activation(&runtime, &manifest.source, &hook_nonce, activation_wait) {
            // Roll back atomically (C-F2): invalidate the token and unpublish
            // anything the failed activation staged (services, contributions, UI
            // mounts) BEFORE shutting the actor down, so no in-flight op can
            // re-publish against a generation we are tearing down. Then drop the
            // kernel's handles and drain, bounded by the same activation wait so
            // a wedged activation cannot extend the stall (NEW-3).
            self.host.teardown_generation(generation, true);
            self.instances.lock().expect("instances lock poisoned").remove(&generation);
            {
                let mut tables = self.tables.lock().expect("lifecycle tables lock poisoned");
                tables.current.remove(&manifest.module_id);
                tables.generation_token.remove(&generation);
                tables.packages.remove(&generation);
                tables.hook_nonces.remove(&generation);
            }
            let _ = runtime.shutdown(DRAIN_DEADLINE.min(activation_wait));
            return Err(e);
        }
        Ok(Generation {
            generation,
            module_id: manifest.module_id,
            package,
            runtime,
            scope: manifest.scope.clone(),
            instances: Arc::clone(&self.instances),
            tables: Arc::clone(&self.tables),
            host: Arc::clone(&self.host),
        })
    }

    /// Runs the activation entry on the generation's actor (see the module
    /// docs: `run_script` of `source + ACTIVATION_SHIM`).
    fn run_activation(
        &self,
        runtime: &Arc<GenerationRuntime>,
        source: &str,
        hook_nonce: &str,
        wait: Duration,
    ) -> Result<(), ModuleError> {
        let script = format!(
            "{source}\n{}\n{ACTIVATION_SHIM}",
            hot_multiplexer(hook_nonce)
        );
        runtime
            .run_script_within(&script, wait)
            .map_err(|e| ModuleError::Activation(format!("activation entry failed: {e}")))?
            .map_err(|e| ModuleError::Activation(format!("activation entry failed: {e}")))
    }

    /// Deactivates a module: fails without mutating anything when any of the
    /// generation's published services still has dependents
    /// ([`ModuleError::DependentsRemain`]); otherwise removes the services,
    /// invalidates the token (stale → the host rejects its effects), drains and
    /// joins the generation's actor, and records the disposal.
    pub fn deactivate(&mut self, module_id: Id128) -> Result<DisposalRecord, ModuleError> {
        let generation = *self
            .tables
            .lock()
            .expect("lifecycle tables lock poisoned")
            .current
            .get(&module_id)
            .ok_or(ModuleError::NotActivated { module_id })?;
        let published = self.published_keys(generation);
        let dependents = self.service_dependents(&published);
        if !dependents.is_empty() {
            return Err(ModuleError::DependentsRemain {
                module_id,
                dependents,
            });
        }
        // Invalidate the token and unpublish the generation's effects BEFORE
        // dropping it. Token-first (T7): the old order removed services while
        // the token was still current, so an in-flight op could pass
        // `ensure_current` and re-publish a key we had just removed.
        self.host.teardown_generation(generation, true);
        let drain = self.drop_generation(module_id, generation);
        Ok(drain.record(generation, "deactivation"))
    }

    /// As [`Self::deactivate`], but tears the generation down even when its
    /// published services still have dependents (safe-mode drop, F2): the
    /// caller has decided the committed removal is true, so the teardown must
    /// not silently no-op. Services are unpublished unconditionally.
    pub fn force_deactivate(&mut self, module_id: Id128) -> Result<DisposalRecord, ModuleError> {
        let generation = *self
            .tables
            .lock()
            .expect("lifecycle tables lock poisoned")
            .current
            .get(&module_id)
            .ok_or(ModuleError::NotActivated { module_id })?;
        // Invalidate the token and unpublish the generation's effects BEFORE
        // dropping it (token-first, mirroring `deactivate`).
        self.host.teardown_generation(generation, true);
        let drain = self.drop_generation(module_id, generation);
        Ok(drain.record(generation, "deactivation"))
    }

    /// Generation replacement (R-25/C-05): activates the new generation first
    /// (its activation may re-publish the module's services via
    /// `service_publish` — the same-module replace intent), then disposes the
    /// old generation, then re-publishes any old service publication the new
    /// generation did not take over (preserving the old contract version) and
    /// plans dependents: version-compatible ones rebind, version-incompatible
    /// ones must restart — M2 cannot restart dependent generations, so a
    /// non-empty restart plan fails with [`ModuleError::RestartFailed`]
    /// (surfaced after the swap; M2's transaction is not rollback-atomic).
    ///
    /// Note: `deactivate` itself is not used here — its dependents pre-check
    /// would reject the very replacement R-25/C-05 exists for.
    pub fn replace(
        &mut self,
        module_id: Id128,
        new_manifest: &PackageManifest,
    ) -> Result<ReplacementOutcome, ModuleError> {
        if new_manifest.module_id != module_id {
            return Err(ModuleError::InvalidInput(format!(
                "replace: manifest module_id {} differs from the replaced module {module_id}",
                new_manifest.module_id
            )));
        }
        let old_generation = *self
            .tables
            .lock()
            .expect("lifecycle tables lock poisoned")
            .current
            .get(&module_id)
            .ok_or(ModuleError::NotActivated { module_id })?;
        let old_entries = self.service_entries(old_generation);
        let new_gen = self.activate(new_manifest)?;
        let old_drain = self.drop_generation(module_id, old_generation);
        let mut rebind = Vec::new();
        let mut restart = Vec::new();
        for (key, old_provider) in old_entries {
            let holder = self
                .services
                .lock()
                .expect("services lock poisoned")
                .snapshot()
                .into_iter()
                .find(|(k, _, _)| *k == key)
                .map(|(_, p, _)| p);
            let provider = match holder {
                // The new generation took the key over during its activation.
                Some(p) if p.generation == new_gen.generation => p,
                // Another module owns it now (defensive; single-threaded, so
                // unreachable in M2).
                Some(_) => continue,
                // Still held by the old generation: preserve the publication
                // under the new generation with the old contract version
                // (same-module replace intent).
                None => {
                    let p = ServiceProvider {
                        module_id,
                        generation: new_gen.generation,
                        contract: old_provider.contract.clone(),
                    };
                    self.services
                        .lock()
                        .expect("services lock poisoned")
                        .replace_publish(
                            key.clone(),
                            p.clone(),
                            &ReplaceIntent {
                                current: old_provider,
                                proposed: p.clone(),
                            },
                        )?;
                    p
                }
            };
            let plan = replacement::plan_replacement(
                &self.services.lock().expect("services lock poisoned"),
                &key,
                &provider,
            )?;
            rebind.extend(plan.rebind);
            restart.extend(plan.restart);
        }
        if let Some(dependent) = restart.first() {
            return Err(ModuleError::RestartFailed {
                dependent: dependent.clone(),
                reason: "version-incompatible dependent cannot be restarted in M2 (no dependent-generation registry)"
                    .into(),
            });
        }
        Ok(ReplacementOutcome {
            old: old_drain.record(old_generation, "replacement"),
            new: new_gen,
            rebind,
            restart,
        })
    }

    /// The `StateStore` currency callback source: a generation is current
    /// while it is registered (generation ids are never reused, so being
    /// registered = current).
    pub fn generation_current(&self, generation: u64) -> bool {
        self.host.is_current(generation)
    }

    /// `(module_id, generation, package digest)` for the execution-snapshot
    /// manifest's module pins (the session lane uses it), in module_id order.
    /// The contributions a generation staged via `contribution_publish`
    /// (the session's activation-delta source for non-service contributions,
    /// M5 UI/theme).
    pub fn published_contributions(&self, generation: u64) -> Vec<kanbei_scopes::contrib::Contribution> {
        self.host.published_contributions(generation)
    }

    /// The live generation that mounted a UI component, if any.
    pub fn ui_generation(&self, component: &str) -> Option<u64> {
        self.host.ui_generation(component)
    }

    /// The stable module id of a live generation, if any (UI keybinding
    /// ownership attribution).
    pub fn generation_module_id(&self, generation: u64) -> Option<Id128> {
        self.host.generation_module_id(generation)
    }

    /// The manifest origin of a live generation, if any (UI render-context
    /// trust gate).
    pub fn generation_origin(&self, generation: u64) -> Option<ModuleOrigin> {
        self.host.generation_origin(generation)
    }

    /// The live generation that declared hook `(scope, kind, name)`, if any
    /// (T9/E: the full scope path is part of the key).
    pub fn hook_generation(&self, scope: &ScopePath, hook: HookKind, name: &str) -> Option<u64> {
        self.host.hook_generation(scope, hook, name)
    }

    /// Direct kernel-side call of a generation's `kb_hot` (the kernel side of
    /// `service_call`; used by the UI host). Generation must be live.
    pub fn call_generation(&self, generation: u64, args: &str) -> Result<String, ModuleError> {
        let runtime = self
            .instances
            .lock()
            .expect("instances lock poisoned")
            .get(&generation)
            .cloned()
            .ok_or_else(|| ModuleError::Call(format!("generation {generation} is not live")))?;
        runtime
            .hot("kb_hot", args)
            .map_err(|e| ModuleError::Call(format!("generation {generation} is unavailable: {e}")))?
            .map_err(|e| ModuleError::Call(format!("generation {generation} failed: {e}")))
    }

    /// Invoke a kernel-initiated hook (T9) on `generation`: multiplexes over
    /// `kb_hot` with the envelope
    /// `{"__kb_hook":"<kind>","context":<context_json>}`. Runs under a FRESH
    /// root scope (depth 0, empty visited, its own deadline) — hooks are
    /// kernel-initiated and must not join a `service_call` chain.
    ///
    /// Bounded by `wait` (callers pass [`HOOK_WAIT`]; tests use a shorter
    /// bound): a wedged actor surfaces as [`HookError::Timeout`] within the
    /// bound. Failures stay structured so the session can classify
    /// trap/timeout/invalid for its degrade policy — malformed decision JSON
    /// comes back as the raw string and is parsed at the session layer.
    pub fn call_hook(
        &self,
        generation: u64,
        hook: HookKind,
        context_json: &str,
        wait: Duration,
    ) -> Result<String, HookError> {
        let runtime = self
            .instances
            .lock()
            .expect("instances lock poisoned")
            .get(&generation)
            .cloned()
            .ok_or(HookError::Gone)?;
        // D: present the per-generation secret so a peer module's guessed
        // `service_call` cannot impersonate a kernel-initiated hook.
        let nonce = self
            .tables
            .lock()
            .expect("lifecycle tables lock poisoned")
            .hook_nonces
            .get(&generation)
            .cloned()
            .ok_or(HookError::Gone)?;
        let context: serde_json::Value =
            serde_json::from_str(context_json).map_err(|_| HookError::InvalidContext)?;
        let envelope = serde_json::json!({
            "__kb_hook": hook.as_str(),
            "__kb_nonce": nonce,
            "context": context,
        })
        .to_string();
        let scope = Scope::hook(Instant::now() + wait);
        match runtime.hot_within("kb_hot", &envelope, wait, scope) {
            Err(crate::runtime::ActorError::Wedged) => Err(HookError::Timeout),
            Err(crate::runtime::ActorError::Gone) => Err(HookError::Gone),
            Ok(Ok(result)) => Ok(result),
            Ok(Err(e)) => Err(HookError::from_guest(e)),
        }
    }

    /// Respawn a module under a NEW generation id (T9): dispose the current
    /// generation through the canonical teardown path (token-first, broker
    /// prune, contribution drop, drain) and re-activate the SAME package. The
    /// generation id and token are never reused, and the manifest is read back
    /// byte-identically from the object store, so the package/composition
    /// digest is unchanged.
    pub fn respawn(&mut self, module_id: Id128) -> Result<u64, ModuleError> {
        self.respawn_bounded(module_id, DRAIN_DEADLINE)
    }

    /// As [`Self::respawn`], but the old actor's drain is bounded by
    /// `drain_budget` (G: the hook fault path must never stall a decision).
    /// The token is invalidated before the drain, so a detached actor cannot
    /// commit further host ops.
    pub fn respawn_bounded(
        &mut self,
        module_id: Id128,
        drain_budget: Duration,
    ) -> Result<u64, ModuleError> {
        let generation = *self
            .tables
            .lock()
            .expect("lifecycle tables lock poisoned")
            .current
            .get(&module_id)
            .ok_or(ModuleError::NotActivated { module_id })?;
        let package = *self
            .tables
            .lock()
            .expect("lifecycle tables lock poisoned")
            .packages
            .get(&generation)
            .ok_or_else(|| {
                ModuleError::InvalidInput(format!(
                    "respawn: no package recorded for generation {generation}"
                ))
            })?;
        let bytes = self.packages.get(&package)?;
        let manifest: PackageManifest = serde_json::from_slice(&bytes).map_err(|e| {
            ModuleError::InvalidInput(format!("respawn: stored package is not a manifest: {e}"))
        })?;
        // Canonical teardown (T7/T18): token-first, generation-scoped broker
        // prune, contribution drop, then drain the actor. Services are
        // unpublished unconditionally — the re-activation re-publishes them.
        self.host.teardown_generation(generation, true);
        self.drop_generation_with(module_id, generation, drain_budget);
        let new = self.activate_bounded(&manifest, &HashSet::new(), RESPAWN_ACTIVATION_WAIT)?;
        Ok(new.generation)
    }

    pub fn snapshot(&self) -> Vec<(Id128, u64, Digest)> {
        let tables = self.tables.lock().expect("lifecycle tables lock poisoned");
        let mut out: Vec<_> = tables
            .current
            .iter()
            .map(|(id, g)| (*id, *g, tables.packages[g]))
            .collect();
        out.sort_by_key(|(id, _, _)| id.to_string());
        out
    }

    fn published_keys(&self, generation: u64) -> Vec<ServiceKey> {
        self.services
            .lock()
            .expect("services lock poisoned")
            .snapshot()
            .into_iter()
            .filter(|(_, p, _)| p.generation == generation)
            .map(|(k, _, _)| k)
            .collect()
    }

    fn service_entries(&self, generation: u64) -> Vec<(ServiceKey, ServiceProvider)> {
        self.services
            .lock()
            .expect("services lock poisoned")
            .snapshot()
            .into_iter()
            .filter(|(_, p, _)| p.generation == generation)
            .map(|(k, p, _)| (k, p))
            .collect()
    }

    fn service_dependents(&self, keys: &[ServiceKey]) -> Vec<ServiceDependency> {
        let reg = self.services.lock().expect("services lock poisoned");
        let mut out = Vec::new();
        for key in keys {
            out.extend(reg.dependents_of(key));
        }
        out
    }

    /// Removes a generation from every kernel table via the canonical teardown
    /// (broker grants pruned, token → stale, contributions dropped, actor
    /// drained, packages/current cleared). Services are untouched — callers
    /// decide their fate (`replace` rebinds them). The `current` entry is
    /// removed only when it still names this generation (a replacement may
    /// already have registered the next generation under the same module id).
    /// Returns how the actor drain ended ([`Drain`]).
    fn drop_generation(&mut self, module_id: Id128, generation: u64) -> Drain {
        self.drop_generation_with(module_id, generation, DRAIN_DEADLINE)
    }

    /// As [`Self::drop_generation`], with a caller-supplied drain deadline so
    /// the fault-decision path can bound its teardown (G).
    fn drop_generation_with(
        &mut self,
        module_id: Id128,
        generation: u64,
        deadline: Duration,
    ) -> Drain {
        // Canonical teardown outside the tables lock (never nest it under the
        // host's own locks).
        self.host.teardown_generation(generation, false);
        {
            let mut tables = self.tables.lock().expect("lifecycle tables lock poisoned");
            tables.generation_token.remove(&generation);
            tables.packages.remove(&generation);
            tables.hook_nonces.remove(&generation);
            if tables.current.get(&module_id) == Some(&generation) {
                tables.current.remove(&module_id);
            }
        }
        // Then drain the actor outside every kernel lock: the mailbox send and
        // join must not run while holding the tables lock (T20's no-lock-across-
        // mailbox rule).
        let runtime = self.instances.lock().expect("instances lock poisoned").remove(&generation);
        Drain::of_with(runtime, deadline)
    }
}

impl Drop for ModuleManager {
    /// Teardown: invalidate every remaining generation's token and drain every
    /// actor still in the table. A wedged actor is detached after
    /// `DRAIN_DEADLINE` (its thread can outlive the manager); generations the vm
    /// already retired were removed from the table and could not be joined.
    fn drop(&mut self) {
        // A detached actor keeps `Arc<ModuleHost>` alive; clear the token table
        // so it cannot commit further host ops after teardown.
        self.tokens.write().expect("tokens lock poisoned").clear();
        let runtimes: Vec<_> = self
            .instances
            .lock()
            .expect("instances lock poisoned")
            .drain()
            .map(|(_, runtime)| runtime)
            .collect();
        for runtime in runtimes {
            let _ = runtime.shutdown(DRAIN_DEADLINE);
        }
    }
}

/// Why a kernel-initiated hook call did not produce a decision (T9).
///
/// Deliberately structured (not flattened into [`ModuleError::Call`]) so the
/// session's degrade policy can classify the outcome:
/// - [`HookError::Trap`] — the guest trapped/exhausted (fuel, epoch, memory,
///   host timeout, generation budget);
/// - [`HookError::Timeout`] — the actor did not answer within the bound
///   (wedged; outcome unknown, the queued command still executes);
/// - [`HookError::Invalid`] — the actor answered with a guest/return error (the
///   session classifies malformed decision JSON itself);
/// - [`HookError::InvalidContext`] — the KERNEL supplied a non-JSON context
///   (a kernel-side bug): never degrades or respawns a healthy guest (H);
/// - [`HookError::Gone`] — the actor is gone or the generation was never live.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum HookError {
    #[error("hook call trapped (fuel/epoch/memory/host-timeout/budget)")]
    Trap,
    #[error("hook call timed out: the actor is wedged (outcome unknown)")]
    Timeout,
    #[error("hook call returned an invalid result")]
    Invalid,
    #[error("hook call received an invalid kernel context")]
    InvalidContext,
    #[error("hook call target generation is gone")]
    Gone,
}

impl HookError {
    /// Classify a guest-side error: trap-class outcomes are retryable/degrade
    /// as `Trap`; everything else (returned error codes, retirement, host
    /// string errors) is `Invalid`.
    fn from_guest(e: GuestError) -> Self {
        match e {
            GuestError::Trap(_)
            | GuestError::Fuel { .. }
            | GuestError::Epoch
            | GuestError::OutOfMemory
            | GuestError::HostTimeout { .. }
            | GuestError::GenerationBudget { .. } => HookError::Trap,
            _ => HookError::Invalid,
        }
    }
}

#[derive(Debug, Error)]
pub enum ModuleError {
    #[error(transparent)]
    Vm(#[from] GuestError),
    #[error(transparent)]
    Object(#[from] ObjectError),
    #[error(transparent)]
    State(#[from] StateError),
    #[error(transparent)]
    Service(#[from] ServiceError),
    #[error(transparent)]
    Capability(#[from] BrokerError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("module {module_id} cannot be deactivated: its services still have dependents: {dependents:?}")]
    DependentsRemain {
        module_id: Id128,
        dependents: Vec<ServiceDependency>,
    },
    #[error("replacement requires restarting dependent `{dependent}`, which M2 cannot do: {reason}")]
    RestartFailed { dependent: ServiceKey, reason: String },
    #[error("module {module_id} is not activated")]
    NotActivated { module_id: Id128 },
    #[error("activation failed: {0}")]
    Activation(String),
    #[error("generation call failed: {0}")]
    Call(String),
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

impl From<crate::package::PackageError> for ModuleError {
    fn from(e: crate::package::PackageError) -> Self {
        use crate::package::PackageError;
        match e {
            PackageError::SchemaMismatch { expected, actual } => ModuleError::InvalidInput(
                format!("package schema {actual} is not supported (expected {expected})"),
            ),
            PackageError::Object(o) => ModuleError::Object(o),
            PackageError::Io(io) => ModuleError::Io(io),
            PackageError::InvalidInput(m) => ModuleError::InvalidInput(m),
        }
    }
}
