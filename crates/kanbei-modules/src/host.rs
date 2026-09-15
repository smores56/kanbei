//! The kernel `kanbei_vm::Host` impl: generation-token-gated dispatch of the
//! module host-call ABI. The vm's linker closures call
//! [`ModuleHost::call`]`(token, op, payload)` from inside a guest call; the
//! token check comes first on every op, so displaced generations cannot act
//! (they get `Err("stale generation")`, which the vm maps to
//! `GuestError::StaleGeneration`).
//!
//! # Host-call ABI (internal/unstable, M2)
//!
//! All payloads are JSON objects; all responses are JSON strings; host errors
//! are `Err` strings (the vm traps them as `GuestError::Host`).
//!
//! | op | name | payload | response |
//! |----|------|---------|----------|
//! | 0 | `log` | `{"msg": <string>}` | `"ok"` |
//! | 1 | `state_get` | `{"key": <string>}` | `{"ok":true,"value":<json\|null>}` |
//! | 2 | `state_set` | `{"key": <string>, "schema": <u32>, "value": <json>}` | `{"ok":true,"head":"<digest>"}` |
//! | 3 | `service_call` | `{"key": <ServiceKey>, "args": <json>}` | the provider generation's `kb_hot` result JSON |
//! | 4 | `check` | `{"resource": <string>, "verbs": [<string>]}` | `{"allowed":true}` |
//! | 5 | `require_approval` | `{"resource": <string>, "verbs": [<string>]}` | `{"intent": <ApprovalIntent>}` |
//! | 6 | `service_publish` | `{"key": <ServiceKey>, "version": <u32>, "deps": [<ServiceDependency>]}` | `"ok"` |
//! | 7 | `contribution_publish` | `{"kind": "ui"\|"theme"\|"settings", ...}` | `"ok"` |
//! | 6 | `service_publish` | `{"key": <ServiceKey>, "version": <u32>, "deps": [<ServiceDependency>]}` | `"ok"` |
//!
//! M2 keeps state bytes as the compact JSON encoding of the value the module
//! wrote. `service_call` is synchronous: one mailbox hop to the provider
//! generation's `kb_hot`, carrying a `{depth, visited, deadline}` scope so a
//! single call chain is bounded in depth, cannot revisit a generation (a cycle
//! like A→B→A is rejected before the hop), and shares one deadline across the
//! whole chain. `service_publish` is an M2 extension of the kernel op set (the
//! module publishes its services during `kb_on_activate`; R-25/C-06 publication
//! is the key free or an explicit same-module replace intent).
//!
//! `check` passes the broker's current policy version (the highest version
//! across registered templates; the session lane owns template mutations).

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use kanbei_capabilities::{ApprovalIntent, Broker, Capability, GrantScope, Principal};
use kanbei_core::id::Id128;
use kanbei_scopes::contrib::{
    ApprovalSettings, Contribution, ContributionKind, HookContribution, HookKind,
    ProviderSettings, SettingsContribution, ThemeContribution, UiMountContribution,
};
use kanbei_services::{
    ReplaceIntent, ScopePath, ServiceContract, ServiceDependency, ServiceError, ServiceKey,
    ServiceProvider, ServiceRegistry,
};
use kanbei_vm::Host;
use serde_json::{json, Value};
use std::collections::HashSet;

use crate::runtime::{ActorError, GenerationRuntime, Scope, REPLY_TIMEOUT};
use crate::state::{StateStore, StateUpdate};

/// The exact string kanbei-vm maps to `GuestError::StaleGeneration`
/// (kanbei-vm's `STALE_GENERATION` const is crate-private; the contract is
/// frozen).
const STALE_GENERATION: &str = "stale generation";

/// Upper bound on waiting for a provider generation's actor inside
/// `service_call`. Kept below the vm's host-import timeout (default 5s) so the
/// caller's supervised worker returns (and releases its permit) rather than
/// being abandoned and retiring the caller; the coupling is not enforced here
/// because the vm's timeout is not visible to this crate.
pub(crate) const SERVICE_CALL_WAIT: Duration = Duration::from_secs(4);

/// Identity of a live generation, resolved from its vm token. The vm token
/// equals the generation id (both are fresh, never-reused counters), so the
/// token table doubles as the generation-currency table.
#[derive(Clone, Debug)]
pub(crate) struct TokenInfo {
    pub generation: u64,
    pub module_id: Id128,
    pub scope: ScopePath,
    /// The module's declared service dependencies (manifest `deps`) — the
    /// caller-side version contract for `service_call`.
    pub deps: Vec<ServiceDependency>,
    /// The manifest's state binding (R-07/C-F1), if declared: the module's
    /// designated head key and its schema. Writes to the bound key must use the
    /// declared schema, so a module cannot create a head its own manifest would
    /// later reject at activation.
    pub state_key: Option<String>,
    pub state_schema: Option<u32>,
    /// Service keys this generation may take over from a LOWER-precedence active
    /// config layer (decision 28 precedence-driven implicit replacement). The
    /// session computes this at activation time from `active_config_layers`; the
    /// host allows `service_publish` to displace a DIFFERENT module's holder only
    /// for keys in this set. Empty for every non-config activation, preserving
    /// the plain `Conflict`.
    pub supersede: HashSet<ServiceKey>,
}

