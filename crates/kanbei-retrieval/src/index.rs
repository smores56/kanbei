//! The disposable SQLite retrieval index (architecture.md "Memory",
//! R-12/M-03): claims, edges, derived entity keys, root rows, retracted
//! claims (for contradiction annotation), and the FTS5 external-content
//! table. [`MemoryIndex::build`] is a clean full rebuild in one transaction;
//! [`MemoryIndex::reconcile`] is an incremental refresh. Activation logs are
//! disposable rows written by salience projection (R-12/F-S5) — never
//! session-stream events.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use kanbei_core::Digest;
use kanbei_memory::{
    Claim, ClaimEdge, EdgeKind, MemoryScope, RootFold, ValidationStatus, derive_validation_status,
};
use rusqlite::{Connection, params};

use crate::entities::{EntityKind, extract_entities};
use crate::error::RetrievalError;

/// Schema version of the tables this crate creates.
pub const RETRIEVAL_SCHEMA: u32 = 1;

/// Meta key for the scoring module used by the last build.
pub const META_SCORER: &str = "scorer";
/// Meta key for the build marker: the newest provenance event seq folded in
/// (a deterministic replay marker — never wall-clock time, so equal inputs
/// rebuild to identical rows).
pub const META_BUILT_AT: &str = "built_at";

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS meta (k TEXT PRIMARY KEY, v TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS claims (
  digest TEXT PRIMARY KEY, claim_id TEXT NOT NULL, kind TEXT NOT NULL,
  content TEXT NOT NULL, sensitivity TEXT NOT NULL, scope TEXT NOT NULL,
  status TEXT NOT NULL, dedup_key TEXT NOT NULL, source_events TEXT NOT NULL,
  root TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS edges (
  digest TEXT PRIMARY KEY, from_id TEXT NOT NULL, to_id TEXT, kind TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS entities (
  claim_digest TEXT NOT NULL, entity_key TEXT NOT NULL, kind TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS roots (
  scope TEXT NOT NULL, root TEXT NOT NULL, transition_id TEXT, seq INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS retracted (
  digest TEXT PRIMARY KEY, claim_id TEXT NOT NULL, kind TEXT NOT NULL,
  content TEXT NOT NULL, sensitivity TEXT NOT NULL, scope TEXT NOT NULL
);
CREATE VIRTUAL TABLE IF NOT EXISTS claims_fts USING fts5(
  content, content='claims', content_rowid=rowid
);
CREATE TABLE IF NOT EXISTS activation_log (
  claim_digest TEXT NOT NULL, score REAL NOT NULL, scorer TEXT NOT NULL, at_seq INTEGER NOT NULL
);
";

/// The per-scope fold excerpt the index ingests. The retrieval crate takes
/// data, never actors: the caller resolves the scope's pinned root and folds
/// it ([`kanbei_memory::MemoryRootActor::fold`]) before calling build or
/// reconcile.
pub struct ScopeIndexInput {
    pub scope: MemoryScope,
    /// The scope's current root digest (`None` = empty scope).
    pub root: Option<Digest>,
    /// The projection-time fold: active claims + edges, retracted claims, and
    /// the manifest history.
    pub fold: RootFold,
}

/// Row counts after a build or reconcile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuildReport {
    pub claims: u64,
    pub edges: u64,
    pub entities: u64,
    pub scopes: u64,
}

/// The disposable SQLite retrieval index. Disposable: [`build`](Self::build)
/// wipes and rebuilds; [`reconcile`](Self::reconcile) refreshes incrementally.
pub struct MemoryIndex {
    pub(crate) conn: Connection,
    path: PathBuf,
}

impl MemoryIndex {
    /// Opens (creating if needed) the index DB and ensures the schema.
    /// WAL + synchronous OFF, matching the disposable projection conventions
    /// (kanbei-projection: no durability claim).
    pub fn open(path: &Path) -> Result<Self, RetrievalError> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "OFF")?;
        conn.execute_batch(SCHEMA_SQL)?;
        Ok(Self {
            conn,
            path: path.to_path_buf(),
        })
    }

    /// The DB path this index was opened at.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Disposable clean rebuild: one transaction deletes every table and
    /// inserts the rows of every scope's fold, then the FTS index is
    /// resynced with a `rebuild` command. `scorer` is recorded in meta for
    /// replay/evaluation (architecture.md:506).
    pub fn build(
        &mut self,
        scopes: &[ScopeIndexInput],
        scorer: &str,
    ) -> Result<BuildReport, RetrievalError> {
        self.conn.execute_batch("BEGIN")?;
        for table in [
            "meta",
            "activation_log",
            "roots",
            "retracted",
            "entities",
            "edges",
            "claims",
        ] {
            self.conn.execute(&format!("DELETE FROM {table}"), [])?;
        }
        let mut report = BuildReport {
            claims: 0,
            edges: 0,
            entities: 0,
            scopes: scopes.len() as u64,
        };
        let mut built_at = 0u64;
        for (seq, input) in scopes.iter().enumerate() {
            let scope_json = scope_json(&input.scope)?;
            let root = input.root.map(|d| d.to_string()).unwrap_or_default();
            let active_ids: Vec<_> = input.fold.claims.iter().map(|(_, c)| c.claim_id).collect();
            let active_edges: Vec<ClaimEdge> =
                input.fold.edges.iter().map(|(_, e)| e.clone()).collect();
            for (digest, claim) in &input.fold.claims {
                let status = derive_validation_status(claim.claim_id, &active_ids, &active_edges);
                let dedup_key = Digest::new(&dedup_bytes(claim)).to_string();
                let source_events = source_events_json(claim)?;
                built_at = built_at.max(claim.provenance.event);
                self.conn.execute(
                    "INSERT INTO claims (digest, claim_id, kind, content, sensitivity, scope, \
                     status, dedup_key, source_events, root) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        digest.to_string(),
                        claim.claim_id.to_string(),
                        claim.kind,
                        claim.content,
                        claim.sensitivity,
                        scope_json,
                        status_str(&status),
                        dedup_key,
                        source_events,
                        root,
                    ],
                )?;
                for (key, kind) in extract_entities(&claim.content) {
                    self.conn.execute(
                        "INSERT INTO entities (claim_digest, entity_key, kind) VALUES (?1, ?2, ?3)",
                        params![digest.to_string(), key, entity_kind_str(&kind)],
                    )?;
                    report.entities += 1;
                }
                report.claims += 1;
            }
            for (digest, edge) in &input.fold.edges {
                built_at = built_at.max(edge.provenance.event);
                self.conn.execute(
                    "INSERT INTO edges (digest, from_id, to_id, kind) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        digest.to_string(),
                        edge.from.to_string(),
                        edge.to.map(|t| t.to_string()),
                        edge_kind_str(&edge.kind),
                    ],
                )?;
                report.edges += 1;
            }
            for (digest, claim) in &input.fold.retracted {
                self.conn.execute(
                    "INSERT INTO retracted (digest, claim_id, kind, content, sensitivity, scope) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        digest.to_string(),
                        claim.claim_id.to_string(),
                        claim.kind,
                        claim.content,
                        claim.sensitivity,
                        scope_json,
                    ],
                )?;
            }
            self.conn.execute(
                "INSERT INTO roots (scope, root, transition_id, seq) VALUES (?1, ?2, NULL, ?3)",
                params![scope_json, root, seq as i64],
            )?;
        }
        self.conn.execute(
            "INSERT INTO meta (k, v) VALUES (?1, ?2), (?3, ?4)",
            params![META_SCORER, scorer, META_BUILT_AT, built_at.to_string()],
        )?;
        self.conn.execute_batch("COMMIT")?;
        self.rebuild_fts()?;
        Ok(report)
    }

    /// Incremental refresh: upserts claims by digest, deletes claims rows no
    /// longer in the folds (plus their entity rows), fully refreshes
    /// edges/roots/retracted, and resyncs FTS. Meta is untouched (reconcile
    /// carries no scorer). Deterministic — equal inputs, equal rows.
    pub fn reconcile(&mut self, scopes: &[ScopeIndexInput]) -> Result<BuildReport, RetrievalError> {
        self.conn.execute_batch("BEGIN")?;
        let mut new_claims: HashSet<String> = HashSet::new();
        let mut new_retracted: HashSet<String> = HashSet::new();
        for input in scopes {
            for (digest, _) in &input.fold.claims {
                new_claims.insert(digest.to_string());
            }
            for (digest, _) in &input.fold.retracted {
                new_retracted.insert(digest.to_string());
            }
        }
        let existing: Vec<String> = self
            .conn
            .prepare("SELECT digest FROM claims")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        for digest in existing {
            if !new_claims.contains(&digest) {
                self.conn
                    .execute("DELETE FROM claims WHERE digest = ?1", params![digest])?;
                self.conn.execute(
                    "DELETE FROM entities WHERE claim_digest = ?1",
                    params![digest],
                )?;
            }
        }
        let existing_retracted: Vec<String> = self
            .conn
            .prepare("SELECT digest FROM retracted")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<Vec<String>>>()?;
        for digest in existing_retracted {
            if !new_retracted.contains(&digest) {
                self.conn
                    .execute("DELETE FROM retracted WHERE digest = ?1", params![digest])?;
            }
        }
        // Scope-wide tables are full-refresh across ALL scopes, not per
        // scope: a per-scope `DELETE` here keeps only the last scope's rows
        // after a multi-scope reconcile (the edges/roots of every earlier
        // scope were silently dropped).
        self.conn.execute("DELETE FROM edges", [])?;
        self.conn.execute("DELETE FROM roots", [])?;
        for (seq, input) in scopes.iter().enumerate() {
            let scope_json = scope_json(&input.scope)?;
            let root = input.root.map(|d| d.to_string()).unwrap_or_default();
            let active_ids: Vec<_> = input.fold.claims.iter().map(|(_, c)| c.claim_id).collect();
            let active_edges: Vec<ClaimEdge> =
                input.fold.edges.iter().map(|(_, e)| e.clone()).collect();
            for (digest, claim) in &input.fold.claims {
                // claims are immutable, but re-deriving keeps the index
                // self-consistent under any fold
                let status = derive_validation_status(claim.claim_id, &active_ids, &active_edges);
                let dedup_key = Digest::new(&dedup_bytes(claim)).to_string();
                let source_events = source_events_json(claim)?;
                self.conn.execute(
                    "DELETE FROM entities WHERE claim_digest = ?1",
                    params![digest.to_string()],
                )?;
                self.conn.execute(
                    "INSERT OR REPLACE INTO claims (digest, claim_id, kind, content, sensitivity, \
                     scope, status, dedup_key, source_events, root) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        digest.to_string(),
                        claim.claim_id.to_string(),
                        claim.kind,
                        claim.content,
                        claim.sensitivity,
                        scope_json,
                        status_str(&status),
                        dedup_key,
                        source_events,
                        root,
                    ],
                )?;
                for (key, kind) in extract_entities(&claim.content) {
                    self.conn.execute(
                        "INSERT INTO entities (claim_digest, entity_key, kind) VALUES (?1, ?2, ?3)",
                        params![digest.to_string(), key, entity_kind_str(&kind)],
                    )?;
                }
            }
            for (digest, edge) in &input.fold.edges {
                self.conn.execute(
                    "INSERT INTO edges (digest, from_id, to_id, kind) VALUES (?1, ?2, ?3, ?4)",
                    params![
                        digest.to_string(),
                        edge.from.to_string(),
                        edge.to.map(|t| t.to_string()),
                        edge_kind_str(&edge.kind),
                    ],
                )?;
            }
            for (digest, claim) in &input.fold.retracted {
                self.conn.execute(
                    "INSERT OR REPLACE INTO retracted (digest, claim_id, kind, content, \
                     sensitivity, scope) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        digest.to_string(),
                        claim.claim_id.to_string(),
                        claim.kind,
                        claim.content,
                        claim.sensitivity,
                        scope_json,
                    ],
                )?;
            }
            self.conn.execute(
                "INSERT INTO roots (scope, root, transition_id, seq) VALUES (?1, ?2, NULL, ?3)",
                params![scope_json, root, seq as i64],
            )?;
        }
        self.conn.execute_batch("COMMIT")?;
        self.rebuild_fts()?;
        let count = |table: &str| -> Result<u64, RetrievalError> {
            Ok(self
                .conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))?)
        };
        Ok(BuildReport {
            claims: count("claims")?,
            edges: count("edges")?,
            entities: count("entities")?,
            scopes: scopes.len() as u64,
        })
    }

    /// Resyncs the FTS5 external-content table from the claims table
    /// (INSERT INTO claims_fts(claims_fts) VALUES('rebuild')).
    pub fn rebuild_fts(&mut self) -> Result<(), RetrievalError> {
        self.conn
            .execute("INSERT INTO claims_fts(claims_fts) VALUES('rebuild')", [])?;
        Ok(())
    }

    /// Every activation-log row for one claim, oldest projection first.
    pub fn activation_rows(
        &self,
        claim_digest: &Digest,
    ) -> Result<Vec<(f64, String, u64)>, RetrievalError> {
        let rows = self
            .conn
            .prepare(
                "SELECT score, scorer, at_seq FROM activation_log \
                 WHERE claim_digest = ?1 ORDER BY at_seq, rowid",
            )?
            .query_map(params![claim_digest.to_string()], |r| {
                Ok((
                    r.get::<_, f64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, u64>(2)?,
                ))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(rows)
    }

    /// Deletes every row (test helper + rebuild path); FTS is resynced.
    pub fn clear(&mut self) -> Result<(), RetrievalError> {
        self.conn.execute_batch(
            "DELETE FROM activation_log; DELETE FROM meta; DELETE FROM roots; \
             DELETE FROM retracted; DELETE FROM entities; DELETE FROM edges; DELETE FROM claims;",
        )?;
        self.rebuild_fts()
    }

    /// Salience projection writes disposable activation rows
    /// (architecture.md:503): claim digest, score, scorer version, frozen seq.
    pub(crate) fn write_activation_rows(
        &self,
        rows: &[(String, f64, String, u64)],
    ) -> Result<(), RetrievalError> {
        let mut stmt = self.conn.prepare(
            "INSERT INTO activation_log (claim_digest, score, scorer, at_seq) \
             VALUES (?1, ?2, ?3, ?4)",
        )?;
        for (digest, score, scorer, at_seq) in rows {
            stmt.execute(params![digest, score, scorer, at_seq])?;
        }
        Ok(())
    }
}

/// Canonical scope text form: the serde JSON.
fn scope_json(scope: &MemoryScope) -> Result<String, RetrievalError> {
    serde_json::to_string(scope)
        .map_err(|e| RetrievalError::InvalidInput(format!("serialize scope {scope:?}: {e}")))
}

/// The R-12/M-02 lineage-union dedup key: a digest over the canonical JSON of
/// (kind, content) — never provenance.
fn dedup_bytes(claim: &Claim) -> Vec<u8> {
    serde_json::to_vec(&(claim.kind.as_str(), claim.content.as_str()))
        .expect("canonical (kind, content) serialization cannot fail")
}

/// The claim's source events (JSON array). Ordinary claims carry no event
/// refs; source-backed (promotion) claims carry their own provenance event
/// (architecture.md "source_events = JSON array of provenance.event"). This
/// is what makes step 8's "rerank source-backed evidence" meaningful.
fn source_events_json(claim: &Claim) -> Result<String, RetrievalError> {
    let events = if claim.provenance.source_claims.is_empty() {
        Vec::new()
    } else {
        vec![claim.provenance.event]
    };
    serde_json::to_string(&events).map_err(|e| {
        RetrievalError::InvalidInput(format!(
            "claim {}: serialize source_events: {e}",
            claim.claim_id
        ))
    })
}

/// The serde-lowercase text forms of the six-edge vocabulary.
pub(crate) fn edge_kind_str(kind: &EdgeKind) -> &'static str {
    match kind {
        EdgeKind::EvidenceFor => "evidence_for",
        EdgeKind::Supports => "supports",
        EdgeKind::Contradicts => "contradicts",
        EdgeKind::Supersedes => "supersedes",
        EdgeKind::PromotedFrom => "promoted_from",
        EdgeKind::AppliesTo => "applies_to",
    }
}

