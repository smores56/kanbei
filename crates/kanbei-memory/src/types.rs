//! The memory object model: claims, edges, root-manifest deltas, and the
//! transition record. Every type is `Serialize + Deserialize + Clone +
//! PartialEq`; canonical serialization is `serde_json::to_vec` (field order
//! as declared), so object digests are byte-stable. All claim/edge/root
//! objects are content-addressed and kernel-validated bootstrap meta-schema
//! structures (R-12/M-01).

use kanbei_capabilities::Principal;
use kanbei_core::{Digest, Id128};
use serde::{Deserialize, Serialize};

use crate::canonical_bytes;
use crate::error::MemoryError;

pub const MEMORY_CLAIM_SCHEMA: u32 = 1;
pub const MEMORY_EDGE_SCHEMA: u32 = 1;
pub const MEMORY_ROOT_SCHEMA: u32 = 1;
pub const MEMORY_TRANSITION_SCHEMA: u32 = 1;

/// Promoted claims carry a bounded evidence excerpt (R-12/M-11): cap the
/// excerpt at 4096 bytes; larger proposals are rejected at construction.
pub const PROMOTION_EXCERPT_MAX: usize = 4096;

/// A memory scope: the lifetime store, or one project's store.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum MemoryScope {
    Lifetime,
    Project(Id128),
}

impl MemoryScope {
    /// The canonical directory name under `<memory_root>/`: `"lifetime"` or
    /// `"projects/<base58 ProjectId>"`.
    pub fn dir_name(&self) -> String {
        match self {
            MemoryScope::Lifetime => "lifetime".into(),
            MemoryScope::Project(id) => format!("projects/{id}"),
        }
    }
}

/// How a branched session follows memory after `continue_from` (M6 wave 2):
/// either the live actor heads, or the checkpoint-pinned roots (the
/// projection then folds the pinned roots — the historical claim set at the
/// checkpoint frontier). Externally tagged serde: the payload records the
/// variant name.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum MemoryFollowPolicy {
    FollowHead,
    PinnedAt {
        lifetime_root: Digest,
        project_root: Option<Digest>,
    },
}

/// The provenance of one claim or edge: the originating session/event plus,
/// for promotions, the source claim digests and a bounded evidence excerpt.
/// The excerpt cap is enforced at construction ([`ClaimProvenance::new_promotion`]).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ClaimProvenance {
    pub session: Id128,
    pub event: u64,
    /// Source claim digests; empty for ordinary claims.
    pub source_claims: Vec<Digest>,
    /// Bounded excerpt of the source evidence; empty for ordinary claims.
    pub evidence_excerpt: String,
}

impl ClaimProvenance {
    /// Ordinary claim provenance: no source claims, no excerpt.
    pub fn new_ordinary(session: Id128, event: u64) -> Self {
        Self {
            session,
            event,
            source_claims: Vec::new(),
            evidence_excerpt: String::new(),
        }
    }

    /// Promotion provenance: source claim digests plus a bounded evidence
    /// excerpt. Rejects excerpts over [`PROMOTION_EXCERPT_MAX`] bytes.
    pub fn new_promotion(
        session: Id128,
        event: u64,
        source_claims: Vec<Digest>,
        evidence_excerpt: &str,
    ) -> Result<Self, MemoryError> {
        if evidence_excerpt.len() > PROMOTION_EXCERPT_MAX {
            return Err(MemoryError::InvalidInput(format!(
                "promotion evidence excerpt is {} bytes, max {PROMOTION_EXCERPT_MAX}",
                evidence_excerpt.len()
            )));
        }
        Ok(Self {
            session,
            event,
            source_claims,
            evidence_excerpt: evidence_excerpt.to_string(),
        })
    }
}

/// An immutable claim object (content-addressed). The claim embeds its
/// ClaimId (R-12/M-02); retrieval dedups by content digest over claim content
/// + kind, never provenance.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct Claim {
    pub schema: u32,
    /// `claim_`-branded id, embedded (R-12/M-02).
    pub claim_id: Id128,
    /// e.g. "decision" | "constraint" | "preference" | "lesson" | "procedure"
    /// | "correction" | "refinement" | "promotion".
    pub kind: String,
    pub content: String,
    pub owner: Principal,
    pub visibility_scope: MemoryScope,
    pub provenance: ClaimProvenance,
    /// Wall-clock, display/heuristic only — never ordering (R-12/M-08).
    pub observed_at: Option<u64>,
    pub valid_from: Option<u64>,
    /// Sensitivity class label.
    pub sensitivity: String,
}