/// The kernel host: split shared fields (no `Arc<Mutex<ModuleManager>>` — a
/// manager holding its lock while calling into an instance would deadlock on
/// the re-entrant host call). All fields are shared with the
/// [`crate::lifecycle::ModuleManager`]; the host itself owns the broker and
/// the log sink.
pub struct ModuleHost {
    session: Mutex<Id128>,
    tokens: Arc<RwLock<HashMap<u64, TokenInfo>>>,
    /// Weak: the manager owns the instance table. A strong edge here would
    /// create the cycle host → table → instance → host (each instance
    /// captures the host Arc), leaking every generation's Wasm store.
    instances: Weak<Mutex<HashMap<u64, Arc<GenerationRuntime>>>>,
    services: Arc<Mutex<ServiceRegistry>>,
    state: Arc<Mutex<StateStore>>,
    broker: Mutex<Broker>,
    /// Kernel log sink: M2 accumulates entries here (tests read them); the
    /// session lane will drain it into canonical log facts later.
    log: Mutex<Vec<String>>,
    rejected_stale_effects: Arc<AtomicU64>,
    /// Contributions published per generation via `contribution_publish`
    /// (M5 UI/theme mounts). Kept out of the live registry until the session
    /// stages + OCC-publishes them atomically (the activation delta).
    contributions: Mutex<HashMap<u64, Vec<Contribution>>>,
    /// UI component name → generation that mounted it (stale generations are
    /// removed on disposal, so a displaced mount cannot be resolved).
    ui_components: Mutex<HashMap<String, u64>>,
    /// Hook `(scope, kind, name)` → generation that declared it (mirrors
    /// `ui_components`; stale generations are pruned on disposal). Keyed by
    /// the full scope path so same-named hooks in different scopes never
    /// collide/misbind (T9/E).
    hooks: Mutex<HashMap<(ScopePath, HookKind, String), u64>>,
    /// The kernel's canonical generation-currency predicate (shared with the
    /// `StateStore`). Mutating ops re-read it at their commit point so a
    /// generation retired mid-op cannot commit (R-02/C-03); the check is
    /// non-blocking, so retirement never waits on an in-flight op.
    current: Arc<dyn Fn(u64) -> bool + Send + Sync>,
}

