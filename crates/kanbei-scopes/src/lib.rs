//! kanbei-scopes — the M2 transactional scopes (R-26/C-09, R-19/A-11/C,
//! R-01): a root scope plus ephemeral single-level child scopes, staged
//! contribution sets validated and atomically published against the current
//! epoch (optimistic concurrency control), the typed contribution registries
//! with fixed kernel-owned conflict rules, and the epoch composition digest.
//! Design inputs: docs/architecture.md "Unified module lifecycle" and
//! "Dynamic registration".
//!
//! # Result sizes
//! `ScopeError` wraps `kanbei_services::ServiceError` (whose `Conflict`
//! variant carries two `ServiceProvider`s), so scope results exceed the
//! `result_large_err` threshold; the error type is fixed by the public API,
//! mirroring the crate-level allowance in kanbei-services.
#![allow(clippy::result_large_err)]

pub mod contrib;
pub mod epoch;
pub mod errors;
pub mod registry;
pub mod scope_tree;
