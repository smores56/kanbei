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
//! | 7 | `contribution_publish` | `{"kind": "ui"|"theme", ...}` | `"ok"` |
//! | 6 | `service_publish` | `{"key": <ServiceKey>, "version": <u32>, "deps": [<ServiceDependency>]}` | `"ok"` |
//!
//! M2 keeps state bytes as the compact JSON encoding of the value the module
//! wrote. `service_call` is synchronous and shallow: one hop to the provider
//! generation's cached `kb_hot`, recursion depth cap 8, no delegation.
//! `service_publish` is an M2 extension of the kernel op set (the module
//! publishes its services during `kb_on_activate`; R-25/C-06 publication is
//! the key free or an explicit same-module replace intent).
//!
//! `check` passes the broker's current policy version (the highest version
//! across registered templates; the session lane owns template mutations).

use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};

use kanbei_capabilities::{ApprovalIntent, Broker, Capability, GrantScope, Principal};
use kanbei_core::id::Id128;
use kanbei_scopes::contrib::{Contribution, ContributionKind, ThemeContribution, UiMountContribution};
use kanbei_services::{
    ReplaceIntent, ScopePath, ServiceContract, ServiceDependency, ServiceError, ServiceKey,
    ServiceProvider, ServiceRegistry,
};
use kanbei_vm::{Host, Instance};
use serde_json::{json, Value};

use crate::state::{StateStore, StateUpdate};

/// The exact string kanbei-vm maps to `GuestError::StaleGeneration`
/// (kanbei-vm's `STALE_GENERATION` const is crate-private; the contract is
/// frozen).
const STALE_GENERATION: &str = "stale generation";

/// `service_call` recursion cap (one hop per level; M2 is shallow).
const MAX_SERVICE_DEPTH: u32 = 8;

thread_local! {
    static SERVICE_DEPTH: Cell<u32> = const { Cell::new(0) };
}

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
    instances: Weak<Mutex<HashMap<u64, Arc<Mutex<Instance>>>>>,
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
}

impl ModuleHost {
    /// The manager constructs the host with the shared tables (see
    /// `ModuleManager::new`); the session lane reaches it via the manager.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        session: Id128,
        tokens: Arc<RwLock<HashMap<u64, TokenInfo>>>,
        instances: Weak<Mutex<HashMap<u64, Arc<Mutex<Instance>>>>>,
        services: Arc<Mutex<ServiceRegistry>>,
        state: Arc<Mutex<StateStore>>,
        rejected_stale_effects: Arc<AtomicU64>,
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
        }
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
        let update = StateUpdate {
            key: key.to_string(),
            schema,
            bytes,
            generation: info.generation,
        };
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
                "service_call: a generation may not call its own service (re-entrant instance lock)"
                    .into(),
            );
        }
        if SERVICE_DEPTH.with(|d| d.get()) >= MAX_SERVICE_DEPTH {
            return Err(format!(
                "service_call: recursion depth cap ({MAX_SERVICE_DEPTH}) exceeded"
            ));
        }
        SERVICE_DEPTH.with(|d| d.set(d.get() + 1));
        let result = (|| {
            let map = self.instances.upgrade().ok_or_else(|| {
                "service_call: kernel module table is gone (hosting shut down)".to_string()
            })?;
            let instance = map
                .lock()
                .expect("instances lock poisoned")
                .get(&provider.generation)
                .cloned()
                .ok_or_else(|| {
                    format!(
                        "service_call: provider generation {} is not live",
                        provider.generation
                    )
                })?;
            drop(map);
            let mut inst = instance
                .lock()
                .map_err(|_| "service_call: provider instance lock poisoned".to_string())?;
            inst.call_json("kb_hot", &args.to_string()).map_err(|e| {
                format!(
                    "service_call: provider generation {} failed: {e}",
                    provider.generation
                )
            })
        })();
        SERVICE_DEPTH.with(|d| d.set(d.get() - 1));
        result
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
        self.broker
            .lock()
            .expect("broker lock poisoned")
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
        let intent = self
            .broker
            .lock()
            .expect("broker lock poisoned")
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
    /// - `{"kind":"ui","name":<string>,"component":<string>}`
    /// - `{"kind":"theme","name":<string>,"overlay":<object>}`
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
                self.ui_components
                    .lock()
                    .expect("ui components lock poisoned")
                    .insert(component.clone(), info.generation);
                Contribution {
                    scope: info.scope.clone(),
                    kind: ContributionKind::UiMount(UiMountContribution { name, component }),
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
            other => return Err(format!("contribution_publish: unknown kind {other:?}")),
        };
        self.contributions
            .lock()
            .expect("contributions lock poisoned")
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
}