impl ModuleHost {
    /// The manager constructs the host with the shared tables (see
    /// `ModuleManager::new`); the session lane reaches it via the manager.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        session: Id128,
        tokens: Arc<RwLock<HashMap<u64, TokenInfo>>>,
        instances: Weak<Mutex<HashMap<u64, Arc<GenerationRuntime>>>>,
        services: Arc<Mutex<ServiceRegistry>>,
        state: Arc<Mutex<StateStore>>,
        rejected_stale_effects: Arc<AtomicU64>,
        current: Arc<dyn Fn(u64) -> bool + Send + Sync>,
    ) -> Self {
        Self {
            session: Mutex::new(session),
            tokens,
            instances,
            services,
            state,
            broker: Mutex::new(Broker::new()),
            log: Mutex::new(Vec::new()),
            rejected_stale_effects,
            contributions: Mutex::new(HashMap::new()),
            ui_components: Mutex::new(HashMap::new()),
            hooks: Mutex::new(HashMap::new()),
            current,
        }
    }

    /// Whether a generation is still registered. Delegates to the shared
    /// predicate so the fence, `StateStore::cas`, and `ModuleManager` agree.
    pub(crate) fn is_current(&self, generation: u64) -> bool {
        (self.current)(generation)
    }

    /// Commit-time fence for a mutating op: the generation must still be
    /// current at the moment it writes. Called adjacent to the mutation (under
    /// the target lock where one exists) so a generation retired while the op
    /// was blocked is rejected rather than committing.
    fn ensure_current(&self, generation: u64) -> Result<(), String> {
        if self.is_current(generation) {
            Ok(())
        } else {
            self.rejected_stale_effects.fetch_add(1, Ordering::Relaxed);
            Err(STALE_GENERATION.into())
        }
    }

    /// Canonical generation teardown (T7). Prune the generation's broker grants
    /// and budget, invalidate its token, then (when `services`) remove its
    /// service holdings, then forget its staged contributions/UI mounts
    /// (R-02/C-03/A1).
    ///
    /// Ordering is load-bearing in two places: the broker is taken FIRST so the
    /// prune is atomic against `op_check`'s currency fence + budget consumption
    /// (both run under the broker lock), and the token is removed before
    /// services/contributions so the T18 commit fence rejects any in-flight
    /// mutating op before its effects disappear. `services = false` is only for
    /// `replace`, which rebinds the old generation's service keys under the new
    /// one and must not drop them.
    pub(crate) fn teardown_generation(&self, generation: u64, services: bool) {
        self.broker
            .lock()
            .expect("broker lock poisoned")
            .retire_generation(generation);
        self.tokens.write().expect("tokens lock poisoned").remove(&generation);
        if services {
            self.services
                .lock()
                .expect("services lock poisoned")
                .remove_generation(generation);
        }
        self.drop_generation_contributions(generation);
    }

    pub fn session(&self) -> Id128 {
        *self.session.lock().expect("session lock poisoned")
    }

    pub fn set_session(&self, session: Id128) {
        *self.session.lock().expect("session lock poisoned") = session;
    }

    /// Broker access for the session lane (templates/grants are session-owned).
    pub fn broker(&self) -> &Mutex<Broker> {
        &self.broker
    }

    /// The accumulated kernel log entries (test/session-lane seam).
    pub fn log_entries(&self) -> Vec<String> {
        self.log.lock().expect("log lock poisoned").clone()
    }

    pub fn clear_log(&self) {
        self.log.lock().expect("log lock poisoned").clear();
    }

    /// Rejected effects from stale tokens (displaced generations cannot act).
    pub fn rejected_stale_effects(&self) -> u64 {
        self.rejected_stale_effects.load(Ordering::Relaxed)
    }

    fn principal(&self, info: &TokenInfo) -> Principal {
        Principal {
            session: self.session(),
            generation: info.generation,
            run: None,
        }
    }

    fn policy_version(&self) -> u64 {
        self.broker
            .lock()
            .expect("broker lock poisoned")
            .templates
            .iter()
            .map(|t| t.version)
            .max()
            .unwrap_or(0)
    }

    fn op_log(&self, payload: &str) -> Result<String, String> {
        let v: Value = serde_json::from_str(payload)
            .map_err(|e| format!("log: invalid payload: {e}"))?;
        let msg = v
            .get("msg")
            .and_then(Value::as_str)
            .ok_or_else(|| "log: payload must be {\"msg\": <string>}".to_string())?;
        self.log.lock().expect("log lock poisoned").push(msg.to_string());
        Ok("ok".into())
    }

    fn op_state_get(&self, payload: &str) -> Result<String, String> {
        let v: Value = serde_json::from_str(payload)
            .map_err(|e| format!("state_get: invalid payload: {e}"))?;
        let key = v
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| "state_get: payload must be {\"key\": <string>}".to_string())?;
        let state = self.state.lock().expect("state lock poisoned");
        let Some((_, bytes)) = state
            .get(key)
            .map_err(|e| format!("state_get({key}): {e}"))?
        else {
            return Ok(r#"{"ok":true,"value":null}"#.into());
        };
        let value: Value = serde_json::from_slice(&bytes)
            .map_err(|e| format!("state_get({key}): snapshot bytes are not JSON: {e}"))?;
        Ok(json!({ "ok": true, "value": value }).to_string())
    }

    fn op_state_set(&self, info: &TokenInfo, payload: &str) -> Result<String, String> {
        let v: Value = serde_json::from_str(payload)
            .map_err(|e| format!("state_set: invalid payload: {e}"))?;
        let key = v
            .get("key")
            .and_then(Value::as_str)
            .ok_or_else(|| "state_set: payload must be {\"key\": <string>, ...}".to_string())?;
        let schema = v
            .get("schema")
            .and_then(Value::as_u64)
            .and_then(|s| u32::try_from(s).ok())
            .ok_or_else(|| "state_set: payload field \"schema\" must be a u32".to_string())?;
        let value = v
            .get("value")
            .ok_or_else(|| "state_set: payload must carry a \"value\"".to_string())?;
        let bytes = serde_json::to_vec(value)
            .map_err(|e| format!("state_set: value is not JSON-serializable: {e}"))?;
        // R-07/C-F1: when the manifest binds a state key, writes to it must use
        // the declared schema — otherwise a module could create a head its own
        // manifest would reject at the next activation.
        if info.state_key.as_deref() == Some(key)
            && let Some(expected) = info.state_schema
            && schema != expected
        {
            return Err(format!(
                "state_set({key}): schema {schema} does not match the declared module schema {expected}"
            ));
        }
        let update = StateUpdate {
            key: key.to_string(),
            schema,
            bytes,
            generation: info.generation,
        };
        // Uniform commit fence for the four mutating ops; `StateStore::cas` also
        // re-checks currency under the state lock (defense in depth).
        self.ensure_current(info.generation)?;
        let head = self
            .state
            .lock()
            .expect("state lock poisoned")
            .cas(update)
            .map_err(|e| format!("state_set({key}): {e}"))?;
        Ok(json!({ "ok": true, "head": head.digest }).to_string())
    }

    fn op_service_call(&self, info: &TokenInfo, payload: &str) -> Result<String, String> {
        let v: Value = serde_json::from_str(payload)
            .map_err(|e| format!("service_call: invalid payload: {e}"))?;
        let key: ServiceKey = serde_json::from_value(
            v.get("key")
                .cloned()
                .ok_or_else(|| "service_call: payload must carry a \"key\"".to_string())?,
        )
        .map_err(|e| format!("service_call: \"key\" is not a ServiceKey: {e}"))?;
        let args = v.get("args").cloned().unwrap_or(Value::Null);
        let required_version = info
            .deps
            .iter()
            .find(|d| d.key == key)
            .map(|d| d.required_version)
            .ok_or_else(|| {
                format!(
                    "service_call: `{key}` is not a declared dependency of generation {}",
                    info.generation
                )
            })?;
        let provider = self
            .services
            .lock()
            .expect("services lock poisoned")
            .resolve(&key, required_version, &info.scope)
            .cloned()
            .map_err(|e| format!("service_call: {e}"))?;
        if provider.generation == info.generation {
            return Err(
                "service_call: a generation may not call its own service (mailbox self-hop)".into(),
            );
        }
        // Resolve the caller's actor and the provider's actor from the shared
        // table, then release the table lock before the cross-actor hop (never
        // hold a kernel lock across a mailbox send).
        let map = self
            .instances
            .upgrade()
            .ok_or_else(|| "service_call: kernel module table is gone (hosting shut down)".to_string())?;
        let (caller_scope, provider_runtime) = {
            let guard = map.lock().expect("instances lock poisoned");
            (
                guard.get(&info.generation).and_then(|r| r.scope()),
                guard.get(&provider.generation).cloned(),
            )
        };
        drop(map);
        // A live actor publishes its scope for the command in flight, so the
        // read above is the caller's current chain for a guest-initiated call.
        // Otherwise the call originates outside the guest (the session/UI
        // dispatching an effect on a generation's behalf) and there is no chain
        // yet — re-seed a root scope, but only if the caller is still current:
        // a retired caller must not drive provider work (R-02/C-03), and
        // re-rooting blindly would also drop the depth/cycle controls.
        let caller_scope = match caller_scope {
            Some(scope) => scope,
            None => {
                self.ensure_current(info.generation)?;
                Scope::root(info.generation, Instant::now() + REPLY_TIMEOUT)
            }
        };
        let child = caller_scope.hop(provider.generation)?;
        // The chain shares one deadline: wait only until it elapses, and never
        // longer than the caller's host-import supervision window (so a slow
        // provider cannot make the vm retire the caller).
        let Some(remaining) = child.remaining_until(Instant::now()) else {
            return Err("service_call: the call chain deadline has already elapsed".into());
        };
        let wait = remaining.min(SERVICE_CALL_WAIT);
        let runtime = provider_runtime.ok_or_else(|| {
            format!(
                "service_call: provider generation {} is not live",
                provider.generation
            )
        })?;
        runtime
            .hot_within("kb_hot", &args.to_string(), wait, child)
            .map_err(|e| match e {
                // No cancellation exists, so a wedged provider may still commit
                // its queued `kb_hot`; the caller must treat this as
                // "outcome unknown", not "did not happen".
                ActorError::Wedged => format!(
                    "service_call: provider generation {} did not answer within {wait:?} \
                     (outcome unknown)",
                    provider.generation
                ),
                ActorError::Gone => format!(
                    "service_call: provider generation {} is no longer live",
                    provider.generation
                ),
            })?
            .map_err(|e| {
                format!(
                    "service_call: provider generation {} failed: {e}",
                    provider.generation
                )
            })
    }

    fn op_check(&self, info: &TokenInfo, payload: &str) -> Result<String, String> {
        let v: Value = serde_json::from_str(payload)
            .map_err(|e| format!("check: invalid payload: {e}"))?;
        let resource = v
            .get("resource")
            .and_then(Value::as_str)
            .ok_or_else(|| "check: payload must be {\"resource\": <string>, \"verbs\": [...]}".to_string())?
            .to_string();
        let verbs = verbs_field(&v)?;
        let want = Capability::new(resource, verbs);
        let principal = self.principal(info);
        let version = self.policy_version();
        // Hold the broker lock across the currency fence and the budget
        // consumption (T7): `teardown_generation` prunes grants under the same
        // lock, so a generation retired mid-check cannot consume budget or be
        // granted past retirement.
        let broker = self.broker.lock().expect("broker lock poisoned");
        self.ensure_current(info.generation)?;
        broker
            .check(&principal, &want, version)
            .map_err(|e| format!("check: {e}"))?;
        Ok(r#"{"allowed":true}"#.into())
    }

    fn op_require_approval(&self, info: &TokenInfo, payload: &str) -> Result<String, String> {
        let v: Value = serde_json::from_str(payload)
            .map_err(|e| format!("require_approval: invalid payload: {e}"))?;
        let resource = v
            .get("resource")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                "require_approval: payload must be {\"resource\": <string>, \"verbs\": [...]}"
                    .to_string()
            })?
            .to_string();
        let verbs = verbs_field(&v)?;
        let want = Capability::new(resource, verbs);
        let principal = self.principal(info);
        let broker = self.broker.lock().expect("broker lock poisoned");
        self.ensure_current(info.generation)?;
        let intent = broker
            .require_approval(&principal, &want)
            .map_err(|e| format!("require_approval: {e}"))?;
        Ok(json!({ "intent": intent_json(&intent) }).to_string())
    }

    fn op_service_publish(&self, info: &TokenInfo, payload: &str) -> Result<String, String> {
        let v: Value = serde_json::from_str(payload)
            .map_err(|e| format!("service_publish: invalid payload: {e}"))?;
        let key: ServiceKey = serde_json::from_value(
            v.get("key")
                .cloned()
                .ok_or_else(|| "service_publish: payload must carry a \"key\"".to_string())?,
        )
        .map_err(|e| format!("service_publish: \"key\" is not a ServiceKey: {e}"))?;
        let version = v
            .get("version")
            .and_then(Value::as_u64)
            .and_then(|x| u32::try_from(x).ok())
            .ok_or_else(|| "service_publish: payload field \"version\" must be a u32".to_string())?;
        let deps: Vec<ServiceDependency> = match v.get("deps") {
            None | Some(Value::Null) => Vec::new(),
            Some(d) => serde_json::from_value(d.clone())
                .map_err(|e| format!("service_publish: \"deps\" is not [ServiceDependency]: {e}"))?,
        };
        // R-25/C-06: keys are namespaced by the owning module's scope.
        if key.scope != info.scope {
            return Err(format!(
                "service_publish: key scope `{}` must equal the generation scope `{}`",
                key.scope, info.scope
            ));
        }
        let provider = ServiceProvider {
            module_id: info.module_id,
            generation: info.generation,
            contract: ServiceContract {
                name: key.name.clone(),
                version,
            },
        };
        let mut reg = self.services.lock().expect("services lock poisoned");
        self.ensure_current(info.generation)?;
        let holder = reg
            .snapshot()
            .into_iter()
            .find(|(k, _, _)| *k == key)
            .map(|(_, p, _)| p);
        let result = match holder {
            // Free key: publish (with dependency edges when declared).
            None => {
                if deps.is_empty() {
                    reg.publish(key, provider)
                } else {
                    reg.publish_with_deps(key, provider, &deps)
                }
            }
            // Same module re-publishing during generation replacement: the
            // explicit replace intent (R-25/C-06). Existing dependency edges
            // are preserved.
            Some(current) if current.module_id == info.module_id => {
                let intent = ReplaceIntent {
                    current,
                    proposed: provider.clone(),
                };
                reg.replace_publish(key, provider, &intent)
            }
            // Decision 28: a higher-precedence config layer takes over a key
            // held by a strictly lower-precedence active config layer. The
            // session scoped this generation's `supersede` set to exactly those
            // keys, so any other cross-module conflict still fails below.
            Some(current) if info.supersede.contains(&key) => {
                let intent = ReplaceIntent {
                    current,
                    proposed: provider.clone(),
                };
                reg.replace_publish(key, provider, &intent)
            }
            Some(current) => Err(ServiceError::Conflict {
                key,
                holder: current,
                challenger: provider,
            }),
        };
        result.map_err(|e| format!("service_publish: {e}"))?;
        Ok("ok".into())
    }

    /// M5 contribution publishing (the standard contribution contract):
    /// a generation stages UI mounts / theme overlays during activation.
    /// Contributions are recorded per generation and only enter the live
    /// composition when the session validates and atomically publishes the
    /// activation delta (staged via OCC, R-26/C-09).
    ///
    /// Payloads:
    /// - `{"kind":"ui","name":<string>,"component":<string>,"slot":<string, optional>}`
    /// - `{"kind":"theme","name":<string>,"overlay":<object>}`
    /// - `{"kind":"settings","provider":<object, optional>,"approval":<object, optional>}`
    fn op_contribution_publish(&self, info: &TokenInfo, payload: &str) -> Result<String, String> {
        let v: Value = serde_json::from_str(payload)
            .map_err(|e| format!("contribution_publish: invalid payload: {e}"))?;
        let kind = v
            .get("kind")
            .and_then(Value::as_str)
            .ok_or_else(|| "contribution_publish: payload must carry a \"kind\"".to_string())?;
        let name = || {
            v.get("name")
                .and_then(Value::as_str)
                .map(String::from)
                .ok_or_else(|| "contribution_publish: payload must carry a \"name\"".to_string())
        };
        let contribution = match kind {
            "ui" => {
                let name = name()?;
                let component = v
                    .get("component")
                    .and_then(Value::as_str)
                    .map(String::from)
                    .ok_or_else(|| {
                        "contribution_publish: ui mount must carry a \"component\"".to_string()
                    })?;
                // M8: an optional composite slot (default "main", normalized
                // by the registry at publish); charset is kernel-validated in
                // the registry validate pass.
                let slot = v.get("slot").and_then(Value::as_str).map(String::from);
                let mut ui_components = self
                    .ui_components
                    .lock()
                    .expect("ui components lock poisoned");
                self.ensure_current(info.generation)?;
                ui_components.insert(component.clone(), info.generation);
                Contribution {
                    scope: info.scope.clone(),
                    kind: ContributionKind::UiMount(UiMountContribution {
                        name,
                        component,
                        slot,
                    }),
                }
            }
            "theme" => {
                let name = name()?;
                let overlay = v.get("overlay").cloned().ok_or_else(|| {
                    "contribution_publish: theme must carry an \"overlay\"".to_string()
                })?;
                Contribution {
                    scope: info.scope.clone(),
                    kind: ContributionKind::Theme(ThemeContribution { name, overlay }),
                }
            }
            "hook" => {
                // T9: a named lifecycle hook. Dispatch is by hook kind over
                // the guest's `kb_hot` multiplexer, so no entry name is
                // carried (H). A blank name is filled with the module id (a
                // stable, module-sourced identity) so two modules hooking the
                // same kind never collide.
                let hook = match v.get("hook").and_then(Value::as_str) {
                    Some("on_turn_start") => HookKind::OnTurnStart,
                    Some("on_tool_intent") => HookKind::OnToolIntent,
                    Some(other) => {
                        return Err(format!("contribution_publish: unknown hook {other:?}"));
                    }
                    None => {
                        return Err(
                            "contribution_publish: hook must carry a \"hook\"".to_string()
                        );
                    }
                };
                let name = v
                    .get("name")
                    .and_then(Value::as_str)
                    .filter(|n| !n.is_empty())
                    .map(String::from)
                    .unwrap_or_else(|| info.module_id.to_string());
                self.ensure_current(info.generation)?;
                {
                    let mut hooks = self.hooks.lock().expect("hooks lock poisoned");
                    match hooks.entry((info.scope.clone(), hook, name.clone())) {
                        // A collision from a DIFFERENT live generation is a
                        // conflict: never clobber an unrelated holder.
                        std::collections::hash_map::Entry::Occupied(e)
                            if *e.get() != info.generation =>
                        {
                            return Err(format!(
                                "contribution_publish: hook {name:?} ({:?}) in {} is already held by generation {}",
                                hook,
                                info.scope,
                                e.get()
                            ));
                        }
                        // Same generation re-publishing is idempotent.
                        std::collections::hash_map::Entry::Occupied(e) => {
                            let _ = e.into_mut();
                        }
                        std::collections::hash_map::Entry::Vacant(e) => {
                            e.insert(info.generation);
                        }
                    }
                }
                Contribution {
                    scope: info.scope.clone(),
                    kind: ContributionKind::Hook(HookContribution { name, hook }),
                }
            }
            "settings" => {
                // Decision 28: a desired-state layer publishes typed settings
                // (built-in defaults, user, project). A malformed payload is
                // rejected here, before anything is staged — no partial
                // publish.
                let provider = v
                    .get("provider")
                    .cloned()
                    .map(serde_json::from_value::<ProviderSettings>)
                    .transpose()
                    .map_err(|e| format!("contribution_publish: settings.provider: {e}"))?;
                let approval = v
                    .get("approval")
                    .cloned()
                    .map(serde_json::from_value::<ApprovalSettings>)
                    .transpose()
                    .map_err(|e| format!("contribution_publish: settings.approval: {e}"))?;
                Contribution {
                    scope: info.scope.clone(),
                    kind: ContributionKind::Settings(SettingsContribution { provider, approval }),
                }
            }
            other => return Err(format!("contribution_publish: unknown kind {other:?}")),
        };
        let mut contributions = self.contributions.lock().expect("contributions lock poisoned");
        self.ensure_current(info.generation)?;
        contributions
            .entry(info.generation)
            .or_default()
            .push(contribution);
        Ok("ok".into())
    }

    /// The contributions a generation staged via `contribution_publish`
    /// (session activation-delta collection).
    pub(crate) fn published_contributions(&self, generation: u64) -> Vec<Contribution> {
        self.contributions
            .lock()
            .expect("contributions lock poisoned")
            .get(&generation)
            .cloned()
            .unwrap_or_default()
    }

    /// The generation that mounted a UI component (session UI host
    /// resolution).
    pub(crate) fn ui_generation(&self, component: &str) -> Option<u64> {
        self.ui_components
            .lock()
            .expect("ui components lock poisoned")
            .get(component)
            .copied()
    }

    /// The generation that declared hook `(scope, kind, name)` (session hook
    /// resolution), if it is still live.
    pub(crate) fn hook_generation(
        &self,
        scope: &ScopePath,
        hook: HookKind,
        name: &str,
    ) -> Option<u64> {
        self.hooks
            .lock()
            .expect("hooks lock poisoned")
            .get(&(scope.clone(), hook, name.to_string()))
            .copied()
    }

    /// Forget a generation's staged contributions (disposal, R-02/C-03:
    /// displaced generations cannot act).
    pub(crate) fn drop_generation_contributions(&self, generation: u64) {
        self.contributions
            .lock()
            .expect("contributions lock poisoned")
            .remove(&generation);
        self.ui_components
            .lock()
            .expect("ui components lock poisoned")
            .retain(|_, g| *g != generation);
        self.hooks
            .lock()
            .expect("hooks lock poisoned")
            .retain(|_, g| *g != generation);
    }
}

