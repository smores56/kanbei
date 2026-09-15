//! Module subsystem: config activation, generation replacement, effect dispatch, state-head CAS, retention, and UI staleness.

use crate::{ConfigActivation, Session, FaultPoint, NewEvent, SessionError};
use kanbei_core::id::Id128;
use kanbei_modules::HeadFile;
use kanbei_modules::ModuleError;
use kanbei_modules::ModuleOrigin;
use kanbei_modules::PackageManifest;
use kanbei_modules::ReplacementOutcome;
use kanbei_modules::StateUpdate;
use kanbei_policy::Admission;
use kanbei_policy::BoundaryKind;
use kanbei_policy::Candidate;
use kanbei_scopes::contrib::Contribution;
use kanbei_scopes::contrib::ContributionKind;
use kanbei_scopes::contrib::ServiceContribution;
use kanbei_services::ServiceKey;
use kanbei_services::ServiceProvider;
use kanbei_vm::Host;
use serde_json::json;

impl Session {
    /// Activates the open-time desired-state config layers in LOW→HIGH
    /// precedence order (decision 28). Each layer activates independently
    /// through [`Self::activate_config`]; the registry's field-wise settings
    /// overlay supplies built-in-defaults-overridden-by-higher fields, so a
    /// higher layer that only sets some fields never clobbers the rest.
    ///
    /// Safe mode (R-01/C-02): a failing NON-builtin layer drops the failed and
    /// every already-activated non-builtin layer, keeps (or activates) the
    /// built-in generation, commits the canonical `safe_mode_activated` fact,
    /// and leaves `modules` enabled — the session stays usable and
    /// `host_settings` reflects the built-in layer. A failing built-in layer
    /// (or no Wasm) drops modules to storage-only.
    ///
    /// Whatever the outcome, the merged settings are snapshotted on the
    /// session (they survive generation teardown).
    pub(crate) fn activate_config_layers(
        &mut self,
        layers: Vec<PackageManifest>,
    ) -> Result<(), SessionError> {
        let mut activated: Vec<PackageManifest> = Vec::new();
        let mut builtin_active = false;
        let mut safe_reason: Option<String> = None;
        for manifest in layers {
            let is_builtin = manifest.origin == ModuleOrigin::Builtin;
            match self.activate_config(manifest.clone()) {
                Ok(_) => {
                    builtin_active |= is_builtin;
                    activated.push(manifest);
                }
                // A non-builtin failure is safe-mode-able; the failed layer is
                // already deactivated by `activate_config`.
                Err(e) if !is_builtin => {
                    safe_reason = Some(e.to_string());
                    break;
                }
                // A built-in failure is not recoverable by keeping built-ins.
                Err(e) => {
                    self.modules = None;
                    self.vm_engine_digest = None;
                    self.commit_safe_mode(&e.to_string())?;
                    return Ok(());
                }
            }
        }
        if let Some(reason) = safe_reason {
            // Drop every activated non-builtin layer (best-effort: a layer with
            // dependents stays, but safe mode is still recorded).
            if let Some(manager) = self.modules.as_mut() {
                for m in activated
                    .iter()
                    .rev()
                    .filter(|m| m.origin != ModuleOrigin::Builtin)
                {
                    let _ = manager.deactivate(m.module_id);
                }
            }
            if !builtin_active {
                let fallback = crate::builtin_config::builtin_config_manifest();
                if self.activate_config(fallback).is_err() {
                    // The built-in itself could not activate: storage-only.
                    self.modules = None;
                    self.vm_engine_digest = None;
                }
            }
            self.commit_safe_mode(&reason)?;
        }
        self.host_settings = self
            .registry
            .settings_for(&crate::builtin_config::root_scope())
            .cloned()
            .unwrap_or_default();
        self.apply_settings();
        Ok(())
    }

    /// Resolves the merged config-layer settings through
    /// [`SessionConfig::settings`](crate::SessionConfig::settings) and applies
    /// each `Some` field as an override of the bootstrap (`SessionConfig`)
    /// value; `None` fields fall back. No-op when no source is configured —
    /// today's behavior is preserved exactly.
    ///
    /// Called after a layer's composition publishes and before its canonical
    /// commit, so the config event's post-manifest pins the resolved
    /// `provider_config` (decision 28 ordering).
    fn apply_settings(&mut self) {
        let Some(source) = self.cfg.settings.clone() else {
            return;
        };
        let resolved = source.resolve(&self.host_settings);
        if let Some(engine) = resolved.provider_engine {
            self.provider = Some(engine);
        }
        if let Some(provider) = resolved.provider {
            self.provider_config = Some(provider);
        }
        if let Some(broker) = resolved.broker {
            self.broker = broker;
        }
        if let Some(resolver) = resolved.approval_resolver {
            self.approval_resolver = Some(resolver);
        }
        if let Some(id) = resolved.session_id {
            self.session_id = id;
        }
    }

