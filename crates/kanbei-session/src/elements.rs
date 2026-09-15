//! Module subsystem: config activation, generation replacement, effect dispatch, state-head CAS, retention, and UI staleness.

use crate::{ConfigActivation, ConfigLayer, Session, FaultPoint, NewEvent, SessionError};
use kanbei_core::digest::Digest;
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
use kanbei_scopes::registry::OverridePlan;
use kanbei_scopes::registry::contribution_override_key;
use kanbei_services::ScopePath;
use kanbei_services::ServiceKey;
use kanbei_services::ServiceProvider;
use kanbei_vm::Host;
use serde_json::json;
use std::collections::HashSet;

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
        let mut activated: Vec<(PackageManifest, Vec<Contribution>)> = Vec::new();
        let mut builtin_active = false;
        let mut safe_reason: Option<String> = None;
        for manifest in layers {
            let is_builtin = manifest.origin == ModuleOrigin::Builtin;
            match self.activate_config(manifest.clone()) {
                Ok(ca) => {
                    builtin_active |= is_builtin;
                    let contributions = self
                        .modules
                        .as_ref()
                        .map(|m| m.published_contributions(ca.generation))
                        .unwrap_or_default();
                    activated.push((manifest, contributions));
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
            // The layers safe mode is about to drop: every activated
            // non-builtin layer. Captured before the retain so the canonical
            // removal event can name their generations/packages (F7/A1).
            let dropped_layers: Vec<ConfigLayer> = self
                .config_layers
                .iter()
                .filter(|l| l.rank != 0)
                .cloned()
                .collect();
            // Drop every activated non-builtin layer (best-effort: a layer with
            // dependents stays, but safe mode is still recorded).
            if let Some(manager) = self.modules.as_mut() {
                for (m, _) in activated
                    .iter()
                    .rev()
                    .filter(|(m, _)| m.origin != ModuleOrigin::Builtin)
                {
                    let _ = manager.deactivate(m.module_id);
                }
            }
            // The dropped layers' generations are no longer active: forget their
            // precedence records so a later publish cannot try to override them.
            self.config_layers.retain(|l| l.rank == 0);
            if !builtin_active {
                let fallback = crate::builtin_config::builtin_config_manifest();
                match self.activate_config(fallback.clone()) {
                    Ok(ca) => {
                        let contributions = self
                            .modules
                            .as_ref()
                            .map(|m| m.published_contributions(ca.generation))
                            .unwrap_or_default();
                        activated.push((fallback, contributions));
                    }
                    // The built-in itself could not activate: storage-only.
                    Err(_) => {
                        self.modules = None;
                        self.vm_engine_digest = None;
                    }
                }
            }
            // Safe-mode residue (A2a): overlay kinds merge and cannot be
            // un-merged by name, so a dropped layer's settings/theme residue
            // would otherwise survive in the registry. Remove the dropped
            // layers' non-overlay contributions and replay the surviving
            // built-in layer's overlays from scratch.
            let dropped: Vec<Contribution> = activated
                .iter()
                .filter(|(m, _)| m.origin != ModuleOrigin::Builtin)
                .flat_map(|(_, c)| c.iter().cloned())
                .collect();
            let surviving_overlays: Vec<Contribution> = activated
                .iter()
                .filter(|(m, _)| m.origin == ModuleOrigin::Builtin)
                .flat_map(|(_, c)| c.iter().cloned())
                .filter(|c| {
                    matches!(
                        c.kind,
                        ContributionKind::Settings(_) | ContributionKind::Theme(_)
                    )
                })
                .collect();
            let to_remove: Vec<Contribution> = dropped
                .iter()
                .filter(|c| {
                    !matches!(
                        c.kind,
                        ContributionKind::Settings(_)
                            | ContributionKind::Theme(_)
                            | ContributionKind::Service(_)
                    )
                })
                .cloned()
                .collect();
            let _ = self.registry.remove_contributions(&to_remove);
            self.registry
                .recompose_overlays(&crate::builtin_config::root_scope(), &surviving_overlays);
            // F7/A1: make the safe-mode drop CANONICAL. The registry mutation
            // above cannot be expressed as an OCC publish (settings/theme
            // overlays are merge-only and their removal needs a full-scope
            // recompose), so re-seed `CompositionStore` from the post-mutation
            // registry — its digest/contributions then match the registry —
            // and commit one `composition_changed` recording the dropped
            // generations. After this, the canonical log, the composition,
            // and the config identity all agree: no later fork/branch can pin
            // the stale pre-drop composition or restore a dropped layer.
            self.composition.reseed(&self.registry);
            self.config_digest = self.config_layers.last().map(|l| l.package);
            self.config_manifest = self.config_layers.last().map(|l| l.manifest.clone());
            if !dropped_layers.is_empty() {
                let epoch = self.composition.current().epoch;
                let composition_digest = self.composition.current().digest;
                let comp_bytes = self.composition.current().to_canonical_bytes();
                self.store.install(&comp_bytes)?;
                let removed: Vec<serde_json::Value> = dropped_layers
                    .iter()
                    .map(|l| {
                        json!({
                            "module_id": l.module_id.to_string(),
                            "generation": l.generation,
                            "package": l.package.to_string(),
                        })
                    })
                    .collect();
                let refs: Vec<Digest> = dropped_layers
                    .iter()
                    .map(|l| l.package)
                    .chain(std::iter::once(composition_digest))
                    .collect();
                self.commit(
                    vec![NewEvent {
                        kind: "composition_changed".into(),
                        payload_schema: 1,
                        payload: json!({
                            "epoch": epoch,
                            "delta": { "added": [], "removed": removed },
                            "scope": crate::builtin_config::root_scope().to_string(),
                            "initiator": "config",
                        }),
                        objects: Vec::new(),
                        refs,
                    }],
                    Some(composition_digest),
                )?;
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
    /// the result as the session's wiring. When a source is configured it
    /// FULLY determines the wiring (F10): each of
    /// `provider_engine`/`provider`/`broker`/`approval_resolver` is assigned
    /// on every apply, `Some` or `None`, so a higher layer that clears a field
    /// (e.g. turns yolo off) uninstalls the earlier wiring instead of leaving
    /// it stuck. No-op when no source is configured — the `SessionConfig`
    /// values are left exactly as the bootstrap set them.
    ///
    /// `session_id` is the one exception: it is only overwritten when the
    /// resolver supplies one. The session identity is committed as the project
    /// registry's `created_session` pin BEFORE config activation (open), so a
    /// config-derived id can diverge from that pin — a known limitation, kept
    /// because reordering open (activate before the identity pin) would ripple
    /// through the memory substrate. The yolo broker is keyed to whatever id
    /// the resolver returns, so the wiring itself stays consistent.
    ///
    /// Called after a layer's composition publishes and before its canonical
    /// commit, so the config event's post-manifest pins the resolved
    /// `provider_config` (decision 28 ordering).
    fn apply_settings(&mut self) {
        let Some(source) = self.cfg.settings.clone() else {
            return;
        };
        let resolved = source.resolve(&self.host_settings);
        self.provider = resolved.provider_engine;
        self.provider_config = resolved.provider;
        self.broker = resolved.broker.unwrap_or_default();
        self.approval_resolver = resolved.approval_resolver;
        if let Some(id) = resolved.session_id {
            self.session_id = id;
        }
    }

    /// Rebuilds the merged settings overlays after a config module replacement
    /// (F6). Settings are a merge-only overlay: precedence displacement cannot
    /// un-merge a replaced generation's contribution, so a stale `yolo`/
    /// `auto_approve` would survive a swap to a benign generation. Replay the
    /// surviving layers' overlays from scratch (their settings AND themes, so
    /// the recompose does not drop a surviving theme), including the new
    /// generation the caller already recorded in `config_layers`, then
    /// re-resolve the runtime wiring. `old_settings_scopes` names the scopes
    /// the replaced generation contributed settings to, so a scope that no
    /// surviving layer touches is cleared too.
    fn recompose_settings_after_replace(&mut self, old_settings_scopes: Vec<ScopePath>) {
        let Some(manager) = self.modules.as_ref() else {
            return;
        };
        let mut overlays: Vec<Contribution> = Vec::new();
        let mut scopes = old_settings_scopes;
        for layer in &self.config_layers {
            for c in manager.published_contributions(layer.generation) {
                match &c.kind {
                    ContributionKind::Settings(_) => {
                        scopes.push(c.scope.clone());
                        overlays.push(c);
                    }
                    ContributionKind::Theme(_) => overlays.push(c),
                    _ => {}
                }
            }
        }
        scopes.sort_by_key(|s| s.to_string());
        scopes.dedup();
        for scope in scopes {
            self.registry.recompose_overlays(&scope, &overlays);
        }
        self.host_settings = self
            .registry
            .settings_for(&crate::builtin_config::root_scope())
            .cloned()
            .unwrap_or_default();
        self.apply_settings();
    }

    /// Commits the canonical `safe_mode_activated` fact with the failure reason.
    pub(crate) fn commit_safe_mode(&mut self, reason: &str) -> Result<(), SessionError> {
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
        // Decision 28 service precedence: this layer may take over the service
        // keys held by the currently-active config layers whose origin ranks
        // strictly lower. Compute them BEFORE activation so the guest's
        // `kb_on_activate` publications are allowed to displace those holders
        // instead of tripping the conflict trap.
        let my_rank = manifest.origin.precedence_rank();
        // Capture both the allowed keys and the displaced holders so the
        // displaced provider can flow into the `OverridePlan` below (the shared
        // registry holder changes during activation, so it can no longer be
        // re-derived from a post-activation snapshot).
        let mut supersede: HashSet<ServiceKey> = HashSet::new();
        let mut superseded: Vec<(ServiceKey, ServiceProvider)> = Vec::new();
        {
            let reg = self.services.lock().expect("services lock poisoned");
            for (key, provider, _) in reg.snapshot() {
                let lower = self
                    .config_layers
                    .iter()
                    .any(|l| l.rank < my_rank && l.generation == provider.generation);
                if lower {
                    supersede.insert(key.clone());
                    superseded.push((key, provider));
                }
            }
        }
        let Some(manager) = self.modules.as_mut() else {
            return Err(SessionError::ModulesDisabled);
        };
        // 4 — activate; kb_on_activate publishes into the shared registry.
        let generation = manager.activate_with_supersede(&manifest, &supersede)?;
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
        // (UI mounts, theme overlays) join the same atomic publish. Settings
        // are trust-gated here (F2): an untrusted layer's sensitive fields are
        // stripped before they can enter the merged overlay or the canonical
        // commit.
        let mut published = manager.published_contributions(generation.generation);
        gate_settings_contributions(manifest.origin, &mut published);
        staged.contributions.extend(published);

        // Decision 28 precedence plan. The delta IS the module's own
        // pre-publication into the shared registry: displace it in the atomic
        // apply instead of removing it beforehand. The atomic guarantee here
        // is registry/epoch-level: `publish_planned` mutates the registry (and
        // the shared service registry the host already published into) only
        // once the OCC epoch check passes, so a stale-epoch publish mutates no
        // registry state. The session path is not fully zero-mutation, though:
        // `kb_on_activate` publishes services into the SHARED service registry
        // before the epoch check, so a stale publish still relies on the
        // module deactivate below to roll those registrations back (best
        // effort — a deferred/dependent provider may survive). There is no
        // deterministic session-path stale-epoch test: activation is
        // synchronous and single-writer, so nothing can advance the
        // composition between `stage` and `publish_planned`; the epoch CAS is
        // exercised directly at the composition/registry layer instead.
        let mut plan = OverridePlan {
            removed: delta
                .iter()
                .map(|(key, provider)| Contribution {
                    scope: manifest.scope.clone(),
                    kind: ContributionKind::Service(ServiceContribution {
                        key: key.clone(),
                        provider: provider.clone(),
                        deps: manifest.deps.clone(),
                    }),
                })
                .collect(),
        };
        // Precedence-driven implicit replacement: a higher-origin layer takes
        // over the identity keys held by lower-precedence active layers.
        let staged_keys: HashSet<(kanbei_services::ScopePath, String)> = staged
            .contributions
            .iter()
            .filter_map(contribution_override_key)
            .collect();
        let lower_generations: Vec<u64> = self
            .config_layers
            .iter()
            .filter(|l| l.rank < my_rank)
            .map(|l| l.generation)
            .collect();
        for lower_generation in lower_generations {
            for c in manager.published_contributions(lower_generation) {
                if contribution_override_key(&c).is_some_and(|k| staged_keys.contains(&k)) {
                    plan.removed.push(c);
                }
            }
            let lower_services: Vec<Contribution> = self
                .services
                .lock()
                .expect("services lock poisoned")
                .snapshot()
                .into_iter()
                .filter(|(_, p, _)| p.generation == lower_generation)
                .map(|(key, provider, deps)| Contribution {
                    scope: key.scope.clone(),
                    kind: ContributionKind::Service(ServiceContribution {
                        key,
                        provider,
                        deps,
                    }),
                })
                .collect();
            for c in lower_services {
                if contribution_override_key(&c).is_some_and(|k| staged_keys.contains(&k)) {
                    plan.removed.push(c);
                }
            }
        }
        // The captured displaced holders: a takeover already re-pointed the
        // shared registry, so those lower providers can no longer be re-derived
        // from the post-activation snapshot above. Add them to the plan (when the
        // staged set actually occupies their key) so `apply_planned`'s
        // force-remove-then-publish ordering displaces the old holder and
        // publishes exactly the staged replacement.
        for (key, provider) in &superseded {
            let c = Contribution {
                scope: key.scope.clone(),
                kind: ContributionKind::Service(ServiceContribution {
                    key: key.clone(),
                    provider: provider.clone(),
                    deps: manifest.deps.clone(),
                }),
            };
            if contribution_override_key(&c).is_some_and(|k| staged_keys.contains(&k)) {
                plan.removed.push(c);
            }
        }
        // 6+7 — validate, epoch-check, and apply atomically (removals + the
        // staged additions); stale → roll back.
        if let Err(e) = self
            .composition
            .publish_planned(&staged, &mut self.registry, &plan)
        {
            let reason = e.to_string();
            // Rollback for the pre-epoch-check service publications: the
            // activation's `kb_on_activate` already published into the shared
            // registry, so a stale-epoch (or otherwise rejected) publish must
            // deactivate the generation to remove those registrations. The
            // typed registry maps themselves were never mutated
            // (`publish_planned` applies on a clone), so this is the only
            // session-path residue — best effort, since a dependent provider
            // may keep the registration alive.
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
        // Track this layer for later precedence-driven implicit replacement
        // (decision 28) and as part of the ordered restore stack (F5): its
        // rank and generation resolve which of its contributions a
        // higher-origin publish may take over; its package digest is the
        // restore identity.
        let module_id = manifest.module_id;
        self.config_layers.push(ConfigLayer {
            rank: my_rank,
            module_id,
            generation: generation.generation,
            package,
            manifest,
        });
        Ok(ConfigActivation {
            module_id,
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
        let old_settings_scopes: Vec<ScopePath> = old_published
            .iter()
            .filter(|c| matches!(c.kind, ContributionKind::Settings(_)))
            .map(|c| c.scope.clone())
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
        // composition and the new generation's own contributions join the
        // staged set — a replaced UI module re-mounts under its new
        // generation. Both the new generation's pre-publication and the old
        // generation's displaced entries are removed in the same atomic apply
        // as the additions (decision 28), so no registry mutation happens
        // before the OCC epoch check.
        let mut plan = OverridePlan {
            removed: new_entries
                .iter()
                .map(|(key, provider, deps)| Contribution {
                    scope: new_manifest.scope.clone(),
                    kind: ContributionKind::Service(ServiceContribution {
                        key: key.clone(),
                        provider: provider.clone(),
                        deps: deps.clone(),
                    }),
                })
                .collect(),
        };
        plan.removed.extend(old_published);
        staged
            .contributions
            .extend(manager.published_contributions(new_generation));
        if let Err(e) = self
            .composition
            .publish_planned(&staged, &mut self.registry, &plan)
        {
            let _ = manager.deactivate(module_id);
            let _ = self.rebind_ui(new_generation);
            return Err(e.into());
        }
        // F6: a replaced config layer's settings overlay is merge-only, so the
        // precedence plan cannot un-merge a stale yolo/auto_approve. Record the
        // new generation as the active layer and rebuild the merged settings
        // from the surviving layers before the canonical commit (so the
        // post-manifest pins the refreshed wiring).
        let is_config_layer = self
            .config_layers
            .iter_mut()
            .find(|l| l.module_id == module_id)
            .map(|layer| {
                layer.generation = new_generation;
                layer.package = new_package;
                layer.manifest = new_manifest.clone();
            })
            .is_some();
        if is_config_layer {
            self.recompose_settings_after_replace(old_settings_scopes);
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

// ---------- settings trust gate (F2) ----------

/// The origins trusted to publish sensitive settings fields (F2). Everything
/// else (`WorkspaceConfig`, `Agent`, `UserInstalled`) is repo- or agent-
/// supplied: a cloned workspace can otherwise auto-approve tools
/// (`approval.yolo`/`auto_approve`) or exfiltrate a secret
/// (`provider.base_url`/`provider.key`) through the CLI's settings source.
fn origin_is_trusted_for_settings(origin: ModuleOrigin) -> bool {
    matches!(origin, ModuleOrigin::Builtin | ModuleOrigin::UserConfig)
}

/// Filters the settings contributions a generation publishes before they are
/// staged/merged (F2).
///
/// Every layer's `provider.base_url` is validated here — an invalid value
/// (not `http`/`https` with a host) is dropped so a malformed URL can never
/// drive the engine. Untrusted layers additionally have every sensitive field
/// removed: `approval.auto_approve`, `approval.yolo`, `provider.base_url`,
/// `provider.key`. Non-sensitive fields (`provider.model`,
/// `provider.protocol`, `provider.fake`) pass through for every origin.
fn gate_settings_contributions(origin: ModuleOrigin, contributions: &mut [Contribution]) {
    let trusted = origin_is_trusted_for_settings(origin);
    for c in contributions.iter_mut() {
        let ContributionKind::Settings(settings) = &mut c.kind else {
            continue;
        };
        if let Some(provider) = settings.provider.as_mut() {
            if provider.base_url.as_deref().is_some_and(|u| !valid_base_url(u)) {
                provider.base_url = None;
            }
            if !trusted {
                provider.base_url = None;
                provider.key = None;
            }
        }
        if !trusted
            && let Some(approval) = settings.approval.as_mut()
        {
            approval.auto_approve = None;
            approval.yolo = None;
        }
    }
}

/// `http`/`https` with a non-empty host. Deliberately structural (no `url`
/// dependency): scheme `://`, and a host before any path/query/fragment.
fn valid_base_url(url: &str) -> bool {
    let Some((scheme, rest)) = url.split_once("://") else {
        return false;
    };
    if scheme != "http" && scheme != "https" {
        return false;
    }
    let authority = rest
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    let host = authority.rsplit('@').next().unwrap_or(authority);
    !host.is_empty()
}
