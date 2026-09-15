//! Typed contribution registries with kernel-owned fixed conflict rules
//! (R-19/A-11/C): modules contribute typed entries, never resolution logic;
//! one kernel staging/validation/publish protocol is shared by all domain
//! registries (commands/tools, services, UI slots, projection-stage slots,
//! keymap tables, themes, guards).
//!
//! Rules per type (docs/architecture.md):
//! - commands/tools: unique per (scope, name), or explicit replacement via
//!   the `replace_*` methods (generation replacement);
//! - services: one provider per scoped key (delegated to the
//!   `kanbei_services` registry);
//! - keymaps: layered match — duplicates are layers, lookup takes the last;
//! - themes: validated overlay — the overlay must be a JSON object and later
//!   overlays merge (shallowly) over earlier ones;
//! - projection stages: named slots with ordering constraints — a
//!   (scope, slot, ordering) triple is unique;
//! - UI: named mount points unique per (scope, name), or explicit replacement;
//! - guards: monotonic — a monotonic guard cannot be replaced by a
//!   non-monotonic one. Exact predicate-superset analysis is deferred; M2
//!   checks only the monotonic bit, and scope disposal still removes guards
//!   (disposal is the scope's lifecycle, not a guard re-registration).

use std::collections::hash_map::Entry;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use kanbei_services::{ScopePath, ServiceDependency, ServiceKey, ServiceProvider, ServiceRegistry};
use serde_json::Value;

use crate::contrib::{
    ApprovalSettings, CommandContribution, Contribution, ContributionKind, GuardContribution,
    KeyReference, KeymapContribution, ProjectionStageContribution, ProviderSettings,
    ServiceContribution, SettingsContribution, ThemeContribution, ToolContribution,
    UiMountContribution,
};
use crate::errors::ScopeError;

/// What a scope removal took with it: every removed contribution (the scope's
/// own plus force-cascaded dependent services) and the scopes that lost
/// services to the cascade.
#[derive(Debug, Clone, PartialEq)]
pub struct RemovedSet {
    pub contributions: Vec<Contribution>,
    pub cascaded_scopes: Vec<ScopePath>,
}

/// A transient precedence plan for one publish (decision 28): the
/// lower-precedence contributions this publish implicitly replaces. The plan
/// is a parameter to validate/publish/apply, never serialized, so the
/// composition digest domain for the existing kinds is unchanged.
///
/// `Contribution` carries no origin: the orchestrator (the session, which
/// knows each active generation's `ModuleOrigin`) resolves precedence and
/// supplies the displaced contributions here.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct OverridePlan {
    /// Lower-precedence contributions taken over by the staged set: removed
    /// from the composed set and the registry in the same atomic apply as the
    /// staged additions.
    pub removed: Vec<Contribution>,
}

impl OverridePlan {
    pub fn is_empty(&self) -> bool {
        self.removed.is_empty()
    }

    fn removed_service_keys(&self) -> HashSet<ServiceKey> {
        self.removed
            .iter()
            .filter_map(|c| match &c.kind {
                ContributionKind::Service(s) => Some(s.key.clone()),
                _ => None,
            })
            .collect()
    }
}

/// The `(scope, kind identity)` key a contribution occupies for precedence
/// replacement, or `None` for kinds that merge (see
/// [`ContributionKind::override_identity`]).
pub fn contribution_override_key(c: &Contribution) -> Option<(ScopePath, String)> {
    c.kind
        .override_identity()
        .map(|identity| (c.scope.clone(), identity))
}

/// The typed contribution registries.
///
/// `apply` is transactional for the registry's own maps: it builds the merged
/// next state on a clone and swaps it in only when every step succeeded. The
/// service registry is SHARED (the kernel's `Arc<Mutex<ServiceRegistry>>` —
/// the module host publishes into the same instance), so `apply`'s service
/// publications land in the shared registry immediately; a failure mid-`apply`
/// leaves earlier service publications visible until the caller's rollback
/// (the session deactivates the failing generation, which removes its
/// registrations). Non-service maps keep the clone-and-swap property.
#[derive(Debug)]
pub struct ContributionRegistry {
    commands: HashMap<(ScopePath, String), CommandContribution>,
    tools: HashMap<(ScopePath, String), ToolContribution>,
    /// Layered keymap table: order is the layer; lookup returns the last
    /// matching layer (R-19 "keymaps: layered match").
    keymaps: Vec<(ScopePath, KeymapContribution)>,
    /// Validated overlay view: one entry per (scope, name); later overlays
    /// merge (shallowly) over earlier ones (R-19 "themes: validated overlay").
    themes: HashMap<(ScopePath, String), ThemeContribution>,
    stages: HashMap<(ScopePath, String, u32), ProjectionStageContribution>,
    ui: HashMap<(ScopePath, String), UiMountContribution>,
    guards: HashMap<(ScopePath, String), GuardContribution>,
    /// Merged settings view: at most one effective entry per scope; later
    /// layers overlay field-wise over earlier ones (R-19).
    settings: HashMap<ScopePath, SettingsContribution>,
    services: Arc<Mutex<ServiceRegistry>>,
}

impl ContributionRegistry {
    pub fn new(services: Arc<Mutex<ServiceRegistry>>) -> Self {
        Self {
            commands: HashMap::new(),
            tools: HashMap::new(),
            keymaps: Vec::new(),
            themes: HashMap::new(),
            stages: HashMap::new(),
            ui: HashMap::new(),
            guards: HashMap::new(),
            settings: HashMap::new(),
            services,
        }
    }

    /// Validates a staged set against the current registrations and earlier
    /// entries of the same set, with no precedence overrides.
    pub fn validate(&self, staged: &[Contribution]) -> Result<(), ScopeError> {
        self.validate_planned(staged, &OverridePlan::default())
    }

    /// Validates a staged set against the current registrations (the current
    /// composition) and against earlier entries of the same set, applying the
    /// fixed per-type rules. Returns the first violation.
    ///
    /// A contribution in `plan.removed` is treated as already displaced: its
    /// current holder no longer conflicts with a staged contribution that
    /// occupies the same identity key (decision 28 precedence-driven implicit
    /// replacement). Validation performs no mutation — the displaced entries
    /// are removed on a private scratch clone.
    pub fn validate_planned(
        &self,
        staged: &[Contribution],
        plan: &OverridePlan,
    ) -> Result<(), ScopeError> {
        if plan.is_empty() {
            return self.validate_inner(staged);
        }
        let mut scratch = self.scratch();
        scratch.remove_override_targets(plan);
        scratch.validate_inner(staged)
    }

    fn validate_inner(&self, staged: &[Contribution]) -> Result<(), ScopeError> {
        let mut seen_commands: HashMap<(ScopePath, String), String> = HashMap::new();
        let mut seen_tools: HashMap<(ScopePath, String), String> = HashMap::new();
        let mut seen_services: HashMap<ServiceKey, String> = HashMap::new();
        let mut seen_stages: HashMap<(ScopePath, String, u32), String> = HashMap::new();
        let mut seen_ui: HashMap<(ScopePath, String), String> = HashMap::new();
        let mut seen_guards: HashMap<(ScopePath, String), (String, bool)> = HashMap::new();
        let published: HashMap<ServiceKey, ServiceProvider> = self
            .services
            .lock()
            .expect("services lock poisoned")
            .snapshot()
            .into_iter()
            .map(|(k, p, _)| (k, p))
            .collect();

        for contribution in staged {
            match &contribution.kind {
                ContributionKind::Command(c) => {
                    let key = (contribution.scope.clone(), c.name.clone());
                    let holder = seen_commands
                        .get(&key)
                        .or_else(|| self.commands.get(&key).map(|e| &e.handler));
                    if let Some(holder) = holder {
                        return Err(conflict(
                            "command",
                            contribution,
                            &c.name,
                            holder,
                            &c.handler,
                        ));
                    }
                    seen_commands.insert(key, c.handler.clone());
                }
                ContributionKind::Tool(t) => {
                    if !t.manifest.is_object()
                        || !t
                            .manifest
                            .get("replay_relevant")
                            .is_some_and(Value::is_boolean)
                    {
                        return Err(ScopeError::InvalidContribution {
                            scope: contribution.scope.clone(),
                            reason: "tool manifest must be a JSON object with a boolean `replay_relevant` (R-04)"
                                .into(),
                        });
                    }
                    let key = (contribution.scope.clone(), t.name.clone());
                    let holder = seen_tools
                        .get(&key)
                        .or_else(|| self.tools.get(&key).map(|e| &e.handler));
                    if let Some(holder) = holder {
                        return Err(conflict("tool", contribution, &t.name, holder, &t.handler));
                    }
                    seen_tools.insert(key, t.handler.clone());
                }
                ContributionKind::Service(s) => {
                    let key = s.key.clone();
                    if key.scope != contribution.scope {
                        return Err(ScopeError::InvalidContribution {
                            scope: contribution.scope.clone(),
                            reason: format!(
                                "service key scope `{}` differs from the contribution scope `{}` \
                                 (R-25/C-06: keys are namespaced by the owning scope)",
                                key.scope, contribution.scope
                            ),
                        });
                    }
                    let holder = seen_services
                        .get(&key)
                        .cloned()
                        .or_else(|| published.get(&key).map(provider_identity));
                    if let Some(holder) = holder {
                        return Err(ScopeError::Conflict {
                            kind: "service",
                            scope: contribution.scope.clone(),
                            name: key.name.clone(),
                            holder,
                            challenger: provider_identity(&s.provider),
                        });
                    }
                    seen_services.insert(key, provider_identity(&s.provider));
                }
                ContributionKind::Keymap(_) => {
                    // Layered match: duplicates are layers, never a conflict.
                }
                ContributionKind::Theme(t) => {
                    if !t.overlay.is_object() {
                        return Err(ScopeError::InvalidContribution {
                            scope: contribution.scope.clone(),
                            reason: "theme overlay must be a JSON object (R-19 validated overlay)"
                                .into(),
                        });
                    }
                }
                ContributionKind::ProjectionStage(p) => {
                    let key = (contribution.scope.clone(), p.slot.clone(), p.ordering);
                    let holder = seen_stages
                        .get(&key)
                        .or_else(|| self.stages.get(&key).map(|e| &e.handler));
                    if let Some(holder) = holder {
                        return Err(conflict("stage", contribution, &p.slot, holder, &p.handler));
                    }
                    seen_stages.insert(key, p.handler.clone());
                }
                ContributionKind::UiMount(u) => {
                    validate_ui_slot(&contribution.scope, u.slot.as_deref())?;
                    let key = (contribution.scope.clone(), u.name.clone());
                    let holder = seen_ui
                        .get(&key)
                        .or_else(|| self.ui.get(&key).map(|e| &e.component));
                    if let Some(holder) = holder {
                        return Err(conflict("ui", contribution, &u.name, holder, &u.component));
                    }
                    seen_ui.insert(key, u.component.clone());
                }
                ContributionKind::Guard(g) => {
                    let key = (contribution.scope.clone(), g.name.clone());
                    let existing = seen_guards.get(&key).cloned().or_else(|| {
                        self.guards
                            .get(&key)
                            .map(|e| (e.predicate.clone(), e.monotonic))
                    });
                    if let Some((predicate, monotonic)) = existing
                        && monotonic
                        && !g.monotonic
                    {
                        return Err(ScopeError::Conflict {
                            kind: "guard",
                            scope: contribution.scope.clone(),
                            name: g.name.clone(),
                            holder: predicate,
                            challenger: g.predicate.clone(),
                        });
                    }
                    seen_guards.insert(key, (g.predicate.clone(), g.monotonic));
                }
                ContributionKind::Settings(s) => {
                    // Overlay kind: never a conflict, but key references must
                    // be structurally well-formed.
                    validate_settings(contribution, s)?;
                }
            }
        }
        Ok(())
    }

