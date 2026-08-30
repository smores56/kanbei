//! Projection fragments: stability classes, kinds, source references, and
//! the E-14-enforcing builder (architecture.md:144, 149).

use kanbei_core::Digest;
use serde::{Deserialize, Serialize};

use crate::error::{ProjectionError, sensitivity_rank};

/// Fragment stability class (architecture.md:144): how long the content may
/// be reused. Lowering caches only non-volatile, cache-eligible fragments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum StabilityClass {
    /// Harness contract and deterministic module/tool schemas.
    Static,
    /// Project/lifetime memory: stable within the scope.
    ScopeStable,
    /// Conversation prefix/compaction: stable within the session.
    SessionStable,
    /// Active memory/recent events/trigger: stable only within the turn.
    TurnVolatile,
}

/// What a fragment carries (architecture.md:143 order and kinds).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FragmentKind {
    HarnessContract,
    ModuleSchema,
    LifetimeMemory,
    ProjectMemory,
    ConversationPrefix,
    CompactionSummary,
    ActiveMemory,
    RetrievedEvidence,
    RecentEvents,
    CurrentTrigger,
}

/// Where a fragment's content comes from. The kernel's read capability
/// (R-05/E-03) decides each ref; the authority filter and validator drop or
/// reject fragments with any unauthorized ref.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SourceRef {
    /// The harness contract (always kernel-owned).
    Harness,
    /// A module/tool schema by id.
    ModuleSchema(String),
    /// A canonical conversation event by seq.
    SessionEvent(u64),
    /// A durable memory claim by digest.
    MemoryClaim(Digest),
    /// A frozen conversation range, by inclusive (start, end) seqs.
    CompactionRange(u64, u64),
}

/// One projection fragment: content plus the metadata that makes it
/// cacheable, auditable, and validator-checkable (architecture.md:144).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fragment {
    pub id: String,
    /// Semantic ordering position; fragments are sorted by (order, id).
    pub order: u32,
    pub kind: FragmentKind,
    pub stability: StabilityClass,
    pub content: String,
    /// `Digest::new(content bytes)` — the cache/change fingerprint.
    pub content_hash: Digest,
    /// Dependency hashes (memory roots, schema digests, event-range hash).
    pub dep_hashes: Vec<Digest>,
    /// Output sensitivity class label (E-14).
    pub sensitivity: String,
    /// Max sensitivity of the inputs this fragment derived from (E-14);
    /// `None` when the fragment is kernel-owned with no derived inputs.
    pub derived_max: Option<String>,
    /// Inclusive event range this fragment carries, when any.
    pub event_range: Option<(u64, u64)>,
    /// Eligible for the provider's stable prefix (architecture.md:145).
    pub cache_eligible: bool,
    pub source_refs: Vec<SourceRef>,
}

/// The E-14 enforcement point (architecture.md:149): a fragment whose output
/// sensitivity ranks below its derived max cannot be built. Id and content
/// must be non-empty. Fields default to empty/`None`/`false`; the default
/// sensitivity is `internal`.
pub struct FragmentBuilder {
    id: String,
    order: u32,
    kind: FragmentKind,
    stability: StabilityClass,
    content: String,
    sensitivity: String,
    derived_max: Option<String>,
    event_range: Option<(u64, u64)>,
    cache_eligible: bool,
    source_refs: Vec<SourceRef>,
    dep_hashes: Vec<Digest>,
}

impl FragmentBuilder {
    /// Start a fragment: id, semantic order position, kind, stability class.
    pub fn new(
        id: impl Into<String>,
        order: u32,
        kind: FragmentKind,
        stability: StabilityClass,
    ) -> Self {
        Self {
            id: id.into(),
            order,
            kind,
            stability,
            content: String::new(),
            sensitivity: "internal".into(),
            derived_max: None,
            event_range: None,
            cache_eligible: false,
            source_refs: Vec::new(),
            dep_hashes: Vec::new(),
        }
    }

    /// The fragment payload (non-empty; hashed into [`Fragment::content_hash`]).
    pub fn content(mut self, content: impl Into<String>) -> Self {
        self.content = content.into();
        self
    }

    /// Output sensitivity label; must rank >= `derived_max` at build.
    pub fn sensitivity(mut self, sensitivity: impl Into<String>) -> Self {
        self.sensitivity = sensitivity.into();
        self
    }