fn verbs_field(v: &Value) -> Result<Vec<String>, String> {
    v.get("verbs")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|x| {
                    x.as_str()
                        .map(String::from)
                        .ok_or_else(|| "verbs entries must be strings".to_string())
                })
                .collect()
        })
        .unwrap_or_else(|| Err("payload field \"verbs\" must be an array of strings".to_string()))
}

/// `ApprovalIntent` wire shape (that crate has no serde impls); the digest is
/// the text form, scope is the canonical name.
fn intent_json(i: &ApprovalIntent) -> Value {
    json!({
        "digest": i.digest.to_string(),
        "principal": {
            "session": i.principal.session.to_string(),
            "generation": i.principal.generation,
            "run": i.principal.run,
        },
        "module_generation": i.module_generation,
        "action": i.action,
        "args": i.args,
        "cwd_env_fingerprint": i.cwd_env_fingerprint,
        "scope": match i.scope {
            GrantScope::Run => "run",
            GrantScope::Session => "session",
            GrantScope::Project => "project",
            GrantScope::Standing => "standing",
        },
        "expiry": i.expiry,
    })
}

impl Host for ModuleHost {
    fn call(&self, generation_token: u64, op: u32, payload: &str) -> Result<String, String> {
        let info = match self.tokens.read().expect("tokens lock poisoned").get(&generation_token) {
            Some(info) => info.clone(),
            None => {
                // Displaced generations cannot act: reject and record the
                // stale effect (R-02/C-03).
                self.rejected_stale_effects.fetch_add(1, Ordering::Relaxed);
                return Err(STALE_GENERATION.into());
            }
        };
        match op {
            0 => self.op_log(payload),
            1 => self.op_state_get(payload),
            2 => self.op_state_set(&info, payload),
            3 => self.op_service_call(&info, payload),
            4 => self.op_check(&info, payload),
            5 => self.op_require_approval(&info, payload),
            6 => self.op_service_publish(&info, payload),
            7 => self.op_contribution_publish(&info, payload),
            other => Err(format!("unknown host op {other}")),
        }
    }

