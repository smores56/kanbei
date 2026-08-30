//! Test fixtures shared by the module test suites: claim/edge/fold builders
//! and unique temp DB paths.

use std::path::{Path, PathBuf};

use kanbei_capabilities::Principal;
use kanbei_core::{Digest, Id128};
use kanbei_memory::{
    Claim, ClaimEdge, ClaimProvenance, EdgeKind, MEMORY_CLAIM_SCHEMA, MemoryScope, RootFold,
};

use crate::index::ScopeIndexInput;

/// A fresh principal.
pub(crate) fn principal() -> Principal {
    Principal {
        session: Id128::generate(),
        generation: 1,
        run: None,
    }
}

/// A fresh session id.
pub(crate) fn session() -> Id128 {
    Id128::generate()
}

/// An ordinary claim; returns (digest, claim).
pub(crate) fn make_claim(
    claim_id: Id128,
    kind: &str,
    content: &str,
    session: Id128,
    event: u64,
) -> (Digest, Claim) {
    let claim = Claim {
        schema: MEMORY_CLAIM_SCHEMA,
        claim_id,
        kind: kind.to_string(),
        content: content.to_string(),
        owner: principal(),
        visibility_scope: MemoryScope::Lifetime,
        provenance: ClaimProvenance::new_ordinary(session, event),
        observed_at: None,
        valid_from: None,
        sensitivity: "public".to_string(),
    };
    (claim.digest(), claim)
}

/// A promotion claim: provenance carries a source claim digest.
pub(crate) fn make_promoted_claim(
    claim_id: Id128,
    kind: &str,
    content: &str,
    session: Id128,
    event: u64,
    source: Digest,
) -> (Digest, Claim) {
    let mut claim = make_claim(claim_id, kind, content, session, event).1;
    claim.provenance = ClaimProvenance::new_promotion(session, event, vec![source], "").unwrap();
    (claim.digest(), claim)
}

/// A canonical edge; returns (digest, edge).
pub(crate) fn make_edge(from: Id128, to: Option<Id128>, kind: EdgeKind) -> (Digest, ClaimEdge) {
    let edge = ClaimEdge::new(
        from,
        to,
        kind,
        Vec::new(),
        ClaimProvenance::new_ordinary(Id128::generate(), 1),
    )
    .unwrap();
    (edge.digest(), edge)
}

/// A projection-time fold.
pub(crate) fn make_fold(
    claims: Vec<(Digest, Claim)>,
    edges: Vec<(Digest, ClaimEdge)>,
    retracted: Vec<(Digest, Claim)>,
) -> RootFold {
    RootFold {
        root: claims.first().map(|(d, _)| *d),
        claims,
        edges,
        retracted,
        history: Vec::new(),
    }
}

/// A scope input built from a fold (root = the fold's root).
pub(crate) fn scope_input(scope: MemoryScope, fold: RootFold) -> ScopeIndexInput {
    ScopeIndexInput {
        scope,
        root: fold.root,
        fold,
    }
}

/// A unique temp DB path per (name, process, time).
pub(crate) fn temp_db(name: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock before unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "kanbei-retrieval-{}-{name}-{nanos}.sqlite",
        std::process::id()
    ))
}

/// Best-effort removal of the DB and its WAL sidecars.
pub(crate) fn remove_db(path: &Path) {
    for suffix in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", path.display()));
    }
}