impl Claim {
    /// The claim object digest over its canonical bytes.
    pub fn digest(&self) -> Digest {
        Digest::new(&canonical_bytes(self))
    }

    /// Canonical serialization — the content-addressed bytes.
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        canonical_bytes(self)
    }
}

/// The six-edge vocabulary (R-12/M-13). `Supersedes` with `to: None` is a
/// retraction (no separate retracts edge).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum EdgeKind {
    EvidenceFor,
    Supports,
    Contradicts,
    Supersedes,
    PromotedFrom,
    AppliesTo,
}

/// An immutable edge object (content-addressed; identity = its digest).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ClaimEdge {
    pub schema: u32,
    /// Source ClaimId (the claim this edge departs from).
    pub from: Id128,
    /// Target ClaimId; `None` allowed ONLY for [`EdgeKind::Supersedes`]
    /// (retraction).
    pub to: Option<Id128>,
    pub kind: EdgeKind,
    /// Typed entity keys for [`EdgeKind::AppliesTo`] (R-12/M-03); empty
    /// otherwise.
    pub entity_keys: Vec<String>,
    /// Origin session/event (source_claims unused here).
    pub provenance: ClaimProvenance,
}

impl ClaimEdge {
    /// Builds an edge with the canonical schema, enforcing the shape
    /// invariants: `to: None` only for `Supersedes`; `entity_keys` non-empty
    /// only for `AppliesTo` (and required there).
    pub fn new(
        from: Id128,
        to: Option<Id128>,
        kind: EdgeKind,
        entity_keys: Vec<String>,
        provenance: ClaimProvenance,
    ) -> Result<Self, MemoryError> {
        if to.is_none() && kind != EdgeKind::Supersedes {
            return Err(MemoryError::InvalidInput(format!(
                "edge {from} -> none: only Supersedes may omit the target (retraction)"
            )));
        }
        match kind {
            EdgeKind::AppliesTo if entity_keys.is_empty() => Err(MemoryError::InvalidInput(
                format!("edge {from}: AppliesTo requires at least one entity key"),
            )),
            EdgeKind::AppliesTo => Ok(Self {
                schema: MEMORY_EDGE_SCHEMA,
                from,
                to,
                kind,
                entity_keys,
                provenance,
            }),
            _ if !entity_keys.is_empty() => Err(MemoryError::InvalidInput(format!(
                "edge {from}: entity_keys are only valid on AppliesTo edges"
            ))),
            _ => Ok(Self {
                schema: MEMORY_EDGE_SCHEMA,
                from,
                to,
                kind,
                entity_keys,
                provenance,
            }),
        }
    }

    /// The edge object digest over its canonical bytes.
    pub fn digest(&self) -> Digest {
        Digest::new(&canonical_bytes(self))
    }

    /// Canonical serialization — the content-addressed bytes.
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        canonical_bytes(self)
    }
}

/// A root-manifest delta object (R-12/M-09): the current claim set is the
/// projection-time fold over the parent chain.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct RootManifest {
    pub schema: u32,
    /// Previous root manifest digest; `None` = genesis.
    pub parent: Option<Digest>,
    pub scope: MemoryScope,
    /// Claim object digests (closure refs, R-12/M-01).
    pub added_claims: Vec<Digest>,
    pub added_edges: Vec<Digest>,
    /// Claim digests removed in this delta (supersession/retraction).
    pub retracted: Vec<Digest>,
    /// `tr_`-branded id.
    pub transition_id: Id128,
}

impl RootManifest {
    /// The manifest object digest over its canonical bytes.
    pub fn digest(&self) -> Digest {
        Digest::new(&canonical_bytes(self))
    }

    /// Canonical serialization — the content-addressed bytes.
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        canonical_bytes(self)
    }
}

