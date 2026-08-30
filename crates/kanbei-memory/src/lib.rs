//! kanbei-memory — the immutable durable claim/provenance DAG substrate
//! (M4 Wave 1, R-11/R-12).
//!
//! Memory has three layers (docs/architecture.md "Memory"): the immutable
//! experience DAG (sessions), the disposable per-run projection (SQLite), and
//! this crate: the immutable durable claim/provenance DAG. Claims and edges
//! are content-addressed objects; root manifests are deltas with an explicit
//! parent edge; the current claim set is the projection-time fold
//! (R-12/M-09). Each scope (lifetime, or one project) has a narrow canonical
//! `transitions.jsonl.zst` stream and one writer/CAS actor
//! ([`MemoryRootActor`]) that commits root-selection transitions
//! (R-11). `head.json` is an atomic convenience pointer repaired from the
//! scope log.
//!
//! Storage layout (canonical, XDG state):
//!
//! ```text
//! <memory_root>/
//! ├── projects.jsonl                  (ProjectRegistry, append-only JSONL)
//! ├── lifetime/
//! │   ├── transitions.jsonl.zst       (AppendLog, stream "memory-transitions")
//! │   ├── head.json                   (atomic convenience pointer)
//! │   └── objects/<alg>:<digest>      (ObjectStore)
//! └── projects/<ProjectId-text>/
//!     ├── transitions.jsonl.zst
//!     ├── head.json
//!     └── objects/<alg>:<digest>
//! ```

pub mod actor;
pub mod error;
pub mod registry;
pub mod types;

pub use actor::{MemoryFaultInjector, MemoryFaultPoint, MemoryRootActor, TransitionOutcome};
pub use error::MemoryError;
pub use registry::{PROJECT_ENTRY_SCHEMA, ProjectEntry, ProjectRegistry};
pub use types::{
    Claim, ClaimEdge, ClaimProvenance, EdgeKind, IdempotencyKey, MEMORY_CLAIM_SCHEMA,
    MEMORY_EDGE_SCHEMA, MEMORY_ROOT_SCHEMA, MEMORY_TRANSITION_SCHEMA, MemoryScope,
    MemoryTransition, PROMOTION_EXCERPT_MAX, RootFold, RootManifest, TransitionKind,
    ValidationStatus, derive_validation_status,
};

/// The canonical AppendLog stream name for every scope's transition log.
use kanbei_core::{Digest, Id128};

pub const TRANSITIONS_STREAM: &str = "memory-transitions";

/// The all-zero digest: the "no decision" sentinel for
/// [`MemoryTransition::decision_digest`]. `Digest::new(b"")` hashes empty
/// input and is NOT the zero digest.
pub(crate) fn zero_digest() -> Digest {
    let zero_hex = "0".repeat(64);
    format!("blake3:{zero_hex}")
        .parse()
        .expect("the zero digest text form is canonical")
}

/// The all-zero [`Id128`]: the "missing session" sentinel. Its base58 text
/// form is sixteen `1` characters (each encodes a zero byte).
pub(crate) fn zero_id() -> Id128 {
    "1111111111111111"
        .parse()
        .expect("sixteen ones decode to the zero id")
}

/// Canonical serialization: `serde_json::to_vec`, byte-stable.
pub(crate) fn canonical_bytes<T: serde::Serialize>(value: &T) -> Vec<u8> {
    serde_json::to_vec(value).expect("canonical serialization cannot fail")
}