    /// Atomically applies a validated staged set to `scope` with no
    /// precedence overrides.
    pub fn apply(&mut self, scope: &ScopePath, staged: &[Contribution]) -> Result<(), ScopeError> {
        self.apply_planned(scope, staged, &OverridePlan::default())
    }

    /// Atomically applies a validated staged set to `scope`: every
    /// contribution must carry that scope. All mutations — the single
    /// service-registry state AND the typed maps — happen on clones; on
    /// success both are swapped into `self` and the shared service registry,
    /// so any failure (e.g. a service-dependency cycle detected at publish
    /// time) rejects the whole set with no partial state, and a caller that
    /// never reaches this method (stale epoch) mutates nothing. Callers must
    /// run [`Self::validate_planned`] first; this method re-checks only the
    /// structural invariants it relies on (theme overlays must be objects for
    /// merging).
    ///
    /// `plan.removed`'s service keys are force-displaced from the cloned DAG
    /// before the staged providers publish (precedence-driven implicit
    /// replacement, decision 28); its non-service kinds are removed from the
    /// cloned maps. Dependents of a displaced service keep resolving against
    /// the staged replacement under the same key.
    pub fn apply_planned(
        &mut self,
        scope: &ScopePath,
        staged: &[Contribution],
        plan: &OverridePlan,
    ) -> Result<(), ScopeError> {
        for c in staged {
            if &c.scope != scope {
                return Err(ScopeError::InvalidContribution {
                    scope: c.scope.clone(),
                    reason: format!(
                        "contribution staged for scope `{}` while applying to `{scope}`",
                        c.scope
                    ),
                });
            }
        }
        let mut next = self.clone_state();
        let mut next_services = self
            .services
            .lock()
            .expect("services lock poisoned")
            .clone();
        let removed_services = plan.removed_service_keys();
        for c in &plan.removed {
            match &c.kind {
                ContributionKind::Service(_) => {}
                ContributionKind::Command(cmd) => {
                    next.commands.remove(&(c.scope.clone(), cmd.name.clone()));
                }
                ContributionKind::Tool(t) => {
                    next.tools.remove(&(c.scope.clone(), t.name.clone()));
                }
                ContributionKind::Theme(t) => {
                    next.themes.remove(&(c.scope.clone(), t.name.clone()));
                }
                ContributionKind::ProjectionStage(p) => {
                    next.stages.remove(&(c.scope.clone(), p.slot.clone(), p.ordering));
                }
                ContributionKind::UiMount(u) => {
                    next.ui.remove(&(c.scope.clone(), u.name.clone()));
                }
                ContributionKind::Guard(g) => {
                    next.guards.remove(&(c.scope.clone(), g.name.clone()));
                }
                ContributionKind::Keymap(km) => {
                    next.keymaps.retain(|(s, e)| !(s == &c.scope && e.key == km.key));
                }
                ContributionKind::Settings(_) => {
                    // Intended asymmetry (F11): settings are a merge-only
                    // overlay, so a precedence plan never un-merges them — a
                    // displaced lower layer's fields must survive under the
                    // higher layer. A layer that must be fully DROPPED is
                    // handled by `remove_contributions` (or
                    // `recompose_overlays`), not by `plan.removed`.
                }
            }
        }
        for c in staged {
            match &c.kind {
                ContributionKind::Service(s) => {
                    if removed_services.contains(&s.key) {
                        next_services.remove_forced(&s.key);
                    }
                    next_services
                        .publish_with_deps(s.key.clone(), s.provider.clone(), &s.deps)?;
                }
                ContributionKind::Command(cmd) => {
                    next.commands
                        .insert((c.scope.clone(), cmd.name.clone()), cmd.clone());
                }
                ContributionKind::Tool(t) => {
                    next.tools
                        .insert((c.scope.clone(), t.name.clone()), t.clone());
                }
                ContributionKind::Keymap(km) => {
                    next.keymaps.push((c.scope.clone(), km.clone()));
                }
                ContributionKind::Theme(t) => {
                    if !t.overlay.is_object() {
                        return Err(ScopeError::InvalidContribution {
                            scope: c.scope.clone(),
                            reason: "theme overlay must be a JSON object (R-19 validated overlay)"
                                .into(),
                        });
                    }
                    match next.themes.entry((c.scope.clone(), t.name.clone())) {
                        Entry::Occupied(mut e) => {
                            let merged = e.get_mut().overlay.as_object_mut().expect(
                                "stored theme overlays were validated as objects at apply time",
                            );
                            merged.extend(
                                t.overlay
                                    .as_object()
                                    .expect("checked above")
                                    .iter()
                                    .map(|(k, v)| (k.clone(), v.clone())),
                            );
                        }
                        Entry::Vacant(v) => {
                            v.insert(t.clone());
                        }
                    }
                }
                ContributionKind::ProjectionStage(p) => {
                    next.stages
                        .insert((c.scope.clone(), p.slot.clone(), p.ordering), p.clone());
                }
                ContributionKind::UiMount(u) => {
                    // None means the default slot "main"; normalize so the
                    // registry state (and the composition digest over its
                    // canonical JSON) is canonical — a mount published
                    // without a slot and one published with "main" are the
                    // same contribution.
                    next.ui.insert(
                        (c.scope.clone(), u.name.clone()),
                        UiMountContribution {
                            name: u.name.clone(),
                            component: u.component.clone(),
                            slot: Some(u.slot.clone().unwrap_or_else(|| "main".to_string())),
                        },
                    );
                }
                ContributionKind::Guard(g) => {
                    next.guards
                        .insert((c.scope.clone(), g.name.clone()), g.clone());
                }
                ContributionKind::Settings(s) => {
                    merge_settings(
                        next.settings
                            .entry(c.scope.clone())
                            .or_insert(SettingsContribution {
                                provider: None,
                                approval: None,
                            }),
                        s,
                    );
                }
            }
        }
        // Atomic swap: the cloned maps replace `self`'s, then the staged DAG
        // replaces the shared instance's contents in place (the module host
        // holds the same `Arc`, so its handle stays valid and no caller ever
        // observes a half-applied service set).
        *self = next;
        *self.services.lock().expect("services lock poisoned") = next_services;
        Ok(())
    }