/// R-11 idempotency key: the originating session/event plus the broker-issued
/// decision digest. The CAS actor rejects a second transition with the same
/// key.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct IdempotencyKey {
    pub session: Id128,
    pub event: u64,
    pub decision: Digest,
}

impl std::fmt::Display for IdempotencyKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "session {} event {} decision {}",
            self.session, self.event, self.decision
        )
    }
}

/// The two transition kinds; serde text forms are lowercase.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TransitionKind {
    RootApproval,
    Promotion,
}

/// One transition-log record: the CAS proposal for a root-selection commit.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct MemoryTransition {
    pub schema: u32,
    /// `tr_`-branded id.
    pub transition_id: Id128,
    pub scope: MemoryScope,
    pub kind: TransitionKind,
    /// CAS expected value; `None` = genesis.
    pub expected_old_root: Option<Digest>,
    /// The RootManifest digest this transition accepts.
    pub accepted_new_root: Digest,
    pub origin_session: Id128,
    pub origin_event: u64,
    /// Typed root-approval event kind: "memory_root_approved" (RootApproval)
    /// or "memory_promotion_approved" (Promotion).
    pub origin_kind: String,
    pub decision_principal: Principal,
    /// Broker-issued approval digest (non-zero required).
    pub decision_digest: Digest,
    pub idempotency_key: IdempotencyKey,
}

/// Derivable claim lifecycle status (R-12/M-07) — derived from canonical data
/// only, never stored.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub enum ValidationStatus {
    Proposed,
    Approved,
    Active,
    Superseded,
    Retracted,
}

/// Derives a claim's lifecycle status from the current fold and the edge
/// set. Supersedes edges win (they are the only status mutation); the
/// `Approved` branch is defensive — it catches claims a committed edge still
/// references that are absent from the fold without a supersedes edge
/// (committed-but-unfolded data would otherwise read as Proposed).
pub fn derive_validation_status(
    claim_id: Id128,
    fold: &[Id128],
    edges: &[ClaimEdge],
) -> ValidationStatus {
    if let Some(edge) = edges
        .iter()
        .find(|e| e.from == claim_id && e.kind == EdgeKind::Supersedes)
    {
        return if edge.to.is_some() {
            ValidationStatus::Superseded
        } else {
            ValidationStatus::Retracted
        };
    }
    if fold.contains(&claim_id) {
        return ValidationStatus::Active;
    }
    if edges
        .iter()
        .any(|e| e.to == Some(claim_id) || e.from == claim_id)
    {
        return ValidationStatus::Approved;
    }
    ValidationStatus::Proposed
}

/// The projection-time fold (R-12/M-09): the active claim set, active edges,
/// superseded/retracted claims (for contradiction annotation), and the full
/// manifest chain.
#[derive(Clone, Debug, PartialEq)]
pub struct RootFold {
    pub root: Option<Digest>,
    /// Active claims (digest, decoded object).
    pub claims: Vec<(Digest, Claim)>,
    /// Active edges (digest, decoded object).
    pub edges: Vec<(Digest, ClaimEdge)>,
    /// Claims removed by supersession/retraction (never in `claims`).
    pub retracted: Vec<(Digest, Claim)>,
    /// All manifest digests on the chain, genesis-first.
    pub history: Vec<Digest>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanbei_capabilities::Principal;

    fn principal() -> Principal {
        Principal {
            session: Id128::generate(),
            generation: 1,
            run: None,
        }
    }

    fn claim(claim_id: Id128) -> Claim {
        Claim {
            schema: MEMORY_CLAIM_SCHEMA,
            claim_id,
            kind: "decision".into(),
            content: "the claim text".into(),
            owner: principal(),
            visibility_scope: MemoryScope::Lifetime,
            provenance: ClaimProvenance::new_ordinary(Id128::generate(), 1),
            observed_at: Some(1_700_000_000),
            valid_from: None,
            sensitivity: "public".into(),
        }
    }

