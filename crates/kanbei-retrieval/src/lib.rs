//! kanbei-retrieval — deterministic active-memory salience and SQLite
//! exact-entity/FTS5/BM25 retrieval over the memory DAG (M4 Wave 3;
//! docs/architecture.md "Memory").
//!
//! The crate takes data, never actors: the caller resolves the allowed
//! scopes, folds each scope's pinned root ([`kanbei_memory::MemoryRootActor::fold`]),
//! and hands the folds to [`MemoryIndex::build`] / [`MemoryIndex::reconcile`].
//! Retrieval runs the 9-step pipeline (architecture.md:509-519) via
//! [`MemoryIndex::search`]; active-memory salience is the deterministic,
//! versioned [`ActiveMemoryProjector`], which writes disposable
//! activation-log rows (R-12/F-S5) — never session-stream events.

pub mod entities;
pub mod error;
pub mod index;
pub mod salience;
pub mod search;

pub use entities::{EntityKind, extract_entities, extract_entity_keys, normalize_query};
pub use error::RetrievalError;
pub use index::{BuildReport, MemoryIndex, ScopeIndexInput};
pub use salience::{
    ActiveMemoryProjector, DEFAULT_TOP_N, SALIENCE_VERSION, SalienceBreakdown, SalienceInput,
    SalienceWeights, ScoredClaim,
};
pub use search::{SearchQuery, SearchResult};

#[cfg(test)]
pub(crate) mod testutil;
