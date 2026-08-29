//! The M2 scope tree (R-26/C-09): a root scope plus ephemeral single-level
//! child scopes. Child scopes are created with an explicit owner lease
//! (generation or run), are name-unique within the root, and vanish on
//! restart — there is no persistence here (durable desired scopes and nested
//! scopes are MVP non-goals).

use std::collections::HashMap;

use kanbei_services::ScopePath;
use serde::{Deserialize, Serialize};

use crate::errors::ScopeError;
use crate::registry::ContributionRegistry;

/// The explicit owner lease of a scope (R-26/C-09): a module generation or a
/// session run. Serialization is derived so [`Scope`] records stay
/// serializable with their owner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum OwnerLease {
    Generation(u64),
    Run(u64),
}

/// A scope record in the tree.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Scope {
    pub path: ScopePath,
    pub owner: OwnerLease,
    pub parent: Option<ScopePath>,
    pub children: Vec<ScopePath>,
}

/// The scope tree. The OCC epoch counter deliberately does NOT live here — it
/// belongs to [`crate::epoch::CompositionStore`], the single source of truth
/// for the publish sequence.
#[derive(Debug)]
pub struct ScopeTree {
    scopes: HashMap<ScopePath, Scope>,
}

impl ScopeTree {
    /// A fresh tree with only the root scope: path `/` (the empty path),
    /// owner lease `Run(0)`, no parent.
    pub fn new_root() -> Self {
        let root_path = ScopePath(vec![]);
        let root = Scope {
            path: root_path.clone(),
            owner: OwnerLease::Run(0),
            parent: None,
            children: vec![],
        };
        let mut scopes = HashMap::new();
        scopes.insert(root_path, root);
        Self { scopes }
    }

    /// Creates a single-level child scope of the root with an explicit owner
    /// lease. The name must be unique within the root; nested scopes are
    /// rejected (MVP non-goal, R-26).
    pub fn create_child(
        &mut self,
        parent: &ScopePath,
        name: &str,
        owner: OwnerLease,
    ) -> Result<ScopePath, ScopeError> {
        let root_path = ScopePath(vec![]);
        if parent != &root_path {
            return Err(ScopeError::InvalidInput(
                "nested scopes are MVP non-goals (R-26): only the root may have children".into(),
            ));
        }
        let Some(root) = self.scopes.get(&root_path) else {
            return Err(ScopeError::InvalidInput(
                "root scope is not present; a disposed root must be rebuilt with `new_root`".into(),
            ));
        };
        if name.is_empty() {
            return Err(ScopeError::InvalidInput(
                "scope name must not be empty".into(),
            ));
        }
        if root
            .children
            .iter()
            .any(|c| c.0.last().map(String::as_str) == Some(name))
        {
            return Err(ScopeError::DuplicateScope {
                parent: parent.clone(),
                name: name.to_string(),
            });
        }
        let path = ScopePath(vec![name.to_string()]);
        self.scopes
            .get_mut(&root_path)
            .expect("root is present (checked above)")
            .children
            .push(path.clone());
        self.scopes.insert(
            path.clone(),
            Scope {
                path: path.clone(),
                owner,
                parent: Some(root_path),
                children: vec![],
            },
        );
        Ok(path)
    }

    /// Disposes a scope: depth-first disposal of its children first, then
    /// removal of all its contributions via [`ContributionRegistry::remove_scope`],
    /// then removal of the scope records. The root's disposal thus disposes
    /// every child (single level at M2).
    pub fn dispose_scope(
        &mut self,
        path: &ScopePath,
        registry: &mut ContributionRegistry,
        force: bool,
    ) -> Result<(), ScopeError> {
        let Some(scope) = self.scopes.get(path).cloned() else {
            return Err(ScopeError::InvalidInput(format!("unknown scope `{path}`")));
        };
        for child in &scope.children {
            self.dispose_scope(child, registry, force)?;
        }
        registry.remove_scope(path, force)?;
        self.scopes.remove(path);
        if let Some(parent) = &scope.parent
            && let Some(parent_scope) = self.scopes.get_mut(parent)
        {
            parent_scope.children.retain(|c| c != path);
        }
        Ok(())
    }