    /// Commits the canonical `safe_mode_activated` fact with the failure reason.
    fn commit_safe_mode(&mut self, reason: &str) -> Result<(), SessionError> {
        self.commit(
            vec![NewEvent {
                kind: "safe_mode_activated".into(),
                payload_schema: 1,
                payload: json!({ "reason": reason }),
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            None,
        )?;
        Ok(())
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
        // Decision 28: re-snapshot the merged settings and resolve the runtime
        // wiring BEFORE the canonical commit, so this event's post-manifest
        // pins the settings-resolved `provider_config` (and the session runs
        // with the resolved engine/broker/resolver).
        self.host_settings = self
            .registry
            .settings_for(&crate::builtin_config::root_scope())
            .cloned()
            .unwrap_or_default();
        self.apply_settings();
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
        self.config_manifest = Some(manifest.clone());
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
    /// old generation and the added new one. Since M8 the replaced
    /// generation's UI mounts/theme overlays are removed from the
    /// composition (mid-session UI deactivation), the new generation's own
    /// contributions join the staged set, and the UI host rebinds — mounts
    /// of the removed generation unbind, the rest rebind in slot order.
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
        // M8 mid-session UI deactivation: capture the replaced generation's
        // non-service contributions (UI mounts / theme overlays staged via
        // `contribution_publish`) BEFORE the swap — `replace` drops the
        // generation's staging records.
        let old_published = manager.published_contributions(old_generation);
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
                    // The old generation is already gone: rebind so its UI
                    // mounts unbind (their components no longer resolve).
                    let _ = self.rebind_ui(new_generation);
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
        // M8: the replaced generation's UI mounts/theme overlays leave the
        // composition (clone-and-swap removal, idempotent), and the new
        // generation's own contributions join the staged set — a replaced UI
        // module re-mounts under its new generation.
        if let Err(e) = self.registry.remove_contributions(&old_published) {
            let _ = manager.deactivate(module_id);
            let _ = self.rebind_ui(new_generation);
            return Err(e.into());
        }
        staged
            .contributions
            .extend(manager.published_contributions(new_generation));
        if let Err(e) = self.registry.validate(&staged.contributions) {
            let _ = manager.deactivate(module_id);
            let _ = self.rebind_ui(new_generation);
            return Err(e.into());
        }
        if let Err(e) = self.composition.publish(&staged, &mut self.registry) {
            let _ = manager.deactivate(module_id);
            let _ = self.rebind_ui(new_generation);
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
            let _ = self.rebind_ui(new_generation);
            return Err(e);
        }
        // M8: rebind the UI host — the replaced generation's mounts unbind
        // (their components no longer resolve) and the remaining mounts
        // rebind in slot order.
        self.rebind_ui(new_generation)?;
        // Keep the retained config manifest (and its digest) in step with the
        // swap so `reset_module_state` reads the live binding (R-07/C-F1).
        if self
            .config_manifest
            .as_ref()
            .is_some_and(|m| m.module_id == module_id)
        {
            self.config_digest = Some(new_package);
            self.config_manifest = Some(new_manifest);
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

    /// `module reset-state` (R-07/C-07): start a fresh state head for a tracked
    /// module's bound `state_key`, discarding the current head, and record the
    /// reinitialization as a canonical `state_reinitialized` fact. M2 tracks
    /// only the activated config module. Returns the discarded head, if any.
    pub fn reset_module_state(
        &mut self,
        module_id: Id128,
    ) -> Result<Option<HeadFile>, SessionError> {
        let manifest = self
            .config_manifest
            .as_ref()
            .filter(|m| m.module_id == module_id)
            .cloned()
            .ok_or_else(|| {
                SessionError::InvalidInput(format!("no tracked module {module_id} to reset"))
            })?;
        let key = manifest.state_key.clone().ok_or_else(|| {
            SessionError::InvalidInput(format!("module {module_id} binds no state_key (R-07/C-F1)"))
        })?;
        let state = self
            .modules
            .as_ref()
            .ok_or(SessionError::ModulesDisabled)?
            .state();
        let previous = state.lock().expect("state lock poisoned").reset_head(&key)?;
        // A fresh head means no state_head pin; the discarded snapshot objects
        // fall out of the head and become GC-eligible. Commit the canonical fact
        // and, if it fails, restore the discarded head so the reset is
        // all-or-nothing (mirrors activate_config's rollback).
        let receipt = self.commit(
            vec![NewEvent {
                kind: "state_reinitialized".into(),
                payload_schema: 1,
                payload: json!({
                    "module_id": module_id.to_string(),
                    "state_key": key,
                    "previous_head": previous.as_ref().map(|h| h.digest.to_string()),
                }),
                objects: vec![],
                refs: vec![],
            }],
            None,
        );
        if let Err(e) = receipt {
            if let Some(head) = &previous {
                let _ = state
                    .lock()
                    .expect("state lock poisoned")
                    .restore_head(&key, head);
            }
            return Err(e);
        }
        Ok(previous)
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
}