    /// Removes every contribution of `scope` (R-24): its service publications
    /// via the service registry, plus its commands/tools/keymaps/themes/
    /// stages/UI mounts/guards.
    ///
    /// A service of the scope that still has dependents in *other* scopes
    /// fails with `DependentsRemain` unless `force` is set, in which case the
    /// dependent services are cascaded away too (recursively — a dependent
    /// may itself have dependents). Dependents within the same scope are
    /// removed with the scope and never trigger the error. The removal
    /// closure is a DAG (dependency cycles are rejected at publish), so
    /// services are always removed in dependency order.
    pub fn remove_scope(
        &mut self,
        scope: &ScopePath,
        force: bool,
    ) -> Result<RemovedSet, ScopeError> {
        let published: Vec<(ServiceKey, ServiceProvider, Vec<ServiceDependency>)> = self
            .services
            .lock()
            .expect("services lock poisoned")
            .snapshot()
            .into_iter()
            .filter(|(k, _, _)| &k.scope == scope)
            .collect();

        let outside_dependents: Vec<ServiceDependency> = published
            .iter()
            .flat_map(|(key, _, _)| {
                self.services
                    .lock()
                    .expect("services lock poisoned")
                    .dependents_of(key)
            })
            .filter(|d| d.key.scope != *scope)
            .collect();
        if !outside_dependents.is_empty() && !force {
            return Err(ScopeError::DependentsRemain {
                scope: scope.clone(),
                dependents: outside_dependents,
            });
        }

        // The removal closure: the scope's services plus the transitive
        // closure of their cross-scope dependents.
        let mut to_remove: Vec<ServiceKey> = published.iter().map(|(k, _, _)| k.clone()).collect();
        let mut visited: HashSet<ServiceKey> = to_remove.iter().cloned().collect();
        let mut queue = to_remove.clone();
        while let Some(key) = queue.pop() {
            for d in self
                .services
                .lock()
                .expect("services lock poisoned")
                .dependents_of(&key)
            {
                if visited.insert(d.key.clone()) {
                    queue.push(d.key.clone());
                    to_remove.push(d.key.clone());
                }
            }
        }

        let all: HashMap<ServiceKey, (ServiceProvider, Vec<ServiceDependency>)> = self
            .services
            .lock()
            .expect("services lock poisoned")
            .snapshot()
            .into_iter()
            .map(|(k, p, d)| (k, (p, d)))
            .collect();

        let mut removed_keys: HashSet<ServiceKey> = HashSet::new();
        let mut pending = to_remove;
        let mut removed_contributions: Vec<Contribution> = Vec::new();
        let mut cascaded: Vec<ScopePath> = Vec::new();
        while !pending.is_empty() {
            let Some(pos) = pending.iter().position(|k| {
                self.services
                    .lock()
                    .expect("services lock poisoned")
                    .dependents_of(k)
                    .iter()
                    .all(|d| removed_keys.contains(&d.key))
            }) else {
                // Unreachable: the closure is a DAG (cycles rejected at publish).
                return Err(ScopeError::InvalidInput(format!(
                    "internal error removing scope `{scope}`: dependency cycle in the removal closure"
                )));
            };
            let key = pending.remove(pos);
            let key_scope = key.scope.clone();
            let is_cross_scope = key_scope != *scope;
            let (provider, deps) = all
                .get(&key)
                .cloned()
                .expect("the removal closure only contains published services");
            self.services
                .lock()
                .expect("services lock poisoned")
                .remove(&key, provider.module_id)?;
            removed_keys.insert(key.clone());
            removed_contributions.push(Contribution {
                scope: key_scope.clone(),
                kind: ContributionKind::Service(ServiceContribution {
                    key,
                    provider,
                    deps,
                }),
            });
            if is_cross_scope {
                cascaded.push(key_scope);
            }
        }

        let mut extras: Vec<Contribution> = Vec::new();
        self.commands.retain(|(s, _), c| {
            if s == scope {
                extras.push(Contribution {
                    scope: s.clone(),
                    kind: ContributionKind::Command(c.clone()),
                });
                false
            } else {
                true
            }
        });
        self.tools.retain(|(s, _), c| {
            if s == scope {
                extras.push(Contribution {
                    scope: s.clone(),
                    kind: ContributionKind::Tool(c.clone()),
                });
                false
            } else {
                true
            }
        });
        self.themes.retain(|(s, _), c| {
            if s == scope {
                extras.push(Contribution {
                    scope: s.clone(),
                    kind: ContributionKind::Theme(c.clone()),
                });
                false
            } else {
                true
            }
        });
        self.stages.retain(|(s, _, _), c| {
            if s == scope {
                extras.push(Contribution {
                    scope: s.clone(),
                    kind: ContributionKind::ProjectionStage(c.clone()),
                });
                false
            } else {
                true
            }
        });
        self.ui.retain(|(s, _), c| {
            if s == scope {
                extras.push(Contribution {
                    scope: s.clone(),
                    kind: ContributionKind::UiMount(c.clone()),
                });
                false
            } else {
                true
            }
        });
        self.guards.retain(|(s, _), c| {
            if s == scope {
                extras.push(Contribution {
                    scope: s.clone(),
                    kind: ContributionKind::Guard(c.clone()),
                });
                false
            } else {
                true
            }
        });
        self.keymaps.retain(|(s, km)| {
            if s == scope {
                extras.push(Contribution {
                    scope: s.clone(),
                    kind: ContributionKind::Keymap(km.clone()),
                });
                false
            } else {
                true
            }
        });
        self.settings.retain(|s, c| {
            if s == scope {
                extras.push(Contribution {
                    scope: s.clone(),
                    kind: ContributionKind::Settings(c.clone()),
                });
                false
            } else {
                true
            }
        });

        removed_contributions.extend(extras);
        removed_contributions.sort_by(|a, b| snapshot_sort_key(a).cmp(&snapshot_sort_key(b)));
        cascaded.sort_by_key(|a| a.to_string());
        cascaded.dedup();
        Ok(RemovedSet {
            contributions: removed_contributions,
            cascaded_scopes: cascaded,
        })
    }

    /// Removes the given non-service contributions (mid-session deactivation,
    /// M8: a replaced generation's UI mounts/theme overlays leave the
    /// composition). Every removal must match a currently registered entry by
    /// (scope, kind, name); unknown entries are skipped so removal is
    /// idempotent (a failed replacement may already have removed them).
    /// Service contributions are rejected — services live in the shared
    /// service registry and have their own lifecycle. Like [`Self::apply`],
    /// all mutations happen on a clone and swap in only on success.
    pub fn remove_contributions(&mut self, removals: &[Contribution]) -> Result<(), ScopeError> {
        let mut next = self.clone_state();
        for c in removals {
            match &c.kind {
                ContributionKind::Command(cmd) => {
                    next.commands.remove(&(c.scope.clone(), cmd.name.clone()));
                }
                ContributionKind::Tool(t) => {
                    next.tools.remove(&(c.scope.clone(), t.name.clone()));
                }
                ContributionKind::Theme(t) => {
                    next.themes.remove(&(c.scope.clone(), t.name.clone()));
                }
                ContributionKind::ProjectionStage(p) => {
                    next.stages.remove(&(c.scope.clone(), p.slot.clone(), p.ordering));
                }
                ContributionKind::UiMount(u) => {
                    next.ui.remove(&(c.scope.clone(), u.name.clone()));
                }
                ContributionKind::Guard(g) => {
                    next.guards.remove(&(c.scope.clone(), g.name.clone()));
                }
                ContributionKind::Keymap(km) => {
                    next.keymaps.retain(|(s, e)| !(s == &c.scope && e.key == km.key));
                }
                ContributionKind::Settings(_) => {
                    // One merged entry per scope: removing the scope's
                    // settings drops that effective entry.
                    next.settings.remove(&c.scope);
                }
                ContributionKind::Service(_) => {
                    return Err(ScopeError::InvalidContribution {
                        scope: c.scope.clone(),
                        reason:
                            "remove_contributions is for non-service contributions; services \
                             are removed through the shared service registry"
                                .into(),
                    });
                }
            }
        }
        *self = next;
        Ok(())
    }

    /// Full registry state as contributions in deterministic order: sorted by
    /// (scope, kind tag, name, ordering) with a content tiebreak, so equal
    /// states always snapshot identically.
    pub fn snapshot(&self) -> Vec<Contribution> {
        let mut out = Vec::new();
        for ((scope, _), c) in &self.commands {
            out.push(Contribution {
                scope: scope.clone(),
                kind: ContributionKind::Command(c.clone()),
            });
        }
        for ((scope, _), c) in &self.tools {
            out.push(Contribution {
                scope: scope.clone(),
                kind: ContributionKind::Tool(c.clone()),
            });
        }
        for ((scope, _), c) in &self.themes {
            out.push(Contribution {
                scope: scope.clone(),
                kind: ContributionKind::Theme(c.clone()),
            });
        }
        for ((scope, _, _), c) in &self.stages {
            out.push(Contribution {
                scope: scope.clone(),
                kind: ContributionKind::ProjectionStage(c.clone()),
            });
        }
        for ((scope, _), c) in &self.ui {
            out.push(Contribution {
                scope: scope.clone(),
                kind: ContributionKind::UiMount(c.clone()),
            });
        }
        for ((scope, _), c) in &self.guards {
            out.push(Contribution {
                scope: scope.clone(),
                kind: ContributionKind::Guard(c.clone()),
            });
        }
        for (scope, km) in &self.keymaps {
            out.push(Contribution {
                scope: scope.clone(),
                kind: ContributionKind::Keymap(km.clone()),
            });
        }
        for (scope, c) in &self.settings {
            out.push(Contribution {
                scope: scope.clone(),
                kind: ContributionKind::Settings(c.clone()),
            });
        }
        for (key, provider, deps) in self
            .services
            .lock()
            .expect("services lock poisoned")
            .snapshot()
        {
            out.push(Contribution {
                scope: key.scope.clone(),
                kind: ContributionKind::Service(ServiceContribution {
                    key,
                    provider,
                    deps,
                }),
            });
        }
        out.sort_by(|a, b| snapshot_sort_key(a).cmp(&snapshot_sort_key(b)));
        out
    }

    /// Layered keymap match (R-19): the LAST matching layer for
    /// `(scope, key)` — later layers win.
    pub fn keymap_for(&self, scope: &ScopePath, key: &str) -> Option<&KeymapContribution> {
        self.keymaps
            .iter()
            .rev()
            .find_map(|(s, km)| (s == scope && km.key == key).then_some(km))
    }

    /// Merged overlay view for `(scope, name)`: the single entry holding the
    /// result of merging all applied overlays (later wins per top-level key).
    pub fn theme_overlay(&self, scope: &ScopePath, name: &str) -> Option<&ThemeContribution> {
        self.themes.get(&(scope.clone(), name.to_string()))
    }

    /// Merged settings view for `scope`: the single entry holding the result
    /// of overlaying all applied settings contributions (later wins per field).
    pub fn settings_for(&self, scope: &ScopePath) -> Option<&SettingsContribution> {
        self.settings.get(scope)
    }

    /// Replaces a command registration. `previous_holder` must name the
    /// current registration's holder (its handler entry name — contribution
    /// records carry no separate module/generation identity at M2), else
    /// `Conflict` names holder and challenger.
    pub fn replace_command(
        &mut self,
        scope: &ScopePath,
        name: &str,
        new: CommandContribution,
        previous_holder: &str,
    ) -> Result<(), ScopeError> {
        if new.name != name {
            return Err(ScopeError::InvalidInput(format!(
                "replacement for command `{name}` in `{scope}` carries name `{}`",
                new.name
            )));
        }
        let key = (scope.clone(), name.to_string());
        let Some(existing) = self.commands.get(&key) else {
            return Err(ScopeError::InvalidInput(format!(
                "replace of command `{name}` in `{scope}`: no current registration"
            )));
        };
        if existing.handler != previous_holder {
            return Err(ScopeError::Conflict {
                kind: "command",
                scope: scope.clone(),
                name: name.to_string(),
                holder: existing.handler.clone(),
                challenger: new.handler.clone(),
            });
        }
        self.commands.insert(key, new);
        Ok(())
    }

    /// Replaces a tool registration; `previous_holder` is the current
    /// handler entry name.
    pub fn replace_tool(
        &mut self,
        scope: &ScopePath,
        name: &str,
        new: ToolContribution,
        previous_holder: &str,
    ) -> Result<(), ScopeError> {
        if new.name != name {
            return Err(ScopeError::InvalidInput(format!(
                "replacement for tool `{name}` in `{scope}` carries name `{}`",
                new.name
            )));
        }
        let key = (scope.clone(), name.to_string());
        let Some(existing) = self.tools.get(&key) else {
            return Err(ScopeError::InvalidInput(format!(
                "replace of tool `{name}` in `{scope}`: no current registration"
            )));
        };
        if existing.handler != previous_holder {
            return Err(ScopeError::Conflict {
                kind: "tool",
                scope: scope.clone(),
                name: name.to_string(),
                holder: existing.handler.clone(),
                challenger: new.handler.clone(),
            });
        }
        self.tools.insert(key, new);
        Ok(())
    }