    /// All scope records sorted by path.
    pub fn scopes(&self) -> Vec<&Scope> {
        let mut out: Vec<&Scope> = self.scopes.values().collect();
        out.sort_by_key(|a| a.path.to_string());
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contrib::{CommandContribution, Contribution, ContributionKind};
    use crate::epoch::CompositionStore;
    use kanbei_services::ServiceRegistry;
    use std::sync::{Arc, Mutex};

    fn root() -> ScopePath {
        ScopePath(vec![])
    }

    #[test]
    fn root_and_single_level_children() {
        let mut tree = ScopeTree::new_root();
        let scopes = tree.scopes();
        assert_eq!(scopes.len(), 1);
        assert_eq!(scopes[0].path, root());
        assert_eq!(scopes[0].owner, OwnerLease::Run(0));
        assert_eq!(scopes[0].parent, None);
        assert!(scopes[0].children.is_empty());

        let a = tree
            .create_child(&root(), "a", OwnerLease::Generation(7))
            .unwrap();
        let b = tree.create_child(&root(), "b", OwnerLease::Run(3)).unwrap();
        assert_eq!(a, ScopePath(vec!["a".into()]));
        assert_eq!(b, ScopePath(vec!["b".into()]));

        let scopes = tree.scopes();
        assert_eq!(scopes.len(), 3);
        // sorted by path: "/" < "/a" < "/b"
        assert_eq!(scopes[0].path, root());
        assert_eq!(scopes[1].path, a.clone());
        assert_eq!(scopes[2].path, b.clone());
        assert_eq!(
            tree.scopes.get(&a).unwrap().owner,
            OwnerLease::Generation(7)
        );
        assert_eq!(tree.scopes.get(&a).unwrap().parent, Some(root()));
        assert_eq!(
            tree.scopes.get(&root()).unwrap().children,
            vec![a.clone(), b.clone()]
        );
    }

    #[test]
    fn nested_child_rejected() {
        let mut tree = ScopeTree::new_root();
        let a = tree.create_child(&root(), "a", OwnerLease::Run(1)).unwrap();
        let err = tree
            .create_child(&a, "a-child", OwnerLease::Run(1))
            .unwrap_err();
        assert!(matches!(
            err,
            ScopeError::InvalidInput(msg) if msg.contains("nested scopes are MVP non-goals")
        ));
    }

    #[test]
    fn duplicate_child_name_rejected() {
        let mut tree = ScopeTree::new_root();
        tree.create_child(&root(), "a", OwnerLease::Run(1)).unwrap();
        let err = tree
            .create_child(&root(), "a", OwnerLease::Generation(2))
            .unwrap_err();
        assert_eq!(
            err,
            ScopeError::DuplicateScope {
                parent: root(),
                name: "a".into(),
            }
        );
    }

    #[test]
    fn empty_child_name_rejected() {
        let mut tree = ScopeTree::new_root();
        let err = tree
            .create_child(&root(), "", OwnerLease::Run(1))
            .unwrap_err();
        assert!(matches!(err, ScopeError::InvalidInput(_)));
    }

    #[test]
    fn dispose_root_recursively_disposes_children() {
        let mut tree = ScopeTree::new_root();
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let a = tree.create_child(&root(), "a", OwnerLease::Run(1)).unwrap();
        let b = tree.create_child(&root(), "b", OwnerLease::Run(2)).unwrap();
        let mut store = CompositionStore::new(&registry);
        for (path, name) in [(&a, "cmd-a"), (&b, "cmd-b"), (&root(), "cmd-root")] {
            let set = vec![Contribution {
                scope: path.clone(),
                kind: ContributionKind::Command(CommandContribution {
                    name: name.into(),
                    handler: "h".into(),
                }),
            }];
            store.stage_publish(&set, &mut registry).unwrap();
        }
        assert_eq!(registry.snapshot().len(), 3);

        // disposing the root disposes every child (single level at M2)
        tree.dispose_scope(&root(), &mut registry, false).unwrap();
        assert!(tree.scopes().is_empty());
        assert!(registry.snapshot().is_empty());
    }

    #[test]
    fn dispose_unknown_scope_rejected() {
        let mut tree = ScopeTree::new_root();
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let err = tree
            .dispose_scope(&ScopePath(vec!["ghost".into()]), &mut registry, false)
            .unwrap_err();
        assert!(matches!(err, ScopeError::InvalidInput(_)));
        // the root survives a failed dispose
        assert_eq!(tree.scopes().len(), 1);
    }
}
