//! Projection errors and the sensitivity class ordering (R-05/E-14).

use thiserror::Error;

/// Sensitivity class rank used by the E-14 non-escalation checks
/// (architecture.md:149): `public` = 0, `internal` = 1, `secret` = 2,
/// `critical` = 3. Unknown labels default to `internal` (rank 1) so an
/// unclassified label never silently outranks a classified one.
pub fn sensitivity_rank(s: &str) -> u32 {
    match s {
        "public" => 0,
        "internal" => 1,
        "secret" => 2,
        "critical" => 3,
        _ => 1,
    }
}

/// Pipeline failure: every variant carries enough context to debug without a
/// stack trace (fragment id, offending values, budgets).
#[derive(Debug, Error)]
pub enum ProjectionError {
    #[error("fragment {0}: authority denied")]
    AuthorityDenied(String),
    #[error("fragment {0}: sensitivity {1} below derived max {2}")]
    SensitivityViolation(String, String, String),
    #[error("fragment {0}: chronology violation — event {1} beyond frozen prefix {2}")]
    ChronologyViolation(String, u64, u64),
    #[error("fragment {0}: opaque artifact payload rejected")]
    OpaqueArtifact(String),
    #[error("projection over budget: needed {needed} tokens, budget {budget}")]
    OverBudget { needed: u64, budget: u64 },
    #[error("invalid projection input: {0}")]
    InvalidInput(String),
}