    fn retire(&self, generation_token: u64, _reason: &str) {
        // Canonical teardown: invalidate currency first so the T18 commit fence
        // rejects any in-flight mutating op at its commit point, then unpublish
        // the generation's effects. `retire` runs on a vm worker and must not
        // block: it only asks the generation's actor to stop (the store drops on
        // the actor thread when it processes the request).
        self.teardown_generation(generation_token, true);
        if let Some(map) = self.instances.upgrade() {
            let runtime = map
                .lock()
                .expect("instances lock poisoned")
                .remove(&generation_token);
            if let Some(runtime) = runtime {
                runtime.request_shutdown();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanbei_core::queue::DurabilityQueue;
    use std::path::PathBuf;

    /// A host with generation 1 registered, sharing its token table with the
    /// test so retirement can be simulated without a live wasm instance.
    fn host_with_generation(tag: &str) -> (PathBuf, Arc<DurabilityQueue>, ModuleHost) {
        let dir = std::env::temp_dir().join(format!("kb-host-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let queue = Arc::new(DurabilityQueue::start(&format!("kb-host-{tag}")));
        let tokens: Arc<RwLock<HashMap<u64, TokenInfo>>> = Arc::new(RwLock::new(HashMap::new()));
        tokens.write().expect("tokens lock poisoned").insert(
            1,
            TokenInfo {
                generation: 1,
                module_id: Id128::generate(),
                scope: ScopePath(vec!["root".into()]),
                deps: Vec::new(),
                state_key: None,
                state_schema: None,
                supersede: HashSet::new(),
            },
        );
        let currency: Arc<dyn Fn(u64) -> bool + Send + Sync> = {
            let tokens = Arc::clone(&tokens);
            Arc::new(move |g| tokens.read().expect("tokens lock poisoned").contains_key(&g))
        };
        let state = StateStore::open(&dir, Arc::clone(&queue), Arc::clone(&currency));
        let host = ModuleHost::new(
            Id128::generate(),
            tokens,
            Weak::new(),
            Arc::new(Mutex::new(ServiceRegistry::new())),
            Arc::new(Mutex::new(state)),
            Arc::new(AtomicU64::new(0)),
            currency,
        );
        (dir, queue, host)
    }

    fn teardown(dir: PathBuf, queue: Arc<DurabilityQueue>) {
        let queue = Arc::try_unwrap(queue)
            .unwrap_or_else(|_| panic!("durability queue Arc still shared"));
        queue.shutdown().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn info() -> TokenInfo {
        TokenInfo {
            generation: 1,
            module_id: Id128::generate(),
            scope: ScopePath(vec!["root".into()]),
            deps: Vec::new(),
            state_key: None,
            state_schema: None,
            supersede: HashSet::new(),
        }
    }

    /// R-02/C-03: a mutating op must re-read generation currency at its commit
    /// point, not trust the `TokenInfo` captured at `call` entry. Retirement
    /// removes the token, so an op that reaches its commit after retirement is
    /// rejected (the entry check in `call` cannot see this race).
    #[test]
    fn mutating_ops_reject_a_generation_retired_before_commit() {
        let (dir, queue, host) = host_with_generation("commit-fence");
        host.retire(1, "test: forced retirement");
        assert_eq!(host.rejected_stale_effects(), 0);

        let i = info();
        let cases: [(&str, Result<String, String>); 4] = [
            ("state_set", host.op_state_set(&i, r#"{"key":"k","schema":1,"value":1}"#)),
            (
                "require_approval",
                host.op_require_approval(&i, r#"{"resource":"process.run","verbs":["start"]}"#),
            ),
            (
                "service_publish",
                host.op_service_publish(
                    &i,
                    r#"{"key":{"scope":["root"],"name":"svc"},"version":1,"deps":[]}"#,
                ),
            ),
            (
                "contribution_publish",
                host.op_contribution_publish(&i, r#"{"kind":"theme","name":"t","overlay":{}}"#),
            ),
        ];
        for (label, res) in cases {
            assert_eq!(
                res,
                Err(STALE_GENERATION.into()),
                "{label} must reject a generation retired before its commit point"
            );
        }
        assert_eq!(host.rejected_stale_effects(), 4);
        drop(host);
        teardown(dir, queue);
    }

    /// The fence must not over-reject: a current generation's mutations commit
    /// normally.
    #[test]
    fn mutating_ops_commit_for_a_current_generation() {
        let (dir, queue, host) = host_with_generation("commit-ok");
        let i = info();
        host.op_state_set(&i, r#"{"key":"k","schema":1,"value":1}"#)
            .unwrap();
        host.op_service_publish(
            &i,
            r#"{"key":{"scope":["root"],"name":"svc"},"version":1,"deps":[]}"#,
        )
        .unwrap();
        host.op_contribution_publish(&i, r#"{"kind":"theme","name":"t","overlay":{}}"#)
            .unwrap();
        assert_eq!(host.rejected_stale_effects(), 0);
        assert_eq!(host.services.lock().unwrap().snapshot().len(), 1);
        assert_eq!(host.published_contributions(1).len(), 1);
        drop(host);
        teardown(dir, queue);
    }

    /// Decision 28: `contribution_publish` accepts `"kind":"settings"` and
    /// stages a typed settings contribution; a malformed payload is a typed
    /// error that stages nothing (no partial publish).
    #[test]
    fn contribution_publish_settings_parses_and_rejects_malformed() {
        let (dir, queue, host) = host_with_generation("settings");
        let i = info();
        host.op_contribution_publish(
            &i,
            r#"{"kind":"settings","provider":{"protocol":"openai","base_url":"https://x"},"approval":{"auto_approve":false,"yolo":false}}"#,
        )
        .unwrap();
        let published = host.published_contributions(1);
        assert_eq!(published.len(), 1);
        match &published[0].kind {
            ContributionKind::Settings(s) => {
                let p = s.provider.as_ref().expect("provider parsed");
                assert_eq!(p.protocol.as_deref(), Some("openai"));
                assert_eq!(p.base_url.as_deref(), Some("https://x"));
                assert_eq!(p.model, None, "unset fields default to None");
                let a = s.approval.as_ref().expect("approval parsed");
                assert_eq!(a.auto_approve, Some(false));
                assert_eq!(a.yolo, Some(false));
            }
            other => panic!("expected a settings contribution, got {other:?}"),
        }

        // A well-formed key reference (externally tagged) parses too.
        host.op_contribution_publish(
            &i,
            r#"{"kind":"settings","provider":{"key":{"Env":{"name":"OPENAI_API_KEY"}}}}"#,
        )
        .unwrap();
        assert_eq!(host.published_contributions(1).len(), 2);

        // Malformed: unknown key-reference shape and a non-object provider.
        for bad in [
            r#"{"kind":"settings","provider":{"key":{"Bogus":{}}}}"#,
            r#"{"kind":"settings","provider":42}"#,
            r#"{"kind":"settings","approval":{"auto_approve":"yes"}}"#,
        ] {
            let err = host.op_contribution_publish(&i, bad).unwrap_err();
            assert!(
                err.starts_with("contribution_publish:"),
                "typed error for {bad}: {err}"
            );
        }
        assert_eq!(
            host.published_contributions(1).len(),
            2,
            "malformed payloads stage nothing"
        );
        drop(host);
        teardown(dir, queue);
    }

    /// T9: op 7 accepts a `"kind":"hook"` payload, stages a hook
    /// contribution, and records a resolvable `(kind, name) → generation`
    /// mapping; malformed payloads are typed errors that stage nothing.
    #[test]
    fn contribution_publish_hook_parses_and_rejects_malformed() {
        let (dir, queue, host) = host_with_generation("hook");
        let i = info();
        host.op_contribution_publish(
            &i,
            r#"{"kind":"hook","name":"guard_mod","hook":"on_turn_start"}"#,
        )
        .unwrap();
        let published = host.published_contributions(1);
        assert_eq!(published.len(), 1);
        match &published[0].kind {
            ContributionKind::Hook(h) => {
                assert_eq!(h.name, "guard_mod");
                assert_eq!(h.hook, HookKind::OnTurnStart);
            }
            other => panic!("expected a hook contribution, got {other:?}"),
        }
        assert_eq!(
            host.hook_generation(&i.scope, HookKind::OnTurnStart, "guard_mod"),
            Some(1)
        );
        assert_eq!(
            host.hook_generation(&i.scope, HookKind::OnToolIntent, "guard_mod"),
            None
        );

        // A blank name is filled with the module's stable id.
        host.op_contribution_publish(
            &i,
            r#"{"kind":"hook","name":"","hook":"on_tool_intent"}"#,
        )
        .unwrap();
        assert_eq!(
            host.hook_generation(&i.scope, HookKind::OnToolIntent, &i.module_id.to_string()),
            Some(1)
        );

        // Malformed payloads stage nothing and record no mapping.
        for bad in [
            r#"{"kind":"hook","name":"x"}"#,
            r#"{"kind":"hook","name":"x","hook":"bogus"}"#,
        ] {
            let err = host.op_contribution_publish(&i, bad).unwrap_err();
            assert!(
                err.starts_with("contribution_publish:"),
                "typed error for {bad}: {err}"
            );
        }
        assert_eq!(host.published_contributions(1).len(), 2);
        drop(host);
        teardown(dir, queue);
    }

    /// T9: disposal drops a generation's hook records.
    #[test]
    fn drop_generation_contributions_drops_hooks() {
        let (dir, queue, host) = host_with_generation("hook-drop");
        let i = info();
        host.op_contribution_publish(
            &i,
            r#"{"kind":"hook","name":"guard_mod","hook":"on_turn_start"}"#,
        )
        .unwrap();
        assert_eq!(
            host.hook_generation(&i.scope, HookKind::OnTurnStart, "guard_mod"),
            Some(1)
        );
        host.drop_generation_contributions(1);
        assert_eq!(
            host.hook_generation(&i.scope, HookKind::OnTurnStart, "guard_mod"),
            None
        );
        assert!(host.published_contributions(1).is_empty());
        drop(host);
        teardown(dir, queue);
    }

    /// T9/E: hooks are keyed by the full `(scope, kind, name)`, so same-named
    /// hooks in different scopes coexist; a same-scope collision from a
    /// different live generation is refused; retiring one generation drops
    /// only its own entry.
    #[test]
    fn hooks_are_scope_keyed_and_teardown_isolated() {
        let a = ScopePath(vec!["a".into()]);
        let b = ScopePath(vec!["b".into()]);
        let (dir, queue, host) = host_with_generations(
            "hook-scope",
            vec![(1, a.clone(), Id128::generate()), (2, b.clone(), Id128::generate())],
        );
        let info_for = |generation: u64, scope: &ScopePath| TokenInfo {
            generation,
            module_id: Id128::generate(),
            scope: scope.clone(),
            deps: Vec::new(),
            state_key: None,
            state_schema: None,
            supersede: HashSet::new(),
        };
        host.op_contribution_publish(
            &info_for(1, &a),
            r#"{"kind":"hook","name":"same","hook":"on_turn_start"}"#,
        )
        .unwrap();
        host.op_contribution_publish(
            &info_for(2, &b),
            r#"{"kind":"hook","name":"same","hook":"on_turn_start"}"#,
        )
        .unwrap();
        assert_eq!(host.hook_generation(&a, HookKind::OnTurnStart, "same"), Some(1));
        assert_eq!(host.hook_generation(&b, HookKind::OnTurnStart, "same"), Some(2));

        // A collision in the same scope from a different live generation is
        // refused instead of clobbering the holder.
        let err = host
            .op_contribution_publish(
                &info_for(2, &a),
                r#"{"kind":"hook","name":"same","hook":"on_turn_start"}"#,
            )
            .unwrap_err();
        assert!(err.contains("already held"), "got: {err}");

        // Retiring generation 1 drops only its own entry.
        host.drop_generation_contributions(1);
        assert_eq!(host.hook_generation(&a, HookKind::OnTurnStart, "same"), None);
        assert_eq!(host.hook_generation(&b, HookKind::OnTurnStart, "same"), Some(2));
        drop(host);
        teardown(dir, queue);
    }

    /// A host with the given `(generation, scope, module_id)` tokens registered.
    fn host_with_generations(
        tag: &str,
        generations: Vec<(u64, ScopePath, Id128)>,
    ) -> (PathBuf, Arc<DurabilityQueue>, ModuleHost) {
        let dir = std::env::temp_dir().join(format!("kb-host-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let queue = Arc::new(DurabilityQueue::start(&format!("kb-host-{tag}")));
        let tokens: Arc<RwLock<HashMap<u64, TokenInfo>>> = Arc::new(RwLock::new(HashMap::new()));
        {
            let mut tokens = tokens.write().expect("tokens lock poisoned");
            for (generation, scope, module_id) in generations {
                tokens.insert(
                    generation,
                    TokenInfo {
                        generation,
                        module_id,
                        scope,
                        deps: Vec::new(),
                        state_key: None,
                        state_schema: None,
                        supersede: HashSet::new(),
                    },
                );
            }
        }
        let currency: Arc<dyn Fn(u64) -> bool + Send + Sync> = {
            let tokens = Arc::clone(&tokens);
            Arc::new(move |g| tokens.read().expect("tokens lock poisoned").contains_key(&g))
        };
        let state = StateStore::open(&dir, Arc::clone(&queue), Arc::clone(&currency));
        let host = ModuleHost::new(
            Id128::generate(),
            tokens,
            Weak::new(),
            Arc::new(Mutex::new(ServiceRegistry::new())),
            Arc::new(Mutex::new(state)),
            Arc::new(AtomicU64::new(0)),
            currency,
        );
        (dir, queue, host)
    }
}