    /// Replaces a UI mount; `previous_holder` is the current component name.
    pub fn replace_ui_mount(
        &mut self,
        scope: &ScopePath,
        name: &str,
        new: UiMountContribution,
        previous_holder: &str,
    ) -> Result<(), ScopeError> {
        if new.name != name {
            return Err(ScopeError::InvalidInput(format!(
                "replacement for UI mount `{name}` in `{scope}` carries name `{}`",
                new.name
            )));
        }
        let key = (scope.clone(), name.to_string());
        let Some(existing) = self.ui.get(&key) else {
            return Err(ScopeError::InvalidInput(format!(
                "replace of UI mount `{name}` in `{scope}`: no current registration"
            )));
        };
        if existing.component != previous_holder {
            return Err(ScopeError::Conflict {
                kind: "ui",
                scope: scope.clone(),
                name: name.to_string(),
                holder: existing.component.clone(),
                challenger: new.component.clone(),
            });
        }
        validate_ui_slot(scope, new.slot.as_deref())?;
        self.ui.insert(
            key,
            UiMountContribution {
                name: new.name.clone(),
                component: new.component.clone(),
                slot: Some(new.slot.clone().unwrap_or_else(|| "main".to_string())),
            },
        );
        Ok(())
    }

    /// Replaces a projection stage; `previous_holder` is the current handler
    /// entry name. The slot and ordering identify the stage.
    pub fn replace_stage(
        &mut self,
        scope: &ScopePath,
        slot: &str,
        new: ProjectionStageContribution,
        previous_holder: &str,
    ) -> Result<(), ScopeError> {
        if new.slot != slot {
            return Err(ScopeError::InvalidInput(format!(
                "replacement for stage slot `{slot}` in `{scope}` carries slot `{}`",
                new.slot
            )));
        }
        let key = (scope.clone(), slot.to_string(), new.ordering);
        let Some(existing) = self.stages.get(&key) else {
            return Err(ScopeError::InvalidInput(format!(
                "replace of stage `{slot}` in `{scope}`: no current registration"
            )));
        };
        if existing.handler != previous_holder {
            return Err(ScopeError::Conflict {
                kind: "stage",
                scope: scope.clone(),
                name: slot.to_string(),
                holder: existing.handler.clone(),
                challenger: new.handler.clone(),
            });
        }
        self.stages.insert(key, new);
        Ok(())
    }

    /// The clone `apply_planned` mutates: maps and service-DAG state are
    /// cloned; the swap-in at the end keeps the module host's shared `Arc`
    /// valid.
    fn clone_state(&self) -> ContributionRegistry {
        ContributionRegistry {
            commands: self.commands.clone(),
            tools: self.tools.clone(),
            keymaps: self.keymaps.clone(),
            themes: self.themes.clone(),
            stages: self.stages.clone(),
            ui: self.ui.clone(),
            guards: self.guards.clone(),
            settings: self.settings.clone(),
            services: Arc::clone(&self.services),
        }
    }

    /// A fully independent clone (own `Arc<Mutex<ServiceRegistry>>` with a
    /// cloned DAG) used by `validate_planned` to remove override targets
    /// without touching the live registry.
    fn scratch(&self) -> ContributionRegistry {
        let mut scratch = self.clone_state();
        scratch.services = Arc::new(Mutex::new(
            self.services
                .lock()
                .expect("services lock poisoned")
                .clone(),
        ));
        scratch
    }

    /// Removes the precedence plan's displaced entries from a scratch clone:
    /// services are force-displaced (the staged replacement will re-occupy the
    /// key), non-service kinds are dropped from their maps.
    fn remove_override_targets(&mut self, plan: &OverridePlan) {
        for c in &plan.removed {
            match &c.kind {
                ContributionKind::Service(s) => {
                    self.services
                        .lock()
                        .expect("services lock poisoned")
                        .remove_forced(&s.key);
                }
                ContributionKind::Command(cmd) => {
                    self.commands.remove(&(c.scope.clone(), cmd.name.clone()));
                }
                ContributionKind::Tool(t) => {
                    self.tools.remove(&(c.scope.clone(), t.name.clone()));
                }
                ContributionKind::Theme(t) => {
                    self.themes.remove(&(c.scope.clone(), t.name.clone()));
                }
                ContributionKind::ProjectionStage(p) => {
                    self.stages
                        .remove(&(c.scope.clone(), p.slot.clone(), p.ordering));
                }
                ContributionKind::UiMount(u) => {
                    self.ui.remove(&(c.scope.clone(), u.name.clone()));
                }
                ContributionKind::Guard(g) => {
                    self.guards.remove(&(c.scope.clone(), g.name.clone()));
                }
                ContributionKind::Keymap(km) => {
                    self.keymaps
                        .retain(|(s, e)| !(s == &c.scope && e.key == km.key));
                }
                ContributionKind::Settings(_) => {
                    // Same intended asymmetry as `apply_planned`: a plan never
                    // displaces a merge-only settings overlay.
                }
            }
        }
    }

    /// Rebuilds `scope`'s overlay kinds (`settings`, `themes`) from exactly
    /// `contributions`, in order. Overlay kinds merge rather than occupy a
    /// slot, so a layer cannot be un-merged by name; the safe-mode rollback
    /// (R-01/C-02) replays the surviving layers' overlays instead. Non-overlay
    /// contributions are ignored (their removal is
    /// [`Self::remove_contributions`]'s job).
    pub fn recompose_overlays(&mut self, scope: &ScopePath, contributions: &[Contribution]) {
        let mut next = self.clone_state();
        next.settings.remove(scope);
        next.themes.retain(|(s, _), _| s != scope);
        for c in contributions.iter().filter(|c| &c.scope == scope) {
            match &c.kind {
                ContributionKind::Settings(s) => {
                    merge_settings(
                        next.settings
                            .entry(c.scope.clone())
                            .or_insert(SettingsContribution {
                                provider: None,
                                approval: None,
                            }),
                        s,
                    );
                }
                ContributionKind::Theme(t) if t.overlay.is_object() => {
                    match next.themes.entry((c.scope.clone(), t.name.clone())) {
                        Entry::Occupied(mut e) => {
                            let merged = e
                                .get_mut()
                                .overlay
                                .as_object_mut()
                                .expect("stored theme overlays are objects");
                            merged.extend(
                                t.overlay
                                    .as_object()
                                    .expect("checked above")
                                    .iter()
                                    .map(|(k, v)| (k.clone(), v.clone())),
                            );
                        }
                        Entry::Vacant(v) => {
                            v.insert(t.clone());
                        }
                    }
                }
                _ => {}
            }
        }
        *self = next;
    }
}

fn conflict(
    kind: &'static str,
    contribution: &Contribution,
    name: &str,
    holder: &str,
    challenger: &str,
) -> ScopeError {
    ScopeError::Conflict {
        kind,
        scope: contribution.scope.clone(),
        name: name.to_string(),
        holder: holder.to_string(),
        challenger: challenger.to_string(),
    }
}

/// UI slot charset (M8): alphanumeric + `-` + `_`, max 32 chars, non-empty.
/// `None` is the default slot (`"main"`) and always valid; validation keeps
/// the composition digest canonical (two mounts can never express the same
/// slot differently).
fn validate_ui_slot(scope: &ScopePath, slot: Option<&str>) -> Result<(), ScopeError> {
    let Some(slot) = slot else {
        return Ok(());
    };
    let valid = !slot.is_empty()
        && slot.len() <= 32
        && slot
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !valid {
        return Err(ScopeError::InvalidContribution {
            scope: scope.clone(),
            reason: format!(
                "ui mount slot `{slot}` violates the kernel charset \
                 (alphanumeric + '-' + '_', max 32 chars)"
            ),
        });
    }
    Ok(())
}

/// Settings are an overlay kind (never a conflict), but key references must be
/// structurally well-formed: an empty env name or keychain coordinate cannot
/// resolve a secret, so it is rejected as an invalid contribution.
fn validate_settings(
    contribution: &Contribution,
    settings: &SettingsContribution,
) -> Result<(), ScopeError> {
    let Some(provider) = &settings.provider else {
        return Ok(());
    };
    let Some(key) = &provider.key else {
        return Ok(());
    };
    let valid = match key {
        KeyReference::Env { name } => !name.is_empty(),
        KeyReference::Keychain { service, account } => !service.is_empty() && !account.is_empty(),
    };
    if valid {
        return Ok(());
    }
    Err(ScopeError::InvalidContribution {
        scope: contribution.scope.clone(),
        reason: format!("settings key reference `{key:?}` has an empty field"),
    })
}

/// Field-wise overlay (R-19): an incoming `Some` overwrites, `None` leaves the
/// existing value; nested `provider`/`approval` merge field-wise too.
fn merge_settings(base: &mut SettingsContribution, incoming: &SettingsContribution) {
    merge_provider(&mut base.provider, &incoming.provider);
    merge_approval(&mut base.approval, &incoming.approval);
}

fn merge_provider(base: &mut Option<ProviderSettings>, incoming: &Option<ProviderSettings>) {
    match (base, incoming) {
        (Some(base), Some(incoming)) => {
            if incoming.base_url.is_some() {
                base.base_url = incoming.base_url.clone();
            }
            if incoming.model.is_some() {
                base.model = incoming.model.clone();
            }
            if incoming.protocol.is_some() {
                base.protocol = incoming.protocol.clone();
            }
            if incoming.key.is_some() {
                base.key = incoming.key.clone();
            }
            if incoming.fake.is_some() {
                base.fake = incoming.fake;
            }
        }
        (slot, incoming) => {
            if incoming.is_some() {
                *slot = incoming.clone();
            }
        }
    }
}

fn merge_approval(base: &mut Option<ApprovalSettings>, incoming: &Option<ApprovalSettings>) {
    match (base, incoming) {
        (Some(base), Some(incoming)) => {
            if incoming.auto_approve.is_some() {
                base.auto_approve = incoming.auto_approve;
            }
            if incoming.yolo.is_some() {
                base.yolo = incoming.yolo;
            }
        }
        (slot, incoming) => {
            if incoming.is_some() {
                *slot = incoming.clone();
            }
        }
    }
}

