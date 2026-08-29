//! kanbei-services — the M2 service DAG: versioned service contracts, scoped
//! keys, publication with replace intent, dependency resolution, and the
//! replacement policy. Design inputs: docs/architecture.md (R-25/C-05,
//! R-25/C-06) and docs/review-reconciliation.md (R-25).
//
// The public API fixes `ServiceError` fields as unboxed values (e.g.
// `Conflict { holder: ServiceProvider, .. }`), so the large-err lint cannot be
// addressed by boxing without changing the contract.
#![allow(clippy::result_large_err)]

use std::collections::HashMap;
use std::fmt;

use kanbei_core::Id128;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// A versioned service contract (R-25/C-05). Dependents declare the exact
/// version they require; resolution matches versions exactly (M2).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ServiceContract {
    pub name: String,
    pub version: u32,
}

/// An ordered scope path; the root scope is the empty path and displays as
/// `/`, `/root/child` as `/root/child`.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ScopePath(pub Vec<String>);

impl fmt::Display for ScopePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "/{}", self.0.join("/"))
    }
}

impl ScopePath {
    /// `self` equals `other` or is an ancestor of it (a prefix of it).
    fn is_same_or_ancestor_of(&self, other: &ScopePath) -> bool {
        other.0.starts_with(&self.0)
    }
}

/// A scoped service key, `ScopePath/Name`, namespaced by the owning module
/// (R-25/C-06).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ServiceKey {
    pub scope: ScopePath,
    pub name: String,
}

impl fmt::Display for ServiceKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.scope, self.name)
    }
}

/// A dependent's declaration of a required service version (R-25/C-05).
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ServiceDependency {
    pub key: ServiceKey,
    pub required_version: u32,
}

/// The module generation that provides a service contract.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ServiceProvider {
    pub module_id: Id128,
    pub generation: u64,
    pub contract: ServiceContract,
}

/// Explicit replace intent: `current` must name the registered holder
/// (module_id + generation), else the replacement fails with `Conflict`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplaceIntent {
    pub current: ServiceProvider,
    pub proposed: ServiceProvider,
}

/// Typed service-DAG errors.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ServiceError {
    #[error("service `{key}` is held by {holder:?}; challenger {challenger:?} cannot take it over")]
    Conflict {
        key: ServiceKey,
        holder: ServiceProvider,
        challenger: ServiceProvider,
    },
    #[error("service `{key}` at required version {required_version} is not published")]
    Unresolved { key: ServiceKey, required_version: u32 },
    #[error("scope violation resolving `{key}` from caller scope `{caller_scope}`: {reason}")]
    ScopeViolation {
        key: ServiceKey,
        caller_scope: ScopePath,
        reason: String,
    },
    #[error("dependency cycle: {}", path.join(" -> "))]
    DependencyCycle { path: Vec<String> },
    #[error("service `{key}` still has dependents: {dependents:?}")]
    DependentsExist {
        key: ServiceKey,
        dependents: Vec<ServiceDependency>,
    },
    #[error("module {module_id} does not own service `{key}`")]
    NotOwner { key: ServiceKey, module_id: Id128 },
    #[error("service `{key}` is at version {actual_version}, required {required_version}")]
    VersionMismatch {
        key: ServiceKey,
        required_version: u32,
        actual_version: u32,
    },
    #[error("invalid input: {0}")]
    InvalidInput(String),
}

/// The service DAG: key → provider publications plus each key's declared
/// dependency edges. All mutations happen through here; the replacement policy
/// lives in [`replacement`].
#[derive(Debug, Default)]
pub struct ServiceRegistry {
    holders: HashMap<ServiceKey, ServiceProvider>,
    deps: HashMap<ServiceKey, Vec<ServiceDependency>>,
}

