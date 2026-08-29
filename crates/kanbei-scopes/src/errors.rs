//! Scope-level errors (kanbei-scopes). Wraps [`ServiceError`] for
//! service-DAG failures surfaced inside scope transactions.

use kanbei_services::{ScopePath, ServiceDependency, ServiceError};
use thiserror::Error;

/// Typed scope-transaction errors (R-19/A-11/C, R-26/C-09).
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ScopeError {
    /// A typed contribution conflicts with an existing registration or with
    /// an earlier entry of the same staged set.
    #[error(
        "`{kind}` contribution `{name}` in scope `{scope}` is held by {holder}; challenger {challenger}"
    )]
    Conflict {
        kind: &'static str,
        scope: ScopePath,
        name: String,
        holder: String,
        challenger: String,
    },
    /// A child scope name is already taken within its parent.
    #[error("duplicate scope name `{name}` under parent `{parent}`")]
    DuplicateScope { parent: ScopePath, name: String },
    /// A staged set built against an older epoch was published (OCC, R-26/C-09).
    #[error("stale staged set: built against epoch {staged}, current epoch is {current}")]
    StaleEpoch { staged: u64, current: u64 },
    /// Removing a scope would orphan dependents of its services.
    #[error("scope `{scope}` still has dependent services: {dependents:?}")]
    DependentsRemain {
        scope: ScopePath,
        dependents: Vec<ServiceDependency>,
    },
    /// A contribution violates a structural rule (e.g. theme overlay shape).
    #[error("invalid contribution in scope `{scope}`: {reason}")]
    InvalidContribution { scope: ScopePath, reason: String },
    /// Invalid input (unknown scope, nested scopes, ...).
    #[error("invalid input: {0}")]
    InvalidInput(String),
    /// A service-DAG failure inside a scope transaction (e.g. dependency cycle).
    #[error(transparent)]
    Service(#[from] ServiceError),
}