/// Deterministic holder identity for service conflicts: `module@generation`.
fn provider_identity(provider: &ServiceProvider) -> String {
    format!("{}@{}", provider.module_id, provider.generation)
}

/// Total deterministic order for snapshots: (scope, kind tag, name, ordering,
/// canonical kind JSON as final tiebreak).
fn snapshot_sort_key(c: &Contribution) -> (String, &'static str, String, String, String) {
    let kind = &c.kind;
    let (name, ordering) = match kind {
        ContributionKind::Command(c) => (c.name.clone(), String::new()),
        ContributionKind::Tool(t) => (t.name.clone(), String::new()),
        ContributionKind::Service(s) => (s.key.name.clone(), String::new()),
        ContributionKind::Keymap(k) => (k.key.clone(), String::new()),
        ContributionKind::Theme(t) => (t.name.clone(), String::new()),
        ContributionKind::ProjectionStage(p) => (p.slot.clone(), format!("{:010}", p.ordering)),
        ContributionKind::UiMount(u) => (u.name.clone(), String::new()),
        ContributionKind::Guard(g) => (g.name.clone(), String::new()),
        ContributionKind::Settings(_) => (String::new(), String::new()),
    };
    (
        c.scope.to_string(),
        kind.kind_tag(),
        name,
        ordering,
        serde_json::to_string(kind).expect("contribution kinds are always serializable"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanbei_core::Id128;
    use kanbei_services::ServiceContract;
    use serde_json::json;

    fn scope(name: &str) -> ScopePath {
        ScopePath(vec![name.to_string()])
    }

    fn provider(name: &str, version: u32) -> ServiceProvider {
        ServiceProvider {
            module_id: Id128::generate(),
            generation: 1,
            contract: ServiceContract {
                name: name.to_string(),
                version,
            },
        }
    }

    fn full_set(s: &ScopePath) -> Vec<Contribution> {
        vec![
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::Command(CommandContribution {
                    name: "cmd".into(),
                    handler: "cmd_h".into(),
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::Tool(ToolContribution {
                    name: "tool".into(),
                    manifest: json!({"replay_relevant": true, "kind": "shell"}),
                    handler: "tool_h".into(),
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::Service(ServiceContribution {
                    key: ServiceKey {
                        scope: s.clone(),
                        name: "svc".into(),
                    },
                    provider: provider("svc", 1),
                    deps: vec![],
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::Keymap(KeymapContribution {
                    key: "k".into(),
                    action: "a".into(),
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::Theme(ThemeContribution {
                    name: "t".into(),
                    overlay: json!({"colors": {"bg": "#000"}}),
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::ProjectionStage(ProjectionStageContribution {
                    slot: "main".into(),
                    ordering: 10,
                    handler: "stage_h".into(),
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::UiMount(UiMountContribution {
                    name: "header".into(),
                    component: "Header".into(),
                    slot: None,
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::Guard(GuardContribution {
                    name: "g".into(),
                    predicate: "pred".into(),
                    monotonic: false,
                }),
            },
        ]
    }

    fn validate_and_apply(
        registry: &mut ContributionRegistry,
        scope: &ScopePath,
        set: &[Contribution],
    ) {
        registry.validate(set).unwrap();
        registry.apply(scope, set).unwrap();
    }

    #[test]
    fn duplicate_command_in_scope_conflicts() {
        let s = scope("app");
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let c1 = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Command(CommandContribution {
                name: "run".into(),
                handler: "h1".into(),
            }),
        };
        let c2 = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Command(CommandContribution {
                name: "run".into(),
                handler: "h2".into(),
            }),
        };
        // staged vs staged
        let err = registry.validate(&[c1.clone(), c2.clone()]).unwrap_err();
        assert_eq!(
            err,
            ScopeError::Conflict {
                kind: "command",
                scope: s.clone(),
                name: "run".into(),
                holder: "h1".into(),
                challenger: "h2".into(),
            }
        );
        // staged vs registry (moves: c1 and c2 are not used again)
        validate_and_apply(&mut registry, &s, &[c1]);
        let err = registry.validate(&[c2]).unwrap_err();
        assert_eq!(
            err,
            ScopeError::Conflict {
                kind: "command",
                scope: s.clone(),
                name: "run".into(),
                holder: "h1".into(),
                challenger: "h2".into(),
            }
        );
        // the same name in another scope is fine
        let other = scope("other");
        let c3 = Contribution {
            scope: other.clone(),
            kind: ContributionKind::Command(CommandContribution {
                name: "run".into(),
                handler: "h3".into(),
            }),
        };
        registry.validate(&[c3]).unwrap();
    }

    #[test]
    fn duplicate_tool_in_scope_conflicts() {
        let s = scope("app");
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let t1 = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Tool(ToolContribution {
                name: "sh".into(),
                manifest: json!({"replay_relevant": true}),
                handler: "h1".into(),
            }),
        };
        let t2 = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Tool(ToolContribution {
                name: "sh".into(),
                manifest: json!({"replay_relevant": false}),
                handler: "h2".into(),
            }),
        };
        let err = registry.validate(&[t1.clone(), t2.clone()]).unwrap_err();
        assert_eq!(
            err,
            ScopeError::Conflict {
                kind: "tool",
                scope: s.clone(),
                name: "sh".into(),
                holder: "h1".into(),
                challenger: "h2".into(),
            }
        );
        validate_and_apply(&mut registry, &s, &[t1]);
        let err = registry.validate(&[t2]).unwrap_err();
        assert!(matches!(err, ScopeError::Conflict { kind: "tool", .. }));
    }

    #[test]
    fn invalid_tool_manifest_rejected() {
        let s = scope("app");
        let registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let bad = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Tool(ToolContribution {
                name: "t".into(),
                manifest: json!({"kind": "shell"}),
                handler: "h".into(),
            }),
        };
        let err = registry.validate(&[bad]).unwrap_err();
        assert!(matches!(
            err,
            ScopeError::InvalidContribution { ref scope, reason }
                if scope == &s && reason.contains("replay_relevant")
        ));
    }

    #[test]
    fn duplicate_service_key_conflicts() {
        let s = scope("app");
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let k = ServiceKey {
            scope: s.clone(),
            name: "db".into(),
        };
        let svc1 = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Service(ServiceContribution {
                key: k.clone(),
                provider: provider("db", 1),
                deps: vec![],
            }),
        };
        let svc2 = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Service(ServiceContribution {
                key: k.clone(),
                provider: provider("db", 2),
                deps: vec![],
            }),
        };
        // staged vs staged
        let err = registry
            .validate(&[svc1.clone(), svc2.clone()])
            .unwrap_err();
        assert!(matches!(
            err,
            ScopeError::Conflict {
                kind: "service",
                ref scope,
                ref name,
                ..
            } if scope == &s && name == "db"
        ));
        // staged vs registry
        validate_and_apply(&mut registry, &s, &[svc1]);
        let err = registry.validate(&[svc2]).unwrap_err();
        assert!(matches!(
            err,
            ScopeError::Conflict {
                kind: "service",
                ..
            }
        ));
    }

    #[test]
    fn duplicate_projection_slot_ordering_conflicts() {
        let s = scope("app");
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let p1 = Contribution {
            scope: s.clone(),
            kind: ContributionKind::ProjectionStage(ProjectionStageContribution {
                slot: "main".into(),
                ordering: 10,
                handler: "h1".into(),
            }),
        };
        let p2 = Contribution {
            scope: s.clone(),
            kind: ContributionKind::ProjectionStage(ProjectionStageContribution {
                slot: "main".into(),
                ordering: 10,
                handler: "h2".into(),
            }),
        };
        let err = registry.validate(&[p1.clone(), p2.clone()]).unwrap_err();
        assert_eq!(
            err,
            ScopeError::Conflict {
                kind: "stage",
                scope: s.clone(),
                name: "main".into(),
                holder: "h1".into(),
                challenger: "h2".into(),
            }
        );
        // same slot, distinct ordering is fine
        let p3 = Contribution {
            scope: s.clone(),
            kind: ContributionKind::ProjectionStage(ProjectionStageContribution {
                slot: "main".into(),
                ordering: 20,
                handler: "h3".into(),
            }),
        };
        registry.validate(&[p1.clone(), p3]).unwrap();
        validate_and_apply(&mut registry, &s, &[p1]);
        let err = registry.validate(&[p2]).unwrap_err();
        assert!(matches!(err, ScopeError::Conflict { kind: "stage", .. }));
    }

    #[test]
    fn duplicate_keymaps_layer() {
        let s = scope("app");
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let k1 = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Keymap(KeymapContribution {
                key: "k".into(),
                action: "a1".into(),
            }),
        };
        let k2 = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Keymap(KeymapContribution {
                key: "k".into(),
                action: "a2".into(),
            }),
        };
        // no conflict: both layers are stored
        registry.validate(&[k1.clone(), k2.clone()]).unwrap();
        registry.apply(&s, &[k1, k2]).unwrap();
        let keymaps: Vec<_> = registry
            .snapshot()
            .into_iter()
            .filter(|c| matches!(c.kind, ContributionKind::Keymap(_)))
            .collect();
        assert_eq!(keymaps.len(), 2);
        // lookup returns the LAST matching layer
        assert_eq!(registry.keymap_for(&s, "k").unwrap().action, "a2");
        assert!(registry.keymap_for(&s, "missing").is_none());
    }

    #[test]
    fn duplicate_themes_merge_overlays() {
        let s = scope("app");
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let t1 = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Theme(ThemeContribution {
                name: "t".into(),
                overlay: json!({"colors": {"bg": "#000"}}),
            }),
        };
        let t2 = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Theme(ThemeContribution {
                name: "t".into(),
                overlay: json!({"fonts": {"size": 14}}),
            }),
        };
        registry.validate(&[t1.clone(), t2.clone()]).unwrap();
        registry.apply(&s, &[t1, t2]).unwrap();
        // merged view: both layers' top-level keys present; one stored entry
        assert_eq!(
            registry.theme_overlay(&s, "t").unwrap().overlay,
            json!({"colors": {"bg": "#000"}, "fonts": {"size": 14}})
        );
        let themes: Vec<_> = registry
            .snapshot()
            .into_iter()
            .filter(|c| matches!(c.kind, ContributionKind::Theme(_)))
            .collect();
        assert_eq!(themes.len(), 1);
        // shallow merge: a later overlay's top-level key replaces the earlier one
        let t3 = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Theme(ThemeContribution {
                name: "t".into(),
                overlay: json!({"colors": {"fg": "#fff"}}),
            }),
        };
        registry.validate(std::slice::from_ref(&t3)).unwrap();
        registry.apply(&s, &[t3]).unwrap();
        assert_eq!(
            registry.theme_overlay(&s, "t").unwrap().overlay,
            json!({"colors": {"fg": "#fff"}, "fonts": {"size": 14}})
        );
    }

    #[test]
    fn theme_overlay_must_be_a_json_object() {
        let s = scope("app");
        let registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let bad = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Theme(ThemeContribution {
                name: "t".into(),
                overlay: json!([1, 2, 3]),
            }),
        };
        let err = registry.validate(&[bad]).unwrap_err();
        assert!(matches!(
            err,
            ScopeError::InvalidContribution { ref scope, reason }
                if scope == &s && reason.contains("JSON object")
        ));
    }

    #[test]
    fn monotonic_guard_cannot_be_weakened() {
        let s = scope("app");
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let strong = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Guard(GuardContribution {
                name: "g".into(),
                predicate: "p1".into(),
                monotonic: true,
            }),
        };
        validate_and_apply(&mut registry, &s, &[strong]);
        // replacing a monotonic guard with a non-monotonic one fails
        let weak = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Guard(GuardContribution {
                name: "g".into(),
                predicate: "p2".into(),
                monotonic: false,
            }),
        };
        let err = registry.validate(&[weak]).unwrap_err();
        assert_eq!(
            err,
            ScopeError::Conflict {
                kind: "guard",
                scope: s.clone(),
                name: "g".into(),
                holder: "p1".into(),
                challenger: "p2".into(),
            }
        );
        // an equal-strength re-registration is fine and replaces the entry
        let strong2 = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Guard(GuardContribution {
                name: "g".into(),
                predicate: "p1v2".into(),
                monotonic: true,
            }),
        };
        validate_and_apply(&mut registry, &s, &[strong2]);
        let guards: Vec<_> = registry
            .snapshot()
            .into_iter()
            .filter(|c| matches!(c.kind, ContributionKind::Guard(_)))
            .collect();
        assert_eq!(guards.len(), 1);
        assert!(matches!(
            &guards[0].kind,
            ContributionKind::Guard(GuardContribution { predicate, .. }) if predicate == "p1v2"
        ));
    }

    #[test]
    fn apply_is_transactional_on_service_failure() {
        let s = scope("app");
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let mut store = crate::epoch::CompositionStore::new(&registry);
        let key = ServiceKey {
            scope: s.clone(),
            name: "cyclic".into(),
        };
        let cmd = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Command(CommandContribution {
                name: "tx-cmd".into(),
                handler: "h".into(),
            }),
        };
        // the self-dependency passes validate (key is free) but fails at
        // apply time (cycle detected by the service registry's publish)
        let svc = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Service(ServiceContribution {
                key: key.clone(),
                provider: provider("cyclic", 1),
                deps: vec![ServiceDependency {
                    key: key.clone(),
                    required_version: 1,
                }],
            }),
        };
        let err = store
            .stage_publish(&[cmd.clone(), svc.clone()], &mut registry)
            .unwrap_err();
        assert!(matches!(
            err,
            ScopeError::Service(kanbei_services::ServiceError::DependencyCycle { .. })
        ));
        // nothing from the rejected set is applied, and the epoch did not bump
        assert!(
            registry
                .snapshot()
                .iter()
                .all(|c| !matches!(&c.kind, ContributionKind::Command(cc) if cc.name == "tx-cmd"))
        );
        assert!(
            registry
                .snapshot()
                .iter()
                .all(|c| !matches!(&c.kind, ContributionKind::Service(sv) if sv.key == key))
        );
        assert_eq!(store.current().epoch, 0);
    }

    #[test]
    fn snapshot_is_deterministic() {
        let s = scope("app");
        let set = full_set(&s);
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        validate_and_apply(&mut registry, &s, &set);
        let snap1 = registry.snapshot();
        assert_eq!(snap1, registry.snapshot());
        // the same state built through a different input order snapshots
        // identically (same set, same providers, reversed apply order)
        let reversed: Vec<Contribution> = set.into_iter().rev().collect();
        let mut registry2 = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        validate_and_apply(&mut registry2, &s, &reversed);
        assert_eq!(snap1, registry2.snapshot());
    }

    #[test]
    fn replace_command_requires_previous_holder() {
        let s = scope("app");
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let c1 = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Command(CommandContribution {
                name: "run".into(),
                handler: "h1".into(),
            }),
        };
        validate_and_apply(&mut registry, &s, &[c1]);
        // correct previous holder replaces
        registry
            .replace_command(
                &s,
                "run",
                CommandContribution {
                    name: "run".into(),
                    handler: "h2".into(),
                },
                "h1",
            )
            .unwrap();
        // wrong previous holder is a conflict naming holder and challenger
        let err = registry
            .replace_command(
                &s,
                "run",
                CommandContribution {
                    name: "run".into(),
                    handler: "h3".into(),
                },
                "wrong",
            )
            .unwrap_err();
        assert_eq!(
            err,
            ScopeError::Conflict {
                kind: "command",
                scope: s.clone(),
                name: "run".into(),
                holder: "h2".into(),
                challenger: "h3".into(),
            }
        );
        // unknown name and mismatched name are invalid input
        let err = registry
            .replace_command(
                &s,
                "ghost",
                CommandContribution {
                    name: "ghost".into(),
                    handler: "h".into(),
                },
                "h2",
            )
            .unwrap_err();
        assert!(matches!(err, ScopeError::InvalidInput(_)));
        let err = registry
            .replace_command(
                &s,
                "run",
                CommandContribution {
                    name: "other".into(),
                    handler: "h".into(),
                },
                "h2",
            )
            .unwrap_err();
        assert!(matches!(err, ScopeError::InvalidInput(_)));
    }

    #[test]
    fn replace_tool_ui_and_stage() {
        let s = scope("app");
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let set = vec![
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::Tool(ToolContribution {
                    name: "t".into(),
                    manifest: json!({"replay_relevant": true}),
                    handler: "h1".into(),
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::UiMount(UiMountContribution {
                    name: "u".into(),
                    component: "Old".into(),
                    slot: None,
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::ProjectionStage(ProjectionStageContribution {
                    slot: "st".into(),
                    ordering: 10,
                    handler: "h1".into(),
                }),
            },
        ];
        validate_and_apply(&mut registry, &s, &set);
        registry
            .replace_tool(
                &s,
                "t",
                ToolContribution {
                    name: "t".into(),
                    manifest: json!({"replay_relevant": false}),
                    handler: "h2".into(),
                },
                "h1",
            )
            .unwrap();
        registry
            .replace_ui_mount(
                &s,
                "u",
                UiMountContribution {
                    name: "u".into(),
                    component: "New".into(),
                    slot: None,
                },
                "Old",
            )
            .unwrap();
        registry
            .replace_stage(
                &s,
                "st",
                ProjectionStageContribution {
                    slot: "st".into(),
                    ordering: 10,
                    handler: "h2".into(),
                },
                "h1",
            )
            .unwrap();
        // wrong holders are conflicts
        let err = registry
            .replace_tool(
                &s,
                "t",
                ToolContribution {
                    name: "t".into(),
                    manifest: json!({"replay_relevant": true}),
                    handler: "h3".into(),
                },
                "wrong",
            )
            .unwrap_err();
        assert!(matches!(err, ScopeError::Conflict { kind: "tool", .. }));
        let err = registry
            .replace_ui_mount(
                &s,
                "u",
                UiMountContribution {
                    name: "u".into(),
                    component: "X".into(),
                    slot: None,
                },
                "wrong",
            )
            .unwrap_err();
        assert!(matches!(err, ScopeError::Conflict { kind: "ui", .. }));
        let err = registry
            .replace_stage(
                &s,
                "st",
                ProjectionStageContribution {
                    slot: "st".into(),
                    ordering: 10,
                    handler: "h3".into(),
                },
                "wrong",
            )
            .unwrap_err();
        assert!(matches!(err, ScopeError::Conflict { kind: "stage", .. }));
        // the registry holds the replacements
        let snap = registry.snapshot();
        assert!(snap.iter().any(|c| matches!(
            &c.kind,
            ContributionKind::Tool(t) if t.handler == "h2"
        )));
        assert!(snap.iter().any(|c| matches!(
            &c.kind,
            ContributionKind::UiMount(u) if u.component == "New"
        )));
        assert!(snap.iter().any(|c| matches!(
            &c.kind,
            ContributionKind::ProjectionStage(p) if p.handler == "h2"
        )));
    }

    #[test]
    fn remove_scope_with_cross_scope_dependents_requires_force() {
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let root = ScopePath(vec![]);
        let child = scope("agent");
        let root_key = ServiceKey {
            scope: root.clone(),
            name: "db".into(),
        };
        let child_key = ServiceKey {
            scope: child.clone(),
            name: "repo".into(),
        };
        let dep = ServiceDependency {
            key: root_key.clone(),
            required_version: 1,
        };
        let root_svc = Contribution {
            scope: root.clone(),
            kind: ContributionKind::Service(ServiceContribution {
                key: root_key.clone(),
                provider: provider("db", 1),
                deps: vec![],
            }),
        };
        let child_svc = Contribution {
            scope: child.clone(),
            kind: ContributionKind::Service(ServiceContribution {
                key: child_key.clone(),
                provider: provider("repo", 1),
                deps: vec![dep.clone()],
            }),
        };
        validate_and_apply(&mut registry, &root, &[root_svc]);
        validate_and_apply(&mut registry, &child, &[child_svc]);

        // force=false: the root service still has a dependent in another
        // scope; `dependents` names the dependent service (its key + version)
        let err = registry.remove_scope(&root, false).unwrap_err();
        assert_eq!(
            err,
            ScopeError::DependentsRemain {
                scope: root.clone(),
                dependents: vec![ServiceDependency {
                    key: child_key.clone(),
                    required_version: 1,
                }],
            }
        );

        // force=true: the dependent service is cascaded away
        let removed = registry.remove_scope(&root, true).unwrap();
        assert_eq!(removed.cascaded_scopes, vec![child.clone()]);
        assert!(removed.contributions.iter().any(|c| matches!(
            &c.kind,
            ContributionKind::Service(s) if s.key == root_key
        )));
        assert!(removed.contributions.iter().any(|c| matches!(
            &c.kind,
            ContributionKind::Service(s) if s.key == child_key
        )));
        assert!(registry.snapshot().is_empty());
    }

    #[test]
    fn remove_scope_removes_all_contribution_kinds() {
        let s = scope("app");
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        validate_and_apply(&mut registry, &s, &full_set(&s));
        let removed = registry.remove_scope(&s, false).unwrap();
        assert_eq!(removed.contributions.len(), 8);
        assert!(removed.cascaded_scopes.is_empty());
        assert!(registry.snapshot().is_empty());
    }

    #[test]
    fn ui_slot_defaults_and_charset() {
        let s = scope("app");
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        // None is the default slot; the registry normalizes it to "main" so
        // the composition digest is canonical.
        let set = vec![Contribution {
            scope: s.clone(),
            kind: ContributionKind::UiMount(UiMountContribution {
                name: "a".into(),
                component: "A".into(),
                slot: None,
            }),
        }];
        validate_and_apply(&mut registry, &s, &set);
        let snap = registry.snapshot();
        let ui = snap
            .iter()
            .find_map(|c| match &c.kind {
                ContributionKind::UiMount(u) if u.name == "a" => Some(u),
                _ => None,
            })
            .expect("mount registered");
        assert_eq!(ui.slot.as_deref(), Some("main"));
        // an explicit "main" mount is the same contribution (no conflict)
        validate_and_apply(
            &mut registry,
            &s,
            &[Contribution {
                scope: s.clone(),
                kind: ContributionKind::UiMount(UiMountContribution {
                    name: "b".into(),
                    component: "B".into(),
                    slot: Some("main".into()),
                }),
            }],
        );
        let snap = registry.snapshot();
        let ui_b = snap
            .iter()
            .find_map(|c| match &c.kind {
                ContributionKind::UiMount(u) if u.name == "b" => Some(u),
                _ => None,
            })
            .expect("mount registered");
        assert_eq!(ui_b.slot.as_deref(), Some("main"));

        // canonical slots pass; the charset rejects anything else
        for ok in ["status", "header", "composer", "aux", "my_slot-2"] {
            let err = registry.validate(&[Contribution {
                scope: s.clone(),
                kind: ContributionKind::UiMount(UiMountContribution {
                    name: format!("ok-{ok}"),
                    component: "C".into(),
                    slot: Some(ok.into()),
                }),
            }]);
            assert!(err.is_ok(), "slot {ok:?} must validate");
        }
        for bad in ["", "sp ace", "semi;colon", "dots.in", "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"] {
            let err = registry
                .validate(&[Contribution {
                    scope: s.clone(),
                    kind: ContributionKind::UiMount(UiMountContribution {
                        name: format!("bad-{bad}"),
                        component: "C".into(),
                        slot: Some(bad.to_string()),
                    }),
                }])
                .unwrap_err();
            assert!(
                matches!(err, ScopeError::InvalidContribution { .. }),
                "slot {bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn remove_contributions_drops_matching_entries_idempotently() {
        let s = scope("app");
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        validate_and_apply(&mut registry, &s, &full_set(&s));
        assert_eq!(registry.snapshot().len(), 8);

        let ui_mount = Contribution {
            scope: s.clone(),
            kind: ContributionKind::UiMount(UiMountContribution {
                name: "header".into(),
                component: "Header".into(),
                slot: None,
            }),
        };
        let theme = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Theme(ThemeContribution {
                name: "t".into(),
                overlay: json!({"colors": {"bg": "#000"}}),
            }),
        };
        registry.remove_contributions(&[ui_mount.clone()]).unwrap();
        assert_eq!(registry.snapshot().len(), 7);
        assert!(registry.snapshot().iter().all(|c| !matches!(
            &c.kind,
            ContributionKind::UiMount(u) if u.name == "header"
        )));
        // idempotent: removing the same (already gone) entry is a no-op
        registry.remove_contributions(&[ui_mount]).unwrap();
        assert_eq!(registry.snapshot().len(), 7);

        registry.remove_contributions(&[theme]).unwrap();
        assert_eq!(registry.snapshot().len(), 6);

        // service removals are rejected (they live in the shared registry)
        let svc = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Service(ServiceContribution {
                key: ServiceKey {
                    scope: s.clone(),
                    name: "svc".into(),
                },
                provider: provider("svc", 1),
                deps: vec![],
            }),
        };
        let err = registry.remove_contributions(&[svc]).unwrap_err();
        assert!(matches!(err, ScopeError::InvalidContribution { .. }));
        assert_eq!(registry.snapshot().len(), 6, "failed removal mutates nothing");
    }

    fn settings(scope: &ScopePath, s: SettingsContribution) -> Contribution {
        Contribution {
            scope: scope.clone(),
            kind: ContributionKind::Settings(s),
        }
    }

    fn provider_with_key(key: KeyReference) -> ProviderSettings {
        ProviderSettings {
            base_url: None,
            model: None,
            protocol: None,
            key: Some(key),
            fake: None,
        }
    }

    #[test]
    fn duplicate_settings_merge_field_wise_later_wins() {
        let s = scope("app");
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let first = settings(
            &s,
            SettingsContribution {
                provider: Some(ProviderSettings {
                    base_url: Some("https://a.example".into()),
                    model: Some("m1".into()),
                    protocol: None,
                    key: None,
                    fake: None,
                }),
                approval: Some(ApprovalSettings {
                    auto_approve: Some(true),
                    yolo: None,
                }),
            },
        );
        let second = settings(
            &s,
            SettingsContribution {
                provider: Some(ProviderSettings {
                    base_url: Some("https://b.example".into()),
                    model: None,
                    protocol: Some("openai".into()),
                    key: Some(KeyReference::Env { name: "KEY".into() }),
                    fake: Some(true),
                }),
                approval: Some(ApprovalSettings {
                    auto_approve: None,
                    yolo: Some(false),
                }),
            },
        );
        // overlay kind: two Settings contributions never conflict
        registry.validate(&[first.clone(), second.clone()]).unwrap();
        registry.apply(&s, &[first, second]).unwrap();

        let merged = registry.settings_for(&s).expect("settings registered");
        let p = merged.provider.as_ref().expect("provider present");
        assert_eq!(
            p.base_url.as_deref(),
            Some("https://b.example"),
            "later wins"
        );
        assert_eq!(
            p.model.as_deref(),
            Some("m1"),
            "incoming None keeps existing"
        );
        assert_eq!(p.protocol.as_deref(), Some("openai"));
        assert_eq!(p.key, Some(KeyReference::Env { name: "KEY".into() }));
        assert_eq!(p.fake, Some(true));
        let a = merged.approval.as_ref().expect("approval present");
        assert_eq!(a.auto_approve, Some(true), "incoming None keeps existing");
        assert_eq!(a.yolo, Some(false));

        let count = registry
            .snapshot()
            .iter()
            .filter(|c| matches!(c.kind, ContributionKind::Settings(_)))
            .count();
        assert_eq!(count, 1, "one effective settings entry per scope");
    }

    #[test]
    fn settings_partial_overlay_does_not_clobber_earlier_fields() {
        let s = scope("app");
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        validate_and_apply(
            &mut registry,
            &s,
            &[settings(
                &s,
                SettingsContribution {
                    provider: Some(ProviderSettings {
                        base_url: Some("https://a.example".into()),
                        model: None,
                        protocol: None,
                        key: None,
                        fake: None,
                    }),
                    approval: None,
                },
            )],
        );
        // a layer that only sets provider.model must not clobber base_url
        validate_and_apply(
            &mut registry,
            &s,
            &[settings(
                &s,
                SettingsContribution {
                    provider: Some(ProviderSettings {
                        base_url: None,
                        model: Some("m2".into()),
                        protocol: None,
                        key: None,
                        fake: None,
                    }),
                    approval: None,
                },
            )],
        );
        let merged = registry.settings_for(&s).expect("settings registered");
        let p = merged.provider.as_ref().expect("provider present");
        assert_eq!(p.base_url.as_deref(), Some("https://a.example"));
        assert_eq!(p.model.as_deref(), Some("m2"));
    }

    #[test]
    fn settings_empty_key_reference_is_rejected() {
        let s = scope("app");
        let registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let cases = [
            KeyReference::Env {
                name: String::new(),
            },
            KeyReference::Keychain {
                service: String::new(),
                account: "acct".into(),
            },
            KeyReference::Keychain {
                service: "svc".into(),
                account: String::new(),
            },
        ];
        for key in cases {
            let c = settings(
                &s,
                SettingsContribution {
                    provider: Some(provider_with_key(key.clone())),
                    approval: None,
                },
            );
            let err = registry.validate(&[c]).unwrap_err();
            assert!(
                matches!(err, ScopeError::InvalidContribution { ref scope, .. } if scope == &s),
                "empty key reference {key:?} must be rejected"
            );
        }
        // a well-formed key reference validates
        let ok = settings(
            &s,
            SettingsContribution {
                provider: Some(provider_with_key(KeyReference::Keychain {
                    service: "svc".into(),
                    account: "acct".into(),
                })),
                approval: None,
            },
        );
        registry.validate(&[ok]).unwrap();
    }

    #[test]
    fn remove_scope_drops_settings() {
        let s = scope("app");
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        validate_and_apply(
            &mut registry,
            &s,
            &[settings(
                &s,
                SettingsContribution {
                    provider: Some(ProviderSettings {
                        base_url: Some("https://a.example".into()),
                        model: None,
                        protocol: None,
                        key: None,
                        fake: None,
                    }),
                    approval: None,
                },
            )],
        );
        assert!(registry.settings_for(&s).is_some());
        let removed = registry.remove_scope(&s, false).unwrap();
        assert!(registry.settings_for(&s).is_none(), "settings dropped");
        assert!(registry.snapshot().is_empty());
        assert!(
            removed
                .contributions
                .iter()
                .any(|c| matches!(c.kind, ContributionKind::Settings(_)))
        );
    }

    #[test]
    fn settings_kind_does_not_alter_existing_kind_encodings() {
        // the composition digest domain is unchanged for existing kinds: the
        // externally-tagged byte shape of every pre-existing variant is stable.
        assert_eq!(
            serde_json::to_value(ContributionKind::Theme(ThemeContribution {
                name: "t".into(),
                overlay: json!({"a": 1}),
            }))
            .unwrap(),
            json!({"Theme": {"name": "t", "overlay": {"a": 1}}})
        );
        assert_eq!(
            ContributionKind::Settings(SettingsContribution {
                provider: None,
                approval: None,
            })
            .kind_tag(),
            "settings"
        );
        assert_eq!(
            serde_json::to_value(ContributionKind::Settings(SettingsContribution {
                provider: None,
                approval: None,
            }))
            .unwrap(),
            json!({"Settings": {"provider": null, "approval": null}})
        );
    }

    // --- decision 28: precedence-driven implicit replacement -------------

    fn command(s: &ScopePath, name: &str, handler: &str) -> Contribution {
        Contribution {
            scope: s.clone(),
            kind: ContributionKind::Command(CommandContribution {
                name: name.into(),
                handler: handler.into(),
            }),
        }
    }

    fn tool(s: &ScopePath, name: &str, handler: &str) -> Contribution {
        Contribution {
            scope: s.clone(),
            kind: ContributionKind::Tool(ToolContribution {
                name: name.into(),
                manifest: json!({"replay_relevant": true}),
                handler: handler.into(),
            }),
        }
    }

    fn service(s: &ScopePath, name: &str, provider: ServiceProvider) -> Contribution {
        Contribution {
            scope: s.clone(),
            kind: ContributionKind::Service(ServiceContribution {
                key: ServiceKey {
                    scope: s.clone(),
                    name: name.into(),
                },
                provider,
                deps: vec![],
            }),
        }
    }

    fn plan(removed: Vec<Contribution>) -> OverridePlan {
        OverridePlan { removed }
    }

    /// A higher-precedence layer replaces a lower command/tool of the same
    /// (scope, name); validation accepts it only because the plan names the
    /// displaced holder, and exactly one holder remains afterwards.
    #[test]
    fn override_plan_replaces_lower_command_and_tool() {
        let s = scope("app");
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let lower_cmd = command(&s, "run", "builtin_h");
        let lower_tool = tool(&s, "sh", "builtin_t");
        validate_and_apply(&mut registry, &s, &[lower_cmd.clone(), lower_tool.clone()]);

        let higher_cmd = command(&s, "run", "project_h");
        let higher_tool = tool(&s, "sh", "project_t");
        let pl = plan(vec![lower_cmd, lower_tool]);
        // without the plan the higher layer conflicts
        assert!(registry.validate(&[higher_cmd.clone()]).is_err());
        registry
            .validate_planned(&[higher_cmd.clone(), higher_tool.clone()], &pl)
            .unwrap();
        registry
            .apply_planned(
                &s,
                &[higher_cmd.clone(), higher_tool.clone()],
                &pl,
            )
            .unwrap();

        let snap = registry.snapshot();
        let commands: Vec<_> = snap
            .iter()
            .filter_map(|c| match &c.kind {
                ContributionKind::Command(cmd) => Some(cmd.handler.as_str()),
                _ => None,
            })
            .collect();
        let tools: Vec<_> = snap
            .iter()
            .filter_map(|c| match &c.kind {
                ContributionKind::Tool(t) => Some(t.handler.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(commands, ["project_h"], "one command holder, the higher one");
        assert_eq!(tools, ["project_t"], "one tool holder, the higher one");
    }

    /// A higher-precedence layer replaces a lower service provider at the same
    /// key; the lower provider no longer resolves and the higher one does.
    #[test]
    fn override_plan_replaces_lower_service_provider() {
        let s = scope("app");
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let lower = service(&s, "greeter", provider("greeter", 1));
        validate_and_apply(&mut registry, &s, &[lower.clone()]);

        let higher_provider = provider("greeter", 3);
        let higher = service(&s, "greeter", higher_provider.clone());
        let pl = plan(vec![lower]);
        assert!(registry.validate(&[higher.clone()]).is_err());
        registry.validate_planned(&[higher.clone()], &pl).unwrap();
        registry.apply_planned(&s, &[higher], &pl).unwrap();

        let reg = registry.services.lock().unwrap();
        let resolved = reg
            .resolve(
                &ServiceKey {
                    scope: s.clone(),
                    name: "greeter".into(),
                },
                3,
                &s,
            )
            .expect("higher provider resolves")
            .clone();
        assert_eq!(resolved.contract.version, 3);
        assert_eq!(resolved.module_id, higher_provider.module_id);
        assert!(
            reg.resolve(
                &ServiceKey {
                    scope: s.clone(),
                    name: "greeter".into(),
                },
                1,
                &s,
            )
            .is_err(),
            "the lower v1 provider is gone"
        );
    }

    /// Keymaps, themes and settings are layered/merged kinds: a plan never
    /// names them, and both layers survive with later-wins semantics.
    #[test]
    fn keymaps_themes_and_settings_still_layer() {
        let s = scope("app");
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let lower_keymap = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Keymap(KeymapContribution {
                key: "ctrl-k".into(),
                action: "lower".into(),
            }),
        };
        let lower_theme = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Theme(ThemeContribution {
                name: "default".into(),
                overlay: json!({"bg": "black", "fg": "white"}),
            }),
        };
        validate_and_apply(
            &mut registry,
            &s,
            &[
                lower_keymap.clone(),
                lower_theme.clone(),
                settings(
                    &s,
                    SettingsContribution {
                        approval: Some(ApprovalSettings {
                            auto_approve: Some(true),
                            yolo: None,
                        }),
                        provider: None,
                    },
                ),
            ],
        );

        let higher_keymap = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Keymap(KeymapContribution {
                key: "ctrl-k".into(),
                action: "higher".into(),
            }),
        };
        let higher_theme = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Theme(ThemeContribution {
                name: "default".into(),
                overlay: json!({"bg": "blue"}),
            }),
        };
        // A plan removing an unrelated entry must not disturb the layered kinds.
        let unrelated = command(&s, "unrelated", "h");
        validate_and_apply(&mut registry, &s, &[unrelated.clone()]);
        let pl = plan(vec![unrelated]);
        registry
            .validate_planned(&[higher_keymap.clone(), higher_theme.clone()], &pl)
            .unwrap();
        registry
            .apply_planned(&s, &[higher_keymap, higher_theme], &pl)
            .unwrap();

        assert_eq!(
            registry.keymap_for(&s, "ctrl-k").map(|k| k.action.as_str()),
            Some("higher"),
            "keymap layers, last wins"
        );
        let theme = registry.theme_overlay(&s, "default").unwrap();
        assert_eq!(theme.overlay, json!({"bg": "blue", "fg": "white"}));
        assert_eq!(
            registry
                .settings_for(&s)
                .and_then(|s| s.approval.as_ref())
                .and_then(|a| a.auto_approve),
            Some(true),
            "lower settings layer survives"
        );
    }

    /// F11: a precedence plan never un-merges settings (the intended
    /// asymmetry) — a displaced lower layer's fields survive. An explicit
    /// `remove_contributions` DOES drop the scope's merged settings.
    #[test]
    fn override_plan_never_unmerges_settings() {
        let s = scope("app");
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let lower = settings(
            &s,
            SettingsContribution {
                approval: Some(ApprovalSettings {
                    auto_approve: Some(true),
                    yolo: None,
                }),
                provider: None,
            },
        );
        validate_and_apply(&mut registry, &s, &[lower.clone()]);

        let higher = settings(
            &s,
            SettingsContribution {
                provider: Some(ProviderSettings {
                    model: Some("m".into()),
                    ..Default::default()
                }),
                approval: None,
            },
        );
        let pl = plan(vec![lower]);
        registry.validate_planned(&[higher.clone()], &pl).unwrap();
        registry.apply_planned(&s, &[higher], &pl).unwrap();

        let merged = registry.settings_for(&s).unwrap();
        assert_eq!(
            merged
                .approval
                .as_ref()
                .and_then(|a| a.auto_approve),
            Some(true),
            "the plan does not un-merge the lower settings layer"
        );
        assert_eq!(
            merged.provider.as_ref().and_then(|p| p.model.as_deref()),
            Some("m")
        );

        registry
            .remove_contributions(&[settings(&s, merged.clone())])
            .unwrap();
        assert!(
            registry.settings_for(&s).is_none(),
            "an explicit removal drops the scope's merged settings"
        );
    }

    /// Safe-mode rollback (R-01/C-02): overlay kinds cannot be un-merged by
    /// name, so `recompose_overlays` replays only the surviving layers'
    /// overlays — a dropped layer's settings/theme residue is gone.
    #[test]
    fn recompose_overlays_drops_a_layer_residue() {
        let s = scope("");
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let builtin = settings(
            &s,
            SettingsContribution {
                provider: Some(ProviderSettings {
                    protocol: Some("openai".into()),
                    ..Default::default()
                }),
                approval: Some(ApprovalSettings {
                    auto_approve: Some(false),
                    yolo: Some(false),
                }),
            },
        );
        let user = settings(
            &s,
            SettingsContribution {
                provider: Some(ProviderSettings {
                    model: Some("user-model".into()),
                    ..Default::default()
                }),
                approval: Some(ApprovalSettings {
                    auto_approve: Some(true),
                    yolo: None,
                }),
            },
        );
        validate_and_apply(&mut registry, &s, &[builtin.clone(), user]);
        let merged = registry.settings_for(&s).unwrap();
        assert_eq!(merged.approval.as_ref().unwrap().auto_approve, Some(true));

        // Drop the user layer: replay only the built-in overlay.
        registry.recompose_overlays(&s, &[builtin]);
        let rebuilt = registry.settings_for(&s).unwrap();
        let approval = rebuilt.approval.as_ref().unwrap();
        assert_eq!(approval.auto_approve, Some(false), "user overlay is gone");
        assert_eq!(approval.yolo, Some(false), "built-in default survives");
        assert_eq!(
            rebuilt.provider.as_ref().and_then(|p| p.model.as_deref()),
            None,
            "user provider field is gone"
        );
    }
}
