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
    CommandContribution, Contribution, ContributionKind, GuardContribution, KeymapContribution,
    ProjectionStageContribution, ServiceContribution, ThemeContribution, ToolContribution,
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
            services,
        }
    }

    /// Validates a staged set against the current registrations (the current
    /// composition) and against earlier entries of the same set, applying the
    /// fixed per-type rules. Returns the first violation.
    pub fn validate(&self, staged: &[Contribution]) -> Result<(), ScopeError> {
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
            }
        }
        Ok(())
    }

    /// Atomically applies a validated staged set to `scope`: every
    /// contribution must carry that scope. All mutations happen on a clone of
    /// the registry; on success the clone is swapped in, so any failure (e.g.
    /// a service-dependency cycle detected by the service registry at publish
    /// time) rejects the whole set with no partial state. Callers must run
    /// [`Self::validate`] first; this method re-checks only the structural
    /// invariants it relies on (theme overlays must be objects for merging).
    pub fn apply(&mut self, scope: &ScopePath, staged: &[Contribution]) -> Result<(), ScopeError> {
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
        for c in staged {
            match &c.kind {
                ContributionKind::Service(s) => {
                    next.services
                        .lock()
                        .expect("services lock poisoned")
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
                    next.ui.insert((c.scope.clone(), u.name.clone()), u.clone());
                }
                ContributionKind::Guard(g) => {
                    next.guards
                        .insert((c.scope.clone(), g.name.clone()), g.clone());
                }
            }
        }
        *self = next;
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

        removed_contributions.extend(extras);
        removed_contributions.sort_by(|a, b| snapshot_sort_key(a).cmp(&snapshot_sort_key(b)));
        cascaded.sort_by_key(|a| a.to_string());
        cascaded.dedup();
        Ok(RemovedSet {
            contributions: removed_contributions,
            cascaded_scopes: cascaded,
        })
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
        self.ui.insert(key, new);
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

    /// The clone `apply` mutates: maps are cloned; the service registry is
    /// the shared kernel registry (same `Arc`), so `apply`'s service
    /// publications are visible to the module host immediately.
    fn clone_state(&self) -> ContributionRegistry {
        ContributionRegistry {
            commands: self.commands.clone(),
            tools: self.tools.clone(),
            keymaps: self.keymaps.clone(),
            themes: self.themes.clone(),
            stages: self.stages.clone(),
            ui: self.ui.clone(),
            guards: self.guards.clone(),
            services: Arc::clone(&self.services),
        }
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
}
