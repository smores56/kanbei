//! kanbei-context — the typed cache-aware context-projection pipeline
//! (M4 Wave 2; docs/architecture.md "History and context projection").
//!
//! One model call projects the run state through a staged pipeline
//! (architecture.md:130): [`ProjectionInput`] (trajectory view, cognitive
//! state, retrieved evidence, memory sources, budgets) → the kernel-owned
//! authority filter, the replaceable stages, and the kernel validator →
//! [`ValidProviderContext`] → [`lower`] → provider messages plus a cache
//! plan (longest legal stable prefix, architecture.md:145).
//!
//! Kernel invariants enforced here: source authority (R-05/E-03), sensitivity
//! non-escalation (R-05/E-14), chronology (R-05/A-06), opaque-artifact ban
//! (R-18/E-07), suppression ban (R-05/E-05), and the fragment-list digest for
//! intent provenance (R-08/E-13). [`ReasoningContinuity`] and
//! [`CompactionSelection`] are declared here and wired by later waves
//! (R-18/E-07, R-18/E-06).

pub mod error;
pub mod fragment;
pub mod input;
pub mod lower;
pub mod pipeline;
pub mod validator;

pub use error::{ProjectionError, sensitivity_rank};
pub use fragment::{Fragment, FragmentBuilder, FragmentKind, SourceRef, StabilityClass};
pub use input::{
    ActiveMemoryView, BudgetSpec, CompactionSummarySource, Contradiction, EvidenceClaim,
    MemoryFragmentSource, OpenLoop, ProjectionInput, RenderedEvent, RetrievedEvidence,
    SchemaFragment, TrajectoryView, TriggerFragment,
};
pub use lower::{Lowering, lower};
pub use pipeline::{
    AuthorityFilter, BudgetStage, CognitiveStage, CompressionStage, DropRecord, EvidenceStage,
    MemoryStage, Projection, ProjectionStage, TrajectoryStage, default_stages, estimate_tokens,
    run_pipeline,
};
pub use validator::{ValidProviderContext, ValidatorStage};

use kanbei_core::Digest;
use serde::{Deserialize, Serialize};

/// Reasoning continuity across provider changes (R-18/E-07): outcome events
/// carry `Broken` on the first call after a provider switch, `Continuous`
/// otherwise. Declared here as the canonical type; wired by the session wave.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReasoningContinuity {
    /// The same provider and its artifacts continued across the call.
    Continuous,
    /// The provider changed and its reasoning artifacts were not transferable.
    Broken {
        from_provider: String,
        at_event: u64,
        /// The model's own flag when it reported its reasoning does not
        /// follow from the projection (R-18/E-07); None on provider-change
        /// breaks. Old records without the field deserialize as None.
        #[serde(default)]
        reason: Option<String>,
    },
}

/// A compaction selection (R-18/E-06): the causal-closed event range covered,
/// the summary object digest, and the fragment ids folded into it. The
/// selection itself becomes a canonical event in a later wave.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionSelection {
    pub range: (u64, u64),
    pub summary_digest: Digest,
    pub covered_fragments: Vec<String>,
}