    #[test]
    fn claim_content_addressing_is_deterministic() {
        let a = claim(Id128::generate());
        let b = a.clone();
        assert_eq!(a.digest(), b.digest());
        assert_eq!(a.to_canonical_bytes(), b.to_canonical_bytes());

        let mut bytes = a.to_canonical_bytes();
        let n = bytes.len();
        bytes[n / 2] ^= 0x01;
        assert_ne!(Digest::new(&bytes), a.digest());
    }

    #[test]
    fn promotion_excerpt_cap_rejects_oversize() {
        let session = Id128::generate();
        let source = Digest::new(b"source claim");
        let err = ClaimProvenance::new_promotion(
            session,
            1,
            vec![source],
            &"x".repeat(PROMOTION_EXCERPT_MAX + 1),
        )
        .unwrap_err();
        assert!(matches!(err, MemoryError::InvalidInput(_)));

        // At the cap it is accepted; a promotion claim embeds it.
        let prov = ClaimProvenance::new_promotion(
            session,
            1,
            vec![source],
            &"x".repeat(PROMOTION_EXCERPT_MAX),
        )
        .unwrap();
        assert_eq!(prov.evidence_excerpt.len(), PROMOTION_EXCERPT_MAX);
    }

    #[test]
    fn edge_vocabulary_roundtrips_serde() {
        let from = Id128::generate();
        let to = Id128::generate();
        let prov = ClaimProvenance::new_ordinary(Id128::generate(), 1);
        let cases: Vec<(EdgeKind, Option<Id128>, Vec<String>)> = vec![
            (EdgeKind::EvidenceFor, Some(to), vec![]),
            (EdgeKind::Supports, Some(to), vec![]),
            (EdgeKind::Contradicts, Some(to), vec![]),
            (EdgeKind::Supersedes, Some(to), vec![]),
            (EdgeKind::PromotedFrom, Some(to), vec![]),
            (
                EdgeKind::AppliesTo,
                Some(to),
                vec!["file:src/lib.rs".into()],
            ),
        ];
        for (kind, to, keys) in cases {
            let edge = ClaimEdge::new(from, to, kind.clone(), keys, prov.clone()).unwrap();
            let json = serde_json::to_string(&edge).unwrap();
            let kind_json = serde_json::to_string(&kind).unwrap();
            assert!(json.contains(&format!("\"kind\":{kind_json}")));
            let back: ClaimEdge = serde_json::from_str(&json).unwrap();
            assert_eq!(back, edge);
        }
    }

    #[test]
    fn supersedes_without_successor_is_retraction() {
        let from = Id128::generate();
        let prov = ClaimProvenance::new_ordinary(Id128::generate(), 1);
        let retraction =
            ClaimEdge::new(from, None, EdgeKind::Supersedes, vec![], prov.clone()).unwrap();
        assert_eq!(retraction.to, None);
        assert_eq!(retraction.kind, EdgeKind::Supersedes);
        assert!(!retraction.to_canonical_bytes().is_empty());

        // Only Supersedes may omit the target.
        let err = ClaimEdge::new(from, None, EdgeKind::Supports, vec![], prov).unwrap_err();
        assert!(matches!(err, MemoryError::InvalidInput(_)));
    }

    #[test]
    fn derive_validation_status_cases() {
        let active = Id128::generate();
        let superseded = Id128::generate();
        let retracted = Id128::generate();
        let unreferenced = Id128::generate();
        let prov = ClaimProvenance::new_ordinary(Id128::generate(), 1);
        let edges = vec![
            ClaimEdge::new(
                superseded,
                Some(Id128::generate()),
                EdgeKind::Supersedes,
                vec![],
                prov.clone(),
            )
            .unwrap(),
            ClaimEdge::new(retracted, None, EdgeKind::Supersedes, vec![], prov.clone()).unwrap(),
        ];
        let fold = vec![active];

        assert_eq!(
            derive_validation_status(active, &fold, &edges),
            ValidationStatus::Active
        );
        assert_eq!(
            derive_validation_status(superseded, &fold, &edges),
            ValidationStatus::Superseded
        );
        assert_eq!(
            derive_validation_status(retracted, &fold, &edges),
            ValidationStatus::Retracted
        );
        assert_eq!(
            derive_validation_status(unreferenced, &fold, &edges),
            ValidationStatus::Proposed
        );
    }
}