    /// Max input sensitivity the fragment derives from (the caller supplies
    /// it; the builder and validator both enforce the E-14 ordering).
    pub fn derived_max(mut self, derived_max: impl Into<String>) -> Self {
        self.derived_max = Some(derived_max.into());
        self
    }

    /// Inclusive event range this fragment carries (chronology A-06).
    pub fn event_range(mut self, range: Option<(u64, u64)>) -> Self {
        self.event_range = range;
        self
    }

    /// Whether the fragment may join the provider's stable prefix.
    pub fn cache_eligible(mut self, eligible: bool) -> Self {
        self.cache_eligible = eligible;
        self
    }

    /// Source references the authority filter checks (R-05/E-03).
    pub fn source_refs(mut self, refs: Vec<SourceRef>) -> Self {
        self.source_refs = refs;
        self
    }

    /// Dependency hashes (memory roots, schema digests, event-range hash).
    pub fn dep_hashes(mut self, hashes: Vec<Digest>) -> Self {
        self.dep_hashes = hashes;
        self
    }

    pub fn build(self) -> Result<Fragment, ProjectionError> {
        if self.id.is_empty() {
            return Err(ProjectionError::InvalidInput("empty fragment id".into()));
        }
        if self.content.is_empty() {
            return Err(ProjectionError::InvalidInput(format!(
                "fragment {}: empty content",
                self.id
            )));
        }
        if let Some(derived_max) = &self.derived_max
            && sensitivity_rank(&self.sensitivity) < sensitivity_rank(derived_max)
        {
            return Err(ProjectionError::SensitivityViolation(
                self.id,
                self.sensitivity,
                derived_max.clone(),
            ));
        }
        Ok(Fragment {
            id: self.id,
            order: self.order,
            kind: self.kind,
            stability: self.stability,
            content_hash: Digest::new(self.content.as_bytes()),
            content: self.content,
            dep_hashes: self.dep_hashes,
            sensitivity: self.sensitivity,
            derived_max: self.derived_max,
            event_range: self.event_range,
            cache_eligible: self.cache_eligible,
            source_refs: self.source_refs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitivity_rank_orders_classes_and_defaults_unknown() {
        assert_eq!(sensitivity_rank("public"), 0);
        assert_eq!(sensitivity_rank("internal"), 1);
        assert_eq!(sensitivity_rank("secret"), 2);
        assert_eq!(sensitivity_rank("critical"), 3);
        // unknown labels default to internal for ordering
        assert_eq!(sensitivity_rank("opaque"), 1);
    }

    #[test]
    fn builder_hashes_content() {
        let f = FragmentBuilder::new(
            "t",
            0,
            FragmentKind::HarnessContract,
            StabilityClass::Static,
        )
        .content("hello")
        .build()
        .unwrap();
        assert_eq!(f.content_hash, Digest::new(b"hello"));
    }

    #[test]
    fn builder_rejects_sensitivity_below_derived_max() {
        let err = FragmentBuilder::new(
            "t",
            0,
            FragmentKind::HarnessContract,
            StabilityClass::Static,
        )
        .content("x")
        .sensitivity("public")
        .derived_max("secret")
        .build()
        .unwrap_err();
        assert!(matches!(err, ProjectionError::SensitivityViolation(..)));
    }

    #[test]
    fn builder_accepts_equal_sensitivity() {
        let f = FragmentBuilder::new(
            "t",
            0,
            FragmentKind::HarnessContract,
            StabilityClass::Static,
        )
        .content("x")
        .sensitivity("secret")
        .derived_max("secret")
        .build()
        .unwrap();
        assert_eq!(f.sensitivity, "secret");
        assert_eq!(f.derived_max.as_deref(), Some("secret"));
    }

    #[test]
    fn builder_rejects_empty_id_and_content() {
        let empty_id =
            FragmentBuilder::new("", 0, FragmentKind::HarnessContract, StabilityClass::Static)
                .content("x")
                .build();
        assert!(matches!(empty_id, Err(ProjectionError::InvalidInput(_))));
        let empty_content = FragmentBuilder::new(
            "t",
            0,
            FragmentKind::HarnessContract,
            StabilityClass::Static,
        )
        .content("")
        .build();
        assert!(matches!(
            empty_content,
            Err(ProjectionError::InvalidInput(_))
        ));
    }
}
