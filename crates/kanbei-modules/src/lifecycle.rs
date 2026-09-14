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

use std::collections::HashMap;
use std::io;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use kanbei_capabilities::BrokerError;
use kanbei_core::id::Id128;
use kanbei_core::Digest;
use kanbei_objects::{ObjectError, ObjectStore};
use kanbei_services::{
    replacement, ReplaceIntent, ScopePath, ServiceDependency, ServiceError, ServiceKey,
    ServiceProvider, ServiceRegistry,
};
use kanbei_vm::{GuestError, Vm};
use thiserror::Error;

use crate::host::{ModuleHost, TokenInfo};
use crate::package::{install_package, PackageManifest};
use crate::runtime::{DRAIN_DEADLINE, GenerationRuntime, REPLY_TIMEOUT};
use crate::state::{StateError, StateStore};

/// The Luau activation shim: builds the `ctx` handle over `kb_host_call` and
/// invokes the module's `kb_on_activate(ctx)`. Appended to the module source
/// and executed via `run_script` (see the module docs). Internal/unstable
/// ABI. `service_publish` is op 6; the ops are documented on
/// [`ModuleHost`].
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
"#;

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
    tokens: Arc<RwLock<HashMap<u64, TokenInfo>>>,
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
}

impl Drain {
    /// Drain a generation's actor, if the table still holds it.
    fn of(runtime: Option<Arc<GenerationRuntime>>) -> Self {
        match runtime {
            None => Drain::AlreadyRetired,
            Some(runtime) if runtime.shutdown(DRAIN_DEADLINE) => Drain::Joined,
            Some(_) => Drain::Detached,
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
        // Invalidate the token and tear down the kernel's handles FIRST (the
        // T18 commit fence makes in-flight mutating ops reject once the token is
        // gone), then drain the actor — never holding a kernel lock across the
        // mailbox send/join.
        self.tokens
            .write()
            .expect("tokens lock poisoned")
            .remove(&self.generation);
        self.host.unpublish_generation(self.generation);
        self.instances
            .lock()
            .expect("instances lock poisoned")
            .remove(&self.generation);
        {
            let mut tables = self.tables.lock().expect("lifecycle tables lock poisoned");
            tables.generation_token.remove(&self.generation);
            tables.packages.remove(&self.generation);
            tables.current.retain(|_, g| *g != self.generation);
        }
        let drain = if self.runtime.shutdown(DRAIN_DEADLINE) {
            Drain::Joined
        } else {
            Drain::Detached
        };
        drain.record(self.generation, "dispose")
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
    store: ObjectStore,
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
        store: ObjectStore,
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
            store,
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

    /// Installs the package (content-deduped), compiles the source, assigns a
    /// fresh generation id (= vm token), instantiates, registers, and runs the
    /// activation entry. On any failure nothing is registered (fail-closed).
    pub fn activate(&mut self, manifest: &PackageManifest) -> Result<Generation, ModuleError> {
        let (package, _deduped) = install_package(&mut self.store, manifest)?;
        let compiled = self.vm.compile(&manifest.source)?;
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
            scope: manifest.scope.clone(),
            deps: manifest.deps.clone(),
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
        }
        if let Err(e) = self.run_activation(&runtime, &manifest.source) {
            // Roll back: nothing registered on activation failure. (A failed
            // activation may have published services via the host before
            // failing; those registry entries point at the dead generation and
            // are re-taken by the next same-module publish — M2 documents this
            // rather than rolling the registry back.)
            let _ = runtime.shutdown(DRAIN_DEADLINE);
            self.tokens.write().expect("tokens lock poisoned").remove(&generation);
            self.instances.lock().expect("instances lock poisoned").remove(&generation);
            let mut tables = self.tables.lock().expect("lifecycle tables lock poisoned");
            tables.current.remove(&manifest.module_id);
            tables.generation_token.remove(&generation);
            tables.packages.remove(&generation);
            return Err(e);
        }
        Ok(Generation {
            generation,
            module_id: manifest.module_id,
            package,
            runtime,
            scope: manifest.scope.clone(),
            tokens: Arc::clone(&self.tokens),
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
    ) -> Result<(), ModuleError> {
        let script = format!("{source}\n{ACTIVATION_SHIM}");
        runtime
            .run_script(&script)
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
        {
            let mut reg = self.services.lock().expect("services lock poisoned");
            for key in &published {
                if let Err(e) = reg.remove(key, module_id) {
                    return Err(match e {
                        ServiceError::DependentsExist { dependents, .. } => {
                            ModuleError::DependentsRemain { module_id, dependents }
                        }
                        other => ModuleError::Service(other),
                    });
                }
            }
        }
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

    /// Removes a generation from every kernel table (token → stale, actor
    /// drained, packages/current cleared). Services are untouched — callers
    /// decide their fate first. The `current` entry is removed only when it
    /// still names this generation (a replacement may already have registered
    /// the next generation under the same module id). Returns how the actor
    /// drain ended ([`Drain`]).
    fn drop_generation(&mut self, module_id: Id128, generation: u64) -> Drain {
        // Invalidate the token first (T18's commit fence then rejects any
        // further mutating op), and clear the tables — all without an actor lock.
        {
            let mut tables = self.tables.lock().expect("lifecycle tables lock poisoned");
            if let Some(token) = tables.generation_token.remove(&generation) {
                self.tokens.write().expect("tokens lock poisoned").remove(&token);
            }
            self.host.drop_generation_contributions(generation);
            tables.packages.remove(&generation);
            if tables.current.get(&module_id) == Some(&generation) {
                tables.current.remove(&module_id);
            }
        }
        // Then drain the actor outside every kernel lock: the mailbox send and
        // join must not run while holding the tables lock (T20's no-lock-across-
        // mailbox rule).
        let runtime = self.instances.lock().expect("instances lock poisoned").remove(&generation);
        Drain::of(runtime)
    }
}

impl Drop for ModuleManager {
    /// Teardown: invalidate every remaining generation's token and drain its
    /// actor, so no store (and no thread) outlives the manager. A `Generation`
    /// handle that outlives the manager becomes inert (`ActorError::Gone`).
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