/// The stored status text form.
pub(crate) fn status_str(status: &ValidationStatus) -> &'static str {
    match status {
        ValidationStatus::Proposed => "Proposed",
        ValidationStatus::Approved => "Approved",
        ValidationStatus::Active => "Active",
        ValidationStatus::Superseded => "Superseded",
        ValidationStatus::Retracted => "Retracted",
    }
}

/// Parses a stored status text form.
pub(crate) fn status_parse(s: &str) -> Result<ValidationStatus, RetrievalError> {
    match s {
        "Proposed" => Ok(ValidationStatus::Proposed),
        "Approved" => Ok(ValidationStatus::Approved),
        "Active" => Ok(ValidationStatus::Active),
        "Superseded" => Ok(ValidationStatus::Superseded),
        "Retracted" => Ok(ValidationStatus::Retracted),
        other => Err(RetrievalError::InvalidInput(format!(
            "index row has unknown validation status {other:?}"
        ))),
    }
}

/// The stored entity-kind text form.
pub(crate) fn entity_kind_str(kind: &EntityKind) -> &'static str {
    match kind {
        EntityKind::Path => "path",
        EntityKind::Symbol => "symbol",
        EntityKind::Commit => "commit",
        EntityKind::Error => "error",
        EntityKind::Ticket => "ticket",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{
        make_claim, make_edge, make_fold, remove_db, scope_input, session, temp_db,
    };
    use kanbei_core::Id128;

    fn table_dump(index: &MemoryIndex) -> Vec<String> {
        let mut out = Vec::new();
        for table in ["meta", "claims", "edges", "entities", "roots", "retracted"] {
            let mut stmt = index
                .conn
                .prepare(&format!("SELECT * FROM {table} ORDER BY 1, rowid"))
                .unwrap();
            let cols = stmt.column_count();
            let mut rows = stmt.query([]).unwrap();
            while let Some(row) = rows.next().unwrap() {
                let mut parts = Vec::new();
                for i in 0..cols {
                    let v: rusqlite::types::Value = row.get(i).unwrap();
                    parts.push(format!("{v:?}"));
                }
                out.push(format!("{table}: {}", parts.join("|")));
            }
        }
        out
    }

    #[test]
    fn build_indexes_two_scopes_with_retracted() {
        let path = temp_db("build");
        let mut index = MemoryIndex::open(&path).unwrap();
        let s = session();
        let (d1, c1) = make_claim(
            Id128::generate(),
            "decision",
            "use /abs/lib.rs everywhere",
            s,
            1,
        );
        let (d2, c2) = make_claim(
            Id128::generate(),
            "constraint",
            "the widget is pinned",
            s,
            2,
        );
        let (dr, cr) = make_claim(Id128::generate(), "decision", "old widget plan", s, 3);
        let (de, ce) = make_edge(c1.claim_id, Some(c2.claim_id), EdgeKind::EvidenceFor);
        let (der, cer) = make_edge(cr.claim_id, None, EdgeKind::Supersedes);
        let cr_id = cr.claim_id;
        let fold_lifetime = make_fold(
            vec![(d1, c1), (d2, c2)],
            vec![(de, ce), (der, cer)],
            vec![(dr, cr)],
        );
        let (d3, c3) = make_claim(
            Id128::generate(),
            "preference",
            "prefer /abs/other.rs",
            s,
            4,
        );
        let fold_project = make_fold(vec![(d3, c3)], vec![], vec![]);
        let report = index
            .build(
                &[
                    scope_input(MemoryScope::Lifetime, fold_lifetime),
                    scope_input(MemoryScope::Project(Id128::generate()), fold_project),
                ],
                "salience-v1",
            )
            .unwrap();
        assert_eq!(report.claims, 3);
        assert_eq!(report.edges, 2);
        assert_eq!(report.entities, 2);
        assert_eq!(report.scopes, 2);
        let claims: u64 = index
            .conn
            .query_row("SELECT COUNT(*) FROM claims", [], |r| r.get(0))
            .unwrap();
        assert_eq!(claims, 3);
        // retracted claims live only in the retracted table
        let in_claims: u64 = index
            .conn
            .query_row(
                "SELECT COUNT(*) FROM claims WHERE digest = ?1",
                params![dr.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(in_claims, 0);
        let in_retracted: u64 = index
            .conn
            .query_row(
                "SELECT COUNT(*) FROM retracted WHERE digest = ?1",
                params![dr.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(in_retracted, 1);
        // one root row per scope, seq = index, transition_id NULL
        let roots: Vec<(i64, Option<String>)> = {
            let mut stmt = index
                .conn
                .prepare("SELECT seq, transition_id FROM roots ORDER BY seq")
                .unwrap();
            let rows = stmt.query_map([], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
            rows.collect::<rusqlite::Result<Vec<_>>>().unwrap()
        };
        assert_eq!(roots.len(), 2);
        assert_eq!(roots[0].0, 0);
        assert_eq!(roots[1].0, 1);
        assert!(roots.iter().all(|(_, t)| t.is_none()));
        // edges kept even when they reference the retracted claim
        let edges: u64 = index
            .conn
            .query_row(
                "SELECT COUNT(*) FROM edges WHERE from_id = ?1",
                params![cr_id.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(edges, 1);
        // meta records the scorer and a deterministic build marker
        let scorer: String = index
            .conn
            .query_row("SELECT v FROM meta WHERE k = 'scorer'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(scorer, "salience-v1");
        let built_at: String = index
            .conn
            .query_row("SELECT v FROM meta WHERE k = 'built_at'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(built_at, "4"); // max provenance event across the folds
        // FTS is synced after build
        let fts: u64 = index
            .conn
            .query_row("SELECT COUNT(*) FROM claims_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fts, 3);
        remove_db(&path);
    }

    #[test]
    fn build_is_deterministic() {
        let path = temp_db("determinism");
        let mut index = MemoryIndex::open(&path).unwrap();
        let s = session();
        let (d1, c1) = make_claim(
            Id128::generate(),
            "decision",
            "use /abs/lib.rs everywhere",
            s,
            1,
        );
        let (d2, c2) = make_claim(
            Id128::generate(),
            "constraint",
            "the widget is pinned",
            s,
            2,
        );
        let (dr, cr) = make_claim(Id128::generate(), "decision", "old widget plan", s, 3);
        let (de, ce) = make_edge(c1.claim_id, Some(c2.claim_id), EdgeKind::EvidenceFor);
        let fold = make_fold(vec![(d1, c1), (d2, c2)], vec![(de, ce)], vec![(dr, cr)]);
        let input = scope_input(MemoryScope::Lifetime, fold);
        index
            .build(std::slice::from_ref(&input), "salience-v1")
            .unwrap();
        let first = table_dump(&index);
        index
            .build(std::slice::from_ref(&input), "salience-v1")
            .unwrap();
        let second = table_dump(&index);
        assert_eq!(first, second);
        remove_db(&path);
    }

    /// Multi-scope reconcile must not clobber earlier scopes' rows: the
    /// edges/roots tables are full-refresh across ALL scopes (a per-scope
    /// DELETE kept only the last scope's rows — lifetime contradictions
    /// and one-hop edges silently vanished after any multi-scope reconcile,
    /// which the session runs before every memory.query).
    #[test]
    fn reconcile_multi_scope_keeps_earlier_scope_rows() {
        let path = temp_db("reconcile-multi");
        let mut index = MemoryIndex::open(&path).unwrap();
        let s = session();
        let (d1, c1) = make_claim(Id128::generate(), "decision", "the widget is fast", s, 1);
        let (d2, c2) = make_claim(Id128::generate(), "decision", "the fast widget broke", s, 2);
        let (de, ce) = make_edge(c1.claim_id, Some(c2.claim_id), EdgeKind::Contradicts);
        let fold_lifetime = make_fold(vec![(d1, c1), (d2, c2)], vec![(de, ce)], vec![]);
        let (d3, c3) = make_claim(Id128::generate(), "preference", "prefer /abs/other.rs", s, 3);
        let fold_project = make_fold(vec![(d3, c3)], vec![], vec![]);
        let inputs = [
            scope_input(MemoryScope::Lifetime, fold_lifetime),
            scope_input(MemoryScope::Project(Id128::generate()), fold_project),
        ];
        index.build(&inputs, "salience-v1").unwrap();
        let built_edges: u64 = index
            .conn
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))
            .unwrap();
        let built_roots: u64 = index
            .conn
            .query_row("SELECT COUNT(*) FROM roots", [], |r| r.get(0))
            .unwrap();
        let report = index.reconcile(&inputs).unwrap();
        assert_eq!(report.claims, 3);
        assert_eq!(report.scopes, 2);
        let after_edges: u64 = index
            .conn
            .query_row("SELECT COUNT(*) FROM edges", [], |r| r.get(0))
            .unwrap();
        let after_roots: u64 = index
            .conn
            .query_row("SELECT COUNT(*) FROM roots", [], |r| r.get(0))
            .unwrap();
        assert_eq!(after_edges, built_edges, "reconcile dropped scope edges");
        assert_eq!(after_roots, built_roots, "reconcile dropped scope roots");
        assert_eq!(after_roots, 2, "both scopes must keep a roots row");
        remove_db(&path);
    }

    #[test]
    fn reconcile_refreshes_incremental() {
        let path = temp_db("reconcile");
        let mut index = MemoryIndex::open(&path).unwrap();
        let s = session();
        let (da, ca) = make_claim(
            Id128::generate(),
            "decision",
            "widget alpha /abs/a.rs",
            s,
            1,
        );
        let (db, cb) = make_claim(Id128::generate(), "decision", "widget beta", s, 2);
        let (dr, cr) = make_claim(Id128::generate(), "decision", "old widget", s, 3);
        index
            .build(
                &[scope_input(
                    MemoryScope::Lifetime,
                    make_fold(vec![(da, ca.clone()), (db, cb)], vec![], vec![(dr, cr)]),
                )],
                "salience-v1",
            )
            .unwrap();
        let (dc, cc) = make_claim(Id128::generate(), "decision", "widget gamma", s, 4);
        let report = index
            .reconcile(&[scope_input(
                MemoryScope::Lifetime,
                make_fold(vec![(da, ca), (dc, cc)], vec![], vec![]),
            )])
            .unwrap();
        let present = |digest: &Digest| -> bool {
            index
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM claims WHERE digest = ?1",
                    params![digest.to_string()],
                    |r| r.get::<_, u64>(0),
                )
                .unwrap()
                > 0
        };
        assert!(present(&da));
        assert!(!present(&db));
        assert!(present(&dc));
        assert_eq!(report.claims, 2);
        // stale entity rows are gone with their claim
        let entities: u64 = index
            .conn
            .query_row(
                "SELECT COUNT(*) FROM entities WHERE claim_digest = ?1",
                params![db.to_string()],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(entities, 0);
        // retracted table refreshed
        let retracted: u64 = index
            .conn
            .query_row("SELECT COUNT(*) FROM retracted", [], |r| r.get(0))
            .unwrap();
        assert_eq!(retracted, 0);
        // FTS resynced
        let fts: u64 = index
            .conn
            .query_row("SELECT COUNT(*) FROM claims_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fts, 2);
        remove_db(&path);
    }

    #[test]
    fn clear_empties_the_index() {
        let path = temp_db("clear");
        let mut index = MemoryIndex::open(&path).unwrap();
        let s = session();
        let (d1, c1) = make_claim(
            Id128::generate(),
            "decision",
            "use /abs/lib.rs everywhere",
            s,
            1,
        );
        index
            .build(
                &[scope_input(
                    MemoryScope::Lifetime,
                    make_fold(vec![(d1, c1)], vec![], vec![]),
                )],
                "salience-v1",
            )
            .unwrap();
        index.clear().unwrap();
        for table in [
            "meta",
            "claims",
            "edges",
            "entities",
            "roots",
            "retracted",
            "activation_log",
        ] {
            let n: u64 = index
                .conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap();
            assert_eq!(n, 0, "{table} should be empty after clear");
        }
        let fts: u64 = index
            .conn
            .query_row("SELECT COUNT(*) FROM claims_fts", [], |r| r.get(0))
            .unwrap();
        assert_eq!(fts, 0);
        remove_db(&path);
    }
}
