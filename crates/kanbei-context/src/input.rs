//! Projection input: the typed views the kernel materializes for one
//! projection (TrajectoryView → ValidProviderContext, architecture.md:130).

use kanbei_core::Digest;
use kanbei_memory::ValidationStatus;
use serde::{Deserialize, Serialize};

/// The trajectory view (architecture.md:131): the frozen committed prefix
/// (A-06), the selected stable ranges, and the bounded recent events.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TrajectoryView {
    /// Last committed event seq at projection time — the frozen prefix.
    pub frozen_seq: u64,
    /// Conversation ranges eligible for the stable prefix.
    pub selected_ranges: Vec<(u64, u64)>,
    /// Explicitly selected event seqs.
    pub selected_events: Vec<u64>,
    /// Bounded recent events — the rendered source for trajectory fragments.
    pub events: Vec<RenderedEvent>,
}

/// One rendered trajectory event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenderedEvent {
    pub seq: u64,
    pub kind: String,
    pub text: String,
    pub sensitivity: String,
}

/// Cognitive selection view (architecture.md:133): the salience-scored
/// working set.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ActiveMemoryView {
    /// Scoring module + version, e.g. "salience-v1".
    pub scorer: String,
    pub pins: Vec<Digest>,
    pub open_loops: Vec<OpenLoop>,
    /// Event seqs of recent causal edges.
    pub recent_causal: Vec<u64>,
}

/// One open loop: a promise to the user that is not yet resolved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenLoop {
    pub id: String,
    pub text: String,
    pub created_event: u64,
    pub sensitivity: String,
}

/// Retrieved evidence view (architecture.md:134).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct RetrievedEvidence {
    pub claims: Vec<EvidenceClaim>,
}

/// One retrieved claim with its contradiction annotation (R-12/M-04).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceClaim {
    pub digest: Digest,
    pub text: String,
    pub kind: String,
    pub sensitivity: String,
    pub status: ValidationStatus,
    pub score: f64,
    pub contradictions: Vec<Contradiction>,
    pub source_events: Vec<u64>,
}

/// A claim the retrieved claim contradicts (R-12/M-04 annotation).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Contradiction {
    pub digest: Digest,
    pub text: String,
    /// True when the contradiction supersedes the retrieved claim.
    pub supersedes: bool,
}

/// A memory scope's fold excerpt (lifetime or project): the root digest,
/// its rendered text, and the claim digests embedded in it. The claim
/// digests become the fragment's source refs so the authority filter can
/// check memory fragments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryFragmentSource {
    pub root: Digest,
    pub text: String,
    pub sensitivity: String,
    pub claim_digests: Vec<Digest>,
}

/// A frozen compaction summary (R-18/E-06): the covered event range and its
/// rendered text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionSummarySource {
    pub range: (u64, u64),
    pub text: String,
    pub sensitivity: String,
}

/// The current trigger — the reason this projection exists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerFragment {
    pub kind: String,
    pub text: String,
    pub sensitivity: String,
}

/// A deterministic, canonically-ordered tool/module schema
/// (architecture.md:146).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SchemaFragment {
    pub id: String,
    pub digest: Digest,
    pub text: String,
    pub sensitivity: String,
}

/// Token budgets the projection must fit (architecture.md:139).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetSpec {
    pub max_total_tokens: u64,
    pub max_volatile_tokens: u64,
}

/// The full projection input: everything a later wave's session materializes
/// before running the pipeline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectionInput {
    /// Harness contract; sensitivity `public`.
    pub harness_contract: String,
    pub schemas: Vec<SchemaFragment>,
    pub lifetime: Option<MemoryFragmentSource>,
    pub project: Option<MemoryFragmentSource>,
    pub compaction: Option<CompactionSummarySource>,
    pub trajectory: TrajectoryView,
    pub active: ActiveMemoryView,
    pub evidence: RetrievedEvidence,
    pub trigger: TriggerFragment,
    pub budgets: BudgetSpec,
}

impl ProjectionInput {
    /// Convenience constructor with sane defaults: empty views, no memory
    /// sources, budgets {8192, 4096}, `frozen_seq` 0. Fields are pub —
    /// callers fill in the views.
    pub fn new(harness_contract: impl Into<String>) -> Self {
        Self {
            harness_contract: harness_contract.into(),
            schemas: Vec::new(),
            lifetime: None,
            project: None,
            compaction: None,
            trajectory: TrajectoryView::default(),
            active: ActiveMemoryView::default(),
            evidence: RetrievedEvidence::default(),
            trigger: TriggerFragment {
                kind: String::new(),
                text: String::new(),
                sensitivity: "public".into(),
            },
            budgets: BudgetSpec {
                max_total_tokens: 8192,
                max_volatile_tokens: 4096,
            },
        }
    }
}