impl ServiceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    fn validate_key_contract(
        key: &ServiceKey,
        provider: &ServiceProvider,
    ) -> Result<(), ServiceError> {
        if key.name != provider.contract.name {
            return Err(ServiceError::InvalidInput(format!(
                "contract name `{}` does not match key name `{}`",
                provider.contract.name, key.name
            )));
        }
        Ok(())
    }

    /// Publishes `provider` under `key`. The key must be free, or an explicit
    /// [`ReplaceIntent`] must be supplied via [`Self::replace_publish`]
    /// (R-25/C-06). The owning module is `provider.module_id`.
    pub fn publish(&mut self, key: ServiceKey, provider: ServiceProvider) -> Result<(), ServiceError> {
        Self::validate_key_contract(&key, &provider)?;
        if let Some(holder) = self.holders.get(&key) {
            return Err(ServiceError::Conflict {
                key,
                holder: holder.clone(),
                challenger: provider,
            });
        }
        self.holders.insert(key.clone(), provider);
        self.deps.insert(key, Vec::new());
        Ok(())
    }

    /// Replaces the holder of `key`. `replace_intent.current` must name the
    /// registered holder (module_id + generation), else
    /// [`ServiceError::Conflict`] names the holder, challenger, and key.
    pub fn replace_publish(
        &mut self,
        key: ServiceKey,
        provider: ServiceProvider,
        replace_intent: &ReplaceIntent,
    ) -> Result<(), ServiceError> {
        Self::validate_key_contract(&key, &provider)?;
        if replace_intent.proposed != provider {
            return Err(ServiceError::InvalidInput(
                "replace intent's proposed provider differs from the published provider".into(),
            ));
        }
        let Some(holder) = self.holders.get(&key) else {
            return Err(ServiceError::InvalidInput(format!(
                "replace intent for `{key}`: no current holder"
            )));
        };
        if holder.module_id != replace_intent.current.module_id
            || holder.generation != replace_intent.current.generation
        {
            return Err(ServiceError::Conflict {
                key,
                holder: holder.clone(),
                challenger: provider,
            });
        }
        self.deps.entry(key.clone()).or_default();
        self.holders.insert(key, provider);
        Ok(())
    }

    /// Resolves `key` for a caller in `caller_scope`. The service scope must be
    /// the caller scope or an ancestor of it — dependencies may point only to
    /// same-scope or ancestor-scope services; parent→child and unrelated
    /// scopes are rejected (R-25/C-06). Version-compatible means the provider
    /// contract version equals `required_version` exactly (M2).
    pub fn resolve(
        &self,
        key: &ServiceKey,
        required_version: u32,
        caller_scope: &ScopePath,
    ) -> Result<&ServiceProvider, ServiceError> {
        if !key.scope.is_same_or_ancestor_of(caller_scope) {
            return Err(ServiceError::ScopeViolation {
                key: key.clone(),
                caller_scope: caller_scope.clone(),
                reason: format!(
                    "service scope `{}` is neither the caller scope `{caller_scope}` nor an ancestor of it",
                    key.scope
                ),
            });
        }
        let Some(holder) = self.holders.get(key) else {
            return Err(ServiceError::Unresolved {
                key: key.clone(),
                required_version,
            });
        };
        if holder.contract.version != required_version {
            return Err(ServiceError::VersionMismatch {
                key: key.clone(),
                required_version,
                actual_version: holder.contract.version,
            });
        }
        Ok(holder)
    }

    /// Reverse edges: every dependent of `key` (its key and the version it
    /// requires) in deterministic (key-sorted) order — the input to
    /// replacement and restart planning.
    pub fn dependents_of(&self, key: &ServiceKey) -> Vec<ServiceDependency> {
        let mut out = Vec::new();
        for (dependent, deps) in &self.deps {
            for d in deps {
                if &d.key == key {
                    out.push(ServiceDependency {
                        key: dependent.clone(),
                        required_version: d.required_version,
                    });
                }
            }
        }
        out.sort_by_key(|a| a.key.to_string());
        out
    }

    /// Rejects dependencies pointing at child or unrelated scopes (R-25/C-06)
    /// and any cycle the new edges would introduce into the full graph
    /// (existing publications + `new_deps`) — required-dependency cycle
    /// rejection. The new node is `(scope, provider.contract.name)`; the
    /// reported cycle path uses scoped key display strings with the start
    /// repeated at the end (e.g. `A -> B -> A`).
    pub fn check_dependency_cycle(
        &self,
        new_deps: &[ServiceDependency],
        provider: &ServiceProvider,
        scope: &ScopePath,
    ) -> Result<(), ServiceError> {
        for dep in new_deps {
            if !dep.key.scope.is_same_or_ancestor_of(scope) {
                return Err(ServiceError::ScopeViolation {
                    key: dep.key.clone(),
                    caller_scope: scope.clone(),
                    reason: format!(
                        "dependency scope `{}` is neither the dependent scope `{scope}` nor an ancestor of it",
                        dep.key.scope
                    ),
                });
            }
        }

        let new_key = ServiceKey {
            scope: scope.clone(),
            name: provider.contract.name.clone(),
        };
        let mut successors: HashMap<ServiceKey, Vec<ServiceKey>> = self
            .deps
            .iter()
            .map(|(k, deps)| (k.clone(), deps.iter().map(|d| d.key.clone()).collect()))
            .collect();
        successors
            .entry(new_key)
            .or_default()
            .extend(new_deps.iter().map(|d| d.key.clone()));

        let mut keys: Vec<ServiceKey> = successors.keys().cloned().collect();
        keys.sort_by_key(|k| k.to_string());
        let index: HashMap<&ServiceKey, usize> =
            keys.iter().enumerate().map(|(i, k)| (k, i)).collect();

        let mut color = vec![DfsColor::White; keys.len()];
        let mut path = Vec::new();
        for start in 0..keys.len() {
            if color[start] != DfsColor::White {
                continue;
            }
            if let Some(cycle) = visit(&keys, &successors, &index, &mut color, &mut path, start) {
                return Err(ServiceError::DependencyCycle { path: cycle });
            }
        }
        Ok(())
    }

    /// Publishes with declared dependencies: runs [`Self::check_dependency_cycle`]
    /// first, then publishes. A provider's dependencies are declared at
    /// publish; [`Self::publish`] alone means no dependencies.
    pub fn publish_with_deps(
        &mut self,
        key: ServiceKey,
        provider: ServiceProvider,
        deps: &[ServiceDependency],
    ) -> Result<(), ServiceError> {
        self.check_dependency_cycle(deps, &provider, &key.scope)?;
        self.publish(key.clone(), provider)?;
        self.deps.insert(key, deps.to_vec());
        Ok(())
    }

    /// Removes a publication and its dependency edges. Only the owning module
    /// may remove; a key with dependents must be handled via a replacement
    /// transaction, not silent removal.
    pub fn remove(&mut self, key: &ServiceKey, module_id: Id128) -> Result<(), ServiceError> {
        let Some(holder) = self.holders.get(key) else {
            return Err(ServiceError::InvalidInput(format!("service `{key}` is not published")));
        };
        if holder.module_id != module_id {
            return Err(ServiceError::NotOwner {
                key: key.clone(),
                module_id,
            });
        }
        let dependents = self.dependents_of(key);
        if !dependents.is_empty() {
            return Err(ServiceError::DependentsExist {
                key: key.clone(),
                dependents,
            });
        }
        self.holders.remove(key);
        self.deps.remove(key);
        Ok(())
    }

    /// Full registry state as `(key, provider, declared dependencies)` in
    /// deterministic order — for scope transactions and manifest
    /// materialization.
    pub fn snapshot(&self) -> Vec<(ServiceKey, ServiceProvider, Vec<ServiceDependency>)> {
        let mut out: Vec<_> = self
            .holders
            .iter()
            .map(|(key, provider)| {
                (
                    key.clone(),
                    provider.clone(),
                    self.deps.get(key).cloned().unwrap_or_default(),
                )
            })
            .collect();
        out.sort_by_key(|a| a.0.to_string());
        out
    }

    pub(crate) fn holder(&self, key: &ServiceKey) -> Option<&ServiceProvider> {
        self.holders.get(key)
    }

    pub(crate) fn deps_of(&self, key: &ServiceKey) -> &[ServiceDependency] {
        self.deps.get(key).map(Vec::as_slice).unwrap_or_default()
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DfsColor {
    White,
    Gray,
    Black,
}

/// Depth-first search for a cycle; reports it as scoped key display strings
/// with the cycle start repeated at the end when a back edge hits a gray node.
fn visit(
    keys: &[ServiceKey],
    successors: &HashMap<ServiceKey, Vec<ServiceKey>>,
    index: &HashMap<&ServiceKey, usize>,
    color: &mut [DfsColor],
    path: &mut Vec<usize>,
    node: usize,
) -> Option<Vec<String>> {
    color[node] = DfsColor::Gray;
    path.push(node);
    let mut next: Vec<usize> = successors
        .get(&keys[node])
        .map(|succs| succs.iter().filter_map(|k| index.get(k).copied()).collect())
        .unwrap_or_default();
    next.sort_unstable();
    for succ in next {
        match color[succ] {
            DfsColor::Gray => {
                let start = path
                    .iter()
                    .position(|&p| p == succ)
                    .expect("a gray node is on the current path");
                let mut cycle: Vec<String> =
                    path[start..].iter().map(|&p| keys[p].to_string()).collect();
                cycle.push(keys[succ].to_string());
                return Some(cycle);
            }
            DfsColor::White => {
                if let Some(cycle) = visit(keys, successors, index, color, path, succ) {
                    return Some(cycle);
                }
            }
            DfsColor::Black => {}
        }
    }
    path.pop();
    color[node] = DfsColor::Black;
    None
}

/// Replacement policy (R-25/C-05) — pure planning; the transaction machinery
/// that executes a plan lives in kanbei-scopes.
pub mod replacement {
    use super::*;

    /// Which direct dependents keep running (`rebind`) and which must be
    /// restarted inside the replacement transaction (`restart`).
    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    pub struct ReplacementPlan {
        pub rebind: Vec<ServiceKey>,
        pub restart: Vec<ServiceKey>,
    }

    /// Plans the replacement of `key` by `new_provider`:
    /// - dependents whose `required_version` equals the new contract version
    ///   rebind atomically — they keep running and their resolution updates;
    /// - version-incompatible dependents restart inside the same transaction;
    ///   a restarting dependent whose remaining dependencies (excluding the
    ///   replaced service — the mismatch with it is the restart trigger) do
    ///   not resolve after the replacement cannot restart, which fails the
    ///   whole transaction with [`ServiceError::Unresolved`] naming the
    ///   dependent (reject is subsumed).
    pub fn plan_replacement(
        registry: &ServiceRegistry,
        key: &ServiceKey,
        new_provider: &ServiceProvider,
    ) -> Result<ReplacementPlan, ServiceError> {
        let mut plan = ReplacementPlan {
            rebind: Vec::new(),
            restart: Vec::new(),
        };
        for dep in registry.dependents_of(key) {
            if dep.required_version == new_provider.contract.version {
                plan.rebind.push(dep.key);
            } else {
                plan.restart.push(dep.key);
            }
        }
        for dependent in &plan.restart {
            for dep in registry.deps_of(dependent) {
                if &dep.key == key {
                    continue;
                }
                let Some(provider) = registry.holder(&dep.key) else {
                    return Err(ServiceError::Unresolved {
                        key: dependent.clone(),
                        required_version: dep.required_version,
                    });
                };
                if provider.contract.version != dep.required_version {
                    return Err(ServiceError::Unresolved {
                        key: dependent.clone(),
                        required_version: dep.required_version,
                    });
                }
            }
        }
        Ok(plan)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(scope: &[&str], name: &str) -> ServiceKey {
        ServiceKey {
            scope: ScopePath(scope.iter().map(|s| s.to_string()).collect()),
            name: name.to_string(),
        }
    }

    fn root() -> ScopePath {
        ScopePath(vec!["root".to_string()])
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

    fn dep(key: ServiceKey, required_version: u32) -> ServiceDependency {
        ServiceDependency {
            key,
            required_version,
        }
    }

    #[test]
    fn publish_and_resolve_same_scope() {
        let mut reg = ServiceRegistry::new();
        let k = key(&["root"], "svc");
        reg.publish(k.clone(), provider("svc", 3)).unwrap();
        assert_eq!(reg.resolve(&k, 3, &root()).unwrap().contract.version, 3);
        let err = reg.resolve(&k, 4, &root()).unwrap_err();
        assert!(matches!(
            err,
            ServiceError::VersionMismatch {
                required_version: 4,
                actual_version: 3,
                ..
            }
        ));
    }

    #[test]
    fn ancestor_scope_resolution_ok() {
        let mut reg = ServiceRegistry::new();
        let k = key(&["root"], "svc");
        reg.publish(k.clone(), provider("svc", 1)).unwrap();
        let caller = ScopePath(vec!["root".to_string(), "child".to_string()]);
        assert_eq!(reg.resolve(&k, 1, &caller).unwrap().contract.name, "svc");
    }

    #[test]
    fn parent_to_child_and_unrelated_resolution_rejected() {
        let mut reg = ServiceRegistry::new();
        let k = key(&["root", "child"], "svc");
        reg.publish(k.clone(), provider("svc", 1)).unwrap();
        let err = reg.resolve(&k, 1, &root()).unwrap_err();
        assert!(matches!(err, ServiceError::ScopeViolation { .. }));
        let err = reg
            .resolve(&k, 1, &ScopePath(vec!["other".to_string()]))
            .unwrap_err();
        assert!(matches!(err, ServiceError::ScopeViolation { .. }));
    }

    #[test]
    fn publish_conflict_and_replace_intent() {
        let mut reg = ServiceRegistry::new();
        let k = key(&["root"], "svc");
        let first = provider("svc", 1);
        reg.publish(k.clone(), first.clone()).unwrap();

        // second publish without replace intent → Conflict naming all three
        let second = provider("svc", 2);
        let err = reg.publish(k.clone(), second.clone()).unwrap_err();
        match err {
            ServiceError::Conflict {
                key: k2,
                holder,
                challenger,
            } => {
                assert_eq!(k2, k);
                assert_eq!(holder, first);
                assert_eq!(challenger, second);
            }
            other => panic!("expected Conflict, got {other:?}"),
        }

        // replace intent naming the current holder → Ok
        let intent = ReplaceIntent {
            current: first.clone(),
            proposed: second.clone(),
        };
        reg.replace_publish(k.clone(), second.clone(), &intent).unwrap();
        assert_eq!(reg.resolve(&k, 2, &root()).unwrap().contract.version, 2);

        // stale intent (holder changed) → Conflict
        let challenger = provider("svc", 3);
        let stale = ReplaceIntent {
            current: first,
            proposed: challenger.clone(),
        };
        let err = reg.replace_publish(k.clone(), challenger, &stale).unwrap_err();
        assert!(matches!(err, ServiceError::Conflict { .. }));

        // intent whose proposed provider differs from the payload → InvalidInput
        let mismatched = ReplaceIntent {
            current: second.clone(),
            proposed: provider("svc", 9),
        };
        let err = reg.replace_publish(k.clone(), second, &mismatched).unwrap_err();
        assert!(matches!(err, ServiceError::InvalidInput(_)));

        // replace intent for a free key → InvalidInput
        let free = key(&["root"], "free");
        let err = reg
            .replace_publish(
                free,
                provider("free", 1),
                &ReplaceIntent {
                    current: provider("free", 1),
                    proposed: provider("free", 1),
                },
            )
            .unwrap_err();
        assert!(matches!(err, ServiceError::InvalidInput(_)));
    }

    #[test]
    fn dependency_cycle_rejected() {
        let mut reg = ServiceRegistry::new();
        let a = key(&["root"], "a");
        let b = key(&["root"], "b");
        reg.publish_with_deps(a.clone(), provider("a", 1), &[dep(b.clone(), 1)])
            .unwrap();

        // B depends on A while A depends on B → cycle naming both
        let err = reg
            .publish_with_deps(b.clone(), provider("b", 1), &[dep(a.clone(), 1)])
            .unwrap_err();
        match err {
            ServiceError::DependencyCycle { path } => {
                assert!(path.iter().any(|p| p.contains("/a")));
                assert!(path.iter().any(|p| p.contains("/b")));
                assert_eq!(path.first(), path.last());
            }
            other => panic!("expected DependencyCycle, got {other:?}"),
        }

        // self-dependency → cycle
        let err = reg
            .publish_with_deps(b.clone(), provider("b", 2), &[dep(b.clone(), 2)])
            .unwrap_err();
        assert!(matches!(err, ServiceError::DependencyCycle { .. }));

        // the failed publishes left the registry unchanged
        assert!(matches!(
            reg.resolve(&b, 1, &root()),
            Err(ServiceError::Unresolved { .. })
        ));
    }

    #[test]
    fn check_dependency_cycle_direct() {
        let mut reg = ServiceRegistry::new();
        let a = key(&["root"], "a");
        let b = key(&["root"], "b");
        reg.publish_with_deps(a.clone(), provider("a", 1), &[dep(b.clone(), 1)])
            .unwrap();

        let err = reg
            .check_dependency_cycle(&[dep(a.clone(), 1)], &provider("b", 1), &root())
            .unwrap_err();
        assert!(matches!(err, ServiceError::DependencyCycle { .. }));

        // an acyclic addition passes
        let c = key(&["root"], "c");
        reg.check_dependency_cycle(&[dep(c, 1)], &provider("b", 1), &root())
            .unwrap();

        // a child-scope dependency is rejected
        let err = reg
            .check_dependency_cycle(
                &[dep(key(&["root", "c"], "deep"), 1)],
                &provider("b", 1),
                &root(),
            )
            .unwrap_err();
        assert!(matches!(err, ServiceError::ScopeViolation { .. }));
    }

    #[test]
    fn remove_ownership_and_dependents() {
        let mut reg = ServiceRegistry::new();
        let a = key(&["root"], "a");
        let b = key(&["root"], "b");
        let owner = Id128::generate();
        let intruder = Id128::generate();

        reg.publish(
            a.clone(),
            ServiceProvider {
                module_id: owner,
                generation: 1,
                contract: ServiceContract {
                    name: "a".to_string(),
                    version: 1,
                },
            },
        )
        .unwrap();
        reg.publish_with_deps(
            b.clone(),
            ServiceProvider {
                module_id: owner,
                generation: 1,
                contract: ServiceContract {
                    name: "b".to_string(),
                    version: 1,
                },
            },
            &[dep(a.clone(), 1)],
        )
        .unwrap();

        // non-owner → NotOwner, even when dependents exist
        let err = reg.remove(&a, intruder).unwrap_err();
        assert!(matches!(err, ServiceError::NotOwner { .. }));

        // owner with a dependent → DependentsExist
        let err = reg.remove(&a, owner).unwrap_err();
        assert!(matches!(err, ServiceError::DependentsExist { .. }));

        // removing the dependent clears its edges; then the owner removes freely
        reg.remove(&b, owner).unwrap();
        reg.remove(&a, owner).unwrap();
        assert!(matches!(
            reg.resolve(&a, 1, &root()),
            Err(ServiceError::Unresolved { .. })
        ));
        assert!(reg.dependents_of(&a).is_empty());

        // removing an unpublished key → InvalidInput
        let err = reg.remove(&b, owner).unwrap_err();
        assert!(matches!(err, ServiceError::InvalidInput(_)));
    }

    #[test]
    fn plan_replacement_rebind_and_restart() {
        let mut reg = ServiceRegistry::new();
        let svc = key(&["root"], "svc");
        let helper = key(&["root"], "helper");
        reg.publish(svc.clone(), provider("svc", 1)).unwrap();
        reg.publish(helper.clone(), provider("helper", 5)).unwrap();

        let compat = key(&["root"], "compat");
        let incompat = key(&["root"], "incompat");
        reg.publish_with_deps(
            compat.clone(),
            provider("compat", 1),
            &[dep(svc.clone(), 2)],
        )
        .unwrap();
        reg.publish_with_deps(
            incompat.clone(),
            provider("incompat", 1),
            &[dep(svc.clone(), 1), dep(helper, 5)],
        )
        .unwrap();

        // svc v1 → v2: compat (requires v2) rebinds, incompat (requires v1)
        // restarts; incompat's remaining dependency (helper v5) still resolves.
        let plan = replacement::plan_replacement(&reg, &svc, &provider("svc", 2)).unwrap();
        assert_eq!(plan.rebind, vec![compat]);
        assert_eq!(plan.restart, vec![incompat]);
    }

    #[test]
    fn plan_replacement_restart_failure_fails_transaction() {
        let mut reg = ServiceRegistry::new();
        let svc = key(&["root"], "svc");
        reg.publish(svc.clone(), provider("svc", 1)).unwrap();

        // restart-dependent whose remaining dependency is not published
        let ghost = key(&["root"], "ghost");
        let failing = key(&["root"], "failing");
        reg.publish_with_deps(
            failing.clone(),
            provider("failing", 1),
            &[dep(svc.clone(), 1), dep(ghost, 3)],
        )
        .unwrap();

        let err = replacement::plan_replacement(&reg, &svc, &provider("svc", 2)).unwrap_err();
        match err {
            ServiceError::Unresolved {
                key,
                required_version,
            } => {
                assert_eq!(key, failing);
                assert_eq!(required_version, 3);
            }
            other => panic!("expected Unresolved, got {other:?}"),
        }

        // a version-mismatched remaining dependency also fails the transaction
        let helper = key(&["root"], "helper");
        reg.publish(helper.clone(), provider("helper", 5)).unwrap();
        let mismatch = key(&["root"], "mismatch");
        reg.publish_with_deps(
            mismatch.clone(),
            provider("mismatch", 1),
            &[dep(svc.clone(), 1), dep(helper, 6)],
        )
        .unwrap();
        let err = replacement::plan_replacement(&reg, &svc, &provider("svc", 2)).unwrap_err();
        assert!(matches!(err, ServiceError::Unresolved { .. }));
    }

    #[test]
    fn snapshot_shape_and_json_roundtrip() {
        let mut reg = ServiceRegistry::new();
        let svc = key(&["root"], "svc");
        let child = key(&["root", "child"], "tool");
        let p = provider("svc", 2);
        reg.publish(svc.clone(), p.clone()).unwrap();
        reg.publish_with_deps(child.clone(), provider("tool", 1), &[dep(svc.clone(), 2)])
            .unwrap();

        let snap = reg.snapshot();
        assert_eq!(snap.len(), 2);
        let (key1, provider1, deps1) = &snap[0];
        assert_eq!(key1, &child); // /root/child/tool sorts before /root/svc
        assert_eq!(provider1.contract.version, 1);
        assert_eq!(deps1, &[dep(svc.clone(), 2)]);
        let (key2, provider2, deps2) = &snap[1];
        assert_eq!(key2, &svc);
        assert_eq!(provider2, &p);
        assert!(deps2.is_empty());

        // serde round-trip of a snapshot entry
        let json = serde_json::to_string(&snap[0]).unwrap();
        let back: (ServiceKey, ServiceProvider, Vec<ServiceDependency>) =
            serde_json::from_str(&json).unwrap();
        assert_eq!(back, snap[0].clone());

        // display forms
        assert_eq!(ScopePath(vec![]).to_string(), "/");
        assert_eq!(root().to_string(), "/root");
        assert_eq!(key(&["root", "child"], "tool").to_string(), "/root/child/tool");
    }

    #[test]
    fn cross_scope_dependencies() {
        let mut reg = ServiceRegistry::new();
        let svc = key(&["root"], "svc");
        reg.publish(svc.clone(), provider("svc", 1)).unwrap();

        // a provider in /root/child may depend on the ancestor /root/service
        let child_svc = key(&["root", "child"], "child-svc");
        reg.publish_with_deps(
            child_svc.clone(),
            provider("child-svc", 1),
            &[dep(svc.clone(), 1)],
        )
        .unwrap();
        assert_eq!(reg.dependents_of(&svc), vec![dep(child_svc.clone(), 1)]);

        // depending on a descendant scope service is rejected
        let grand = key(&["root", "child", "grand"], "grand-svc");
        reg.publish_with_deps(
            grand.clone(),
            provider("grand-svc", 1),
            &[dep(child_svc.clone(), 1)],
        )
        .unwrap();
        let err = reg
            .publish_with_deps(
                key(&["root", "child"], "other"),
                provider("other", 1),
                &[dep(grand, 1)],
            )
            .unwrap_err();
        assert!(matches!(err, ServiceError::ScopeViolation { .. }));

        // unrelated scopes are rejected too
        let unrelated = key(&["other"], "svc2");
        let err = reg
            .publish_with_deps(
                key(&["root"], "x"),
                provider("x", 1),
                &[dep(unrelated, 1)],
            )
            .unwrap_err();
        assert!(matches!(err, ServiceError::ScopeViolation { .. }));
    }

    #[test]
    fn replace_publish_preserves_dependency_edges() {
        let mut reg = ServiceRegistry::new();
        let svc = key(&["root"], "svc");
        let user = key(&["root"], "user");
        let first = provider("svc", 1);
        reg.publish(svc.clone(), first.clone()).unwrap();
        reg.publish_with_deps(user.clone(), provider("user", 1), &[dep(svc.clone(), 1)])
            .unwrap();

        let new = provider("svc", 2);
        reg.replace_publish(
            svc.clone(),
            new.clone(),
            &ReplaceIntent {
                current: first,
                proposed: new,
            },
        )
        .unwrap();
        assert_eq!(reg.dependents_of(&svc), vec![dep(user, 1)]);
    }

    #[test]
    fn publish_rejects_contract_name_mismatch() {
        let mut reg = ServiceRegistry::new();
        let err = reg.publish(key(&["root"], "svc"), provider("other", 1)).unwrap_err();
        assert!(matches!(err, ServiceError::InvalidInput(_)));
        assert!(reg.snapshot().is_empty());
    }
}
