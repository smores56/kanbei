//! The 9-step retrieval pipeline (architecture.md:509-519): scope resolution,
//! exact entities, FTS5/BM25 lexical search with a LIKE fallback, the
//! validity/supersession filter with contradiction annotation, authority
//! ordering + lineage-union dedup, fusion, bounded one-hop expansion,
//! source-backed rerank, and bounded return. Deterministic: equal inputs
//! produce equal results (ties break on claim_id ascending).

use std::cmp::Ordering;
use std::collections::{BTreeSet, HashMap, HashSet};

use kanbei_context::{Contradiction, EvidenceClaim};
use kanbei_memory::{MemoryScope, ValidationStatus};
use rusqlite::{OptionalExtension, params, params_from_iter};

use crate::entities::{EntityKind, extract_entities, normalize_query};
use crate::error::RetrievalError;
use crate::index::{MemoryIndex, status_parse};

/// The documented base score for candidates that hit an exact entity but no
/// lexical term (step 6 floor), and for fallback-lexical candidates (no
/// bm25 available in the LIKE path).
const ENTITY_FLOOR_SCORE: f64 = 5.0;
/// The documented ceiling for the source-backed rerank (step 8).
const EVIDENCE_SCORE_CEILING: f64 = 100.0;
/// How many top fused-score matches seed the one-hop expansion (step 7).
const EXPANSION_SEED: usize = 8;
/// The lexical candidate cap (step 3).
const LEXICAL_CAP: u64 = 200;

/// A retrieval query. Step 1 (capability resolution) is the CALLER's job:
/// the caller resolves the allowed memory scopes and fills `scopes`; the
/// search only filters on them.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchQuery {
    pub text: String,
    /// Caller-resolved allowed scopes; empty means no access and no results.
    pub scopes: Vec<MemoryScope>,
    /// Bounded return (step 9).
    pub max_results: u64,
    /// The fusion multiplier for exact-entity hits (step 6).
    pub entity_boost: f64,
    /// The one-hop candidate score ratio (step 7).
    pub hop_score_ratio: f64,
    /// The source-backed rerank multiplier (step 8).
    pub evidence_boost: f64,
    /// The total one-hop expansion budget (step 7).
    pub max_hops: u64,
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            scopes: Vec::new(),
            max_results: 16,
            entity_boost: 3.0,
            hop_score_ratio: 0.4,
            evidence_boost: 1.2,
            max_hops: 16,
        }
    }
}

/// The bounded, ordered retrieval result.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchResult {
    /// Best-first claims, truncated to `max_results`.
    pub claims: Vec<EvidenceClaim>,
    /// The exact entities extracted from the query text (step 2).
    pub query_entities: Vec<(String, EntityKind)>,
    /// True when the FTS5 path served the lexical step; false on the LIKE
    /// fallback or when the query has no tokens.
    pub fts_used: bool,
    /// How many one-hop expansions were applied (step 7).
    pub expanded: u64,
}

/// One scored candidate in the pipeline pool.
struct Candidate {
    digest: String,
    claim_id: String,
    kind: String,
    content: String,
    sensitivity: String,
    status: ValidationStatus,
    dedup_key: String,
    score: f64,
    source_events: Vec<u64>,
    contradictions: Vec<Contradiction>,
}

impl Candidate {
    /// Authority rank (R-12/M-07): Active > Approved > Proposed. The
    /// superseded/retracted statuses never reach the pool (step 4 filter).
    fn rank(&self) -> u8 {
        match self.status {
            ValidationStatus::Active => 3,
            ValidationStatus::Approved => 2,
            ValidationStatus::Proposed => 1,
            ValidationStatus::Superseded | ValidationStatus::Retracted => 0,
        }
    }
}

/// Deterministic candidate order: authority rank desc, fused score desc,
/// claim_id ascending.
fn candidate_order(a: &Candidate, b: &Candidate) -> Ordering {
    b.rank()
        .cmp(&a.rank())
        .then_with(|| b.score.total_cmp(&a.score))
        .then_with(|| a.claim_id.cmp(&b.claim_id))
}

/// A `scope IN (?, ...)` clause with one mark per allowed scope.
fn scope_clause(n: usize) -> String {
    format!("scope IN ({})", vec!["?"; n].join(","))
}

/// One FTS5 MATCH token: quoted, with embedded quotes doubled. A token with
/// an odd quote count is left verbatim — doubling cannot balance it, so the
/// MATCH expression is malformed and the documented LIKE fallback engages
/// (fts_used = false).
fn quote_token(t: &str) -> String {
    if t.bytes().filter(|b| *b == b'"').count() % 2 == 0 {
        format!("\"{}\"", t.replace('"', "\"\""))
    } else {
        format!("\"{t}\"")
    }
}

/// The full claim row for one digest; None when the claim is not in the
/// index (a hop reference outside the indexed scopes).
struct ClaimRow {
    claim_id: String,
    kind: String,
    content: String,
    sensitivity: String,
    status: ValidationStatus,
    dedup_key: String,
    source_events: Vec<u64>,
}

impl MemoryIndex {
    /// The 9-step retrieval pipeline. Steps 1-4 and 6 run as documented; the
    /// fused scores are computed when candidates form (step 6) because step
    /// 5's dedup keeps the best (rank, score) per lineage key.
    pub fn search(&self, q: &SearchQuery) -> Result<SearchResult, RetrievalError> {
        // Step 1 — scope resolution: the caller resolves capabilities; an
        // empty scope list means no access and no results.
        if q.scopes.is_empty() {
            return Ok(SearchResult {
                claims: Vec::new(),
                query_entities: extract_entities(&q.text),
                fts_used: false,
                expanded: 0,
            });
        }
        let scope_marks = scope_clause(q.scopes.len());
        let scope_params: Vec<String> = q
            .scopes
            .iter()
            .map(|s| serde_json::to_string(s).expect("scope serialization cannot fail"))
            .collect();

        // Step 2 — exact entities: claims whose entity projection contains
        // any query entity key.
        let query_entities = extract_entities(&q.text);
        let mut entity_hits: HashSet<String> = HashSet::new();
        if !query_entities.is_empty() {
            let keys: Vec<String> = query_entities.iter().map(|(k, _)| k.clone()).collect();
            let marks = vec!["?"; keys.len()].join(",");
            let sql = format!(
                "SELECT DISTINCT e.claim_digest FROM entities e JOIN claims c ON c.digest = e.claim_digest \
                 WHERE e.entity_key IN ({marks}) AND {scope_marks}"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let mut rows = stmt.query(params_from_iter(keys.iter().chain(scope_params.iter())))?;
            while let Some(row) = rows.next()? {
                entity_hits.insert(row.get(0)?);
            }
        }

        // Step 3 — FTS5/BM25 lexical search (lower bm25 = better), capped at
        // LEXICAL_CAP candidates. Tokens are normalized words joined by AND,
        // each quoted with embedded quotes doubled. A malformed MATCH
        // expression (e.g. an unterminated quote token) falls back to
        // per-token `content LIKE '%tok%'` (documented fallback).
        let tokens: Vec<String> = normalize_query(&q.text)
            .split_whitespace()
            .map(str::to_string)
            .collect();
        let mut lexical: HashMap<String, f64> = HashMap::new();
        let mut fallback_hits: HashSet<String> = HashSet::new();
        let mut fts_used = false;
        if !tokens.is_empty() {
            let match_expr = tokens
                .iter()
                .map(|t| quote_token(t))
                .collect::<Vec<_>>()
                .join(" AND ");
            let sql = format!(
                "SELECT c.digest, bm25(claims_fts) FROM claims_fts JOIN claims c \
                 ON c.rowid = claims_fts.rowid WHERE claims_fts MATCH ? AND {scope_marks} \
                 ORDER BY bm25(claims_fts) LIMIT {LEXICAL_CAP}"
            );
            let fts = (|| -> rusqlite::Result<Vec<(String, f64)>> {
                let mut stmt = self.conn.prepare(&sql)?;
                let mut rows = stmt.query(params_from_iter(
                    [match_expr.as_str()]
                        .into_iter()
                        .chain(scope_params.iter().map(|s| s.as_str())),
                ))?;
                let mut out = Vec::new();
                while let Some(row) = rows.next()? {
                    out.push((row.get(0)?, row.get(1)?));
                }
                Ok(out)
            })();
            match fts {
                Ok(rows) => {
                    fts_used = true;
                    for (digest, bm25) in rows {
                        lexical.insert(digest, bm25);
                    }
                }
                Err(_) => {
                    'tokens: for tok in &tokens {
                        if fallback_hits.len() >= LEXICAL_CAP as usize {
                            break;
                        }
                        let esc = tok
                            .replace('\\', "\\\\")
                            .replace('%', "\\%")
                            .replace('_', "\\_");
                        let sql = format!(
                            "SELECT digest FROM claims WHERE content LIKE ? ESCAPE '\\' AND {scope_marks}"
                        );
                        let like = format!("%{esc}%");
                        let mut stmt = self.conn.prepare(&sql)?;
                        let mut rows = stmt.query(params_from_iter(
                            [like.as_str()]
                                .into_iter()
                                .chain(scope_params.iter().map(|s| s.as_str())),
                        ))?;
                        while let Some(row) = rows.next()? {
                            if fallback_hits.len() >= LEXICAL_CAP as usize {
                                break 'tokens;
                            }
                            fallback_hits.insert(row.get(0)?);
                        }
                    }
                }
            }
        }

        // Candidate pool: union of entity and lexical hits, deterministically
        // ordered by digest.
        let mut digests: Vec<String> = Vec::new();
        for d in entity_hits
            .iter()
            .chain(fallback_hits.iter())
            .chain(lexical.keys())
        {
            digests.push(d.clone());
        }
        digests.sort();
        digests.dedup();

        // Step 4 — validity filter (superseded/retracted claims never reach
        // the result set; canonical folds only produce Active rows, so this
        // also defends inconsistent folds) + step 6 fusion at formation.
        let mut pool: Vec<Candidate> = Vec::new();
        for digest in &digests {
            let Some(row) = self.fetch_claim_row(digest)? else {
                continue;
            };
            if matches!(
                row.status,
                ValidationStatus::Superseded | ValidationStatus::Retracted
            ) {
                continue;
            }
            // Fusion: lexical base is -bm25 (lower bm25 = better); entity-only
            // and fallback-lexical candidates enter at the documented floor.
            // Entity hits are boosted once (claims in both are not double
            // boosted).
            let base = lexical
                .get(digest)
                .map(|b| -*b)
                .unwrap_or(ENTITY_FLOOR_SCORE);
            let score = if entity_hits.contains(digest) {
                base * q.entity_boost
            } else {
                base
            };
            pool.push(Candidate {
                digest: digest.clone(),
                claim_id: row.claim_id,
                kind: row.kind,
                content: row.content,
                sensitivity: row.sensitivity,
                status: row.status,
                dedup_key: row.dedup_key,
                score,
                source_events: row.source_events,
                contradictions: Vec::new(),
            });
        }

        // Step 5 — authority ordering + lineage-union dedup: keep the best
        // (rank, score, claim_id) per dedup_key, ordered by (rank, score).
        pool.sort_by(candidate_order);
        pool.dedup_by(|a, b| a.dedup_key == b.dedup_key);

        // Step 7 — bounded one-hop expansion seeded by the top fused-score
        // matches: claims sharing an entity key, and edge-connected claims.
        // Hop candidates are validity-filtered, deduped, and scored at
        // `seed_score * hop_score_ratio`; the total additions respect
        // `max_hops` (processed in sorted claim_id order for determinism).
        let mut expanded: u64 = 0;
        let seeds: Vec<(String, String, f64)> = pool
            .iter()
            .take(EXPANSION_SEED)
            .map(|c| (c.digest.clone(), c.claim_id.clone(), c.score))
            .collect();
        for (seed_digest, seed_claim_id, seed_score) in seeds {
            if expanded >= q.max_hops {
                break;
            }
            let mut hops: BTreeSet<(String, String)> = BTreeSet::new();
            let keys: Vec<String> = self
                .conn
                .prepare("SELECT entity_key FROM entities WHERE claim_digest = ?1")?
                .query_map(params![seed_digest], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            if !keys.is_empty() {
                let marks = vec!["?"; keys.len()].join(",");
                let sql = format!(
                    "SELECT DISTINCT e.claim_digest, c.claim_id FROM entities e \
                     JOIN claims c ON c.digest = e.claim_digest \
                     WHERE e.claim_digest != ? AND e.entity_key IN ({marks}) AND {scope_marks}"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let mut rows = stmt.query(params_from_iter(
                    [seed_digest.as_str()]
                        .into_iter()
                        .chain(keys.iter().map(|s| s.as_str()))
                        .chain(scope_params.iter().map(|s| s.as_str())),
                ))?;
                while let Some(row) = rows.next()? {
                    hops.insert((row.get(0)?, row.get(1)?));
                }
            }
            // edge-connected claims: both directions, one hop
            let mut neighbor_ids: Vec<String> = Vec::new();
            let mut stmt = self
                .conn
                .prepare("SELECT to_id FROM edges WHERE from_id = ?1 AND to_id IS NOT NULL")?;
            let mut rows = stmt.query(params![seed_claim_id])?;
            while let Some(row) = rows.next()? {
                neighbor_ids.push(row.get(0)?);
            }
            let mut stmt = self
                .conn
                .prepare("SELECT from_id FROM edges WHERE to_id = ?1")?;
            let mut rows = stmt.query(params![seed_claim_id])?;
            while let Some(row) = rows.next()? {
                neighbor_ids.push(row.get(0)?);
            }
            neighbor_ids.sort();
            neighbor_ids.dedup();
            if !neighbor_ids.is_empty() {
                let marks = vec!["?"; neighbor_ids.len()].join(",");
                let sql = format!(
                    "SELECT digest, claim_id FROM claims WHERE claim_id IN ({marks}) AND {scope_marks}"
                );
                let mut stmt = self.conn.prepare(&sql)?;
                let mut rows = stmt.query(params_from_iter(
                    neighbor_ids.iter().chain(scope_params.iter()),
                ))?;
                while let Some(row) = rows.next()? {
                    hops.insert((row.get(0)?, row.get(1)?));
                }
            }
            for (digest, claim_id) in hops {
                if expanded >= q.max_hops {
                    break;
                }
                if pool.iter().any(|c| c.digest == digest) {
                    continue;
                }
                let Some(row) = self.fetch_claim_row(&digest)? else {
                    continue;
                };
                if matches!(
                    row.status,
                    ValidationStatus::Superseded | ValidationStatus::Retracted
                ) {
                    continue;
                }
                pool.push(Candidate {
                    digest,
                    claim_id,
                    kind: row.kind,
                    content: row.content,
                    sensitivity: row.sensitivity,
                    status: row.status,
                    dedup_key: row.dedup_key,
                    score: seed_score * q.hop_score_ratio,
                    source_events: row.source_events,
                    contradictions: Vec::new(),
                });
                expanded += 1;
            }
        }

        // Step 4 annotation + step 8 source-backed rerank, applied to the
        // surviving pool (annotation never affects scores). Contradiction
        // edges point at the candidate: the superseded/contradicting source
        // claim's digest+text (from claims or the retracted table) travels
        // with the annotation; an edge referencing a claim outside this index
        // (cross-scope) is skipped.
        let from_map = self.claim_lookup()?;
        for cand in &mut pool {
            let edges: Vec<(String, String)> = self
                .conn
                .prepare(
                    "SELECT from_id, kind FROM edges WHERE to_id = ?1 \
                     AND kind IN ('supersedes', 'contradicts') ORDER BY rowid",
                )?
                .query_map(params![cand.claim_id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (from_id, kind) in edges {
                if let Some((digest, content)) = from_map.get(&from_id) {
                    cand.contradictions.push(Contradiction {
                        digest: digest.parse().map_err(|e| {
                            RetrievalError::InvalidInput(format!("index row {digest:?}: {e}"))
                        })?,
                        text: content.clone(),
                        supersedes: kind == "supersedes",
                    });
                }
            }
            if !cand.source_events.is_empty() {
                cand.score = (cand.score * q.evidence_boost).min(EVIDENCE_SCORE_CEILING);
            }
        }

        // Step 9 — bounded return: final authority ordering (hop candidates
        // included in the lineage dedup), truncate to max_results, map to the
        // context view.
        pool.sort_by(candidate_order);
        pool.dedup_by(|a, b| a.dedup_key == b.dedup_key);
        pool.truncate(q.max_results as usize);
        let claims = pool
            .into_iter()
            .map(|c| {
                Ok(EvidenceClaim {
                    digest: c.digest.parse().map_err(|e| {
                        RetrievalError::InvalidInput(format!("index row {:?}: {e}", c.digest))
                    })?,
                    text: c.content,
                    kind: c.kind,
                    sensitivity: c.sensitivity,
                    status: c.status,
                    score: c.score,
                    contradictions: c.contradictions,
                    source_events: c.source_events,
                })
            })
            .collect::<Result<Vec<_>, RetrievalError>>()?;

        Ok(SearchResult {
            claims,
            query_entities,
            fts_used,
            expanded,
        })
    }

    /// The full claim row for one digest.
    fn fetch_claim_row(&self, digest: &str) -> Result<Option<ClaimRow>, RetrievalError> {
        let row = self
            .conn
            .prepare(
                "SELECT claim_id, kind, content, sensitivity, status, dedup_key, source_events \
                 FROM claims WHERE digest = ?1",
            )?
            .query_row(params![digest], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, String>(5)?,
                    r.get::<_, String>(6)?,
                ))
            })
            .optional()?;
        let Some((claim_id, kind, content, sensitivity, status, dedup_key, source_events)) = row
        else {
            return Ok(None);
        };
        let status = status_parse(&status)?;
        let source_events = serde_json::from_str(&source_events).map_err(|e| {
            RetrievalError::InvalidInput(format!("claim {digest}: source_events: {e}"))
        })?;
        Ok(Some(ClaimRow {
            claim_id,
            kind,
            content,
            sensitivity,
            status,
            dedup_key,
            source_events,
        }))
    }

    /// claim_id -> (digest, content) across claims and retracted rows, for
    /// contradiction annotation.
    fn claim_lookup(&self) -> Result<HashMap<String, (String, String)>, RetrievalError> {
        let mut map = HashMap::new();
        for table in ["claims", "retracted"] {
            let sql = format!("SELECT claim_id, digest, content FROM {table}");
            let rows = self
                .conn
                .prepare(&sql)?
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (claim_id, digest, content) in rows {
                map.insert(claim_id, (digest, content));
            }
        }
        Ok(map)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{
        make_claim, make_edge, make_fold, make_promoted_claim, remove_db, scope_input, session,
        temp_db,
    };
    use kanbei_core::{Digest, Id128};
    use kanbei_memory::{Claim, ClaimEdge, EdgeKind};

    /// Builds a single-lifetime-scope index from a fold.
    fn index_with(
        claims: Vec<(Digest, Claim)>,
        edges: Vec<(Digest, ClaimEdge)>,
        retracted: Vec<(Digest, Claim)>,
    ) -> (std::path::PathBuf, MemoryIndex) {
        let path = temp_db("search");
        let mut index = MemoryIndex::open(&path).unwrap();
        let input = scope_input(MemoryScope::Lifetime, make_fold(claims, edges, retracted));
        index
            .build(std::slice::from_ref(&input), "salience-v1")
            .unwrap();
        (path, index)
    }

    /// A lifetime-scope query over the given text, defaults elsewhere.
    fn query(text: &str) -> SearchQuery {
        SearchQuery {
            text: text.to_string(),
            scopes: vec![MemoryScope::Lifetime],
            ..Default::default()
        }
    }

    #[test]
    fn fts_ranks_by_term_frequency() {
        let s = session();
        let (da, a) = make_claim(Id128::generate(), "decision", "the widget is fine", s, 1);
        let (db, b) = make_claim(
            Id128::generate(),
            "decision",
            "widget widget widget widget",
            s,
            2,
        );
        let (path, index) = index_with(vec![(da, a), (db, b)], vec![], vec![]);
        let result = index.search(&query("widget")).unwrap();
        assert!(result.fts_used);
        assert_eq!(result.claims.len(), 2);
        assert_eq!(result.claims[0].text, "widget widget widget widget");
        assert!(result.claims[0].score > result.claims[1].score);
        remove_db(&path);
    }

    #[test]
    fn malformed_fts_query_falls_back_to_like() {
        let s = session();
        let (da, a) = make_claim(Id128::generate(), "decision", "broken widget", s, 1);
        let (path, index) = index_with(vec![(da, a)], vec![], vec![]);
        // the lone quote token produces the MATCH string `"""` — an
        // unterminated FTS5 string — forcing the LIKE fallback
        let result = index.search(&query("broken \" query")).unwrap();
        assert!(!result.fts_used);
        assert_eq!(result.claims.len(), 1);
        assert_eq!(result.claims[0].text, "broken widget");
        remove_db(&path);
    }

    #[test]
    fn superseded_claims_are_filtered_and_annotate_survivors() {
        let s = session();
        let (d1, c1) = make_claim(Id128::generate(), "decision", "the widget is old", s, 1);
        let (d2, c2) = make_claim(Id128::generate(), "decision", "the widget is new", s, 2);
        let (d3, c3) = make_claim(
            Id128::generate(),
            "decision",
            "widget contradicts note",
            s,
            3,
        );
        let (de, ce) = make_edge(c1.claim_id, Some(c2.claim_id), EdgeKind::Supersedes);
        let (dc, cc) = make_edge(c3.claim_id, Some(c2.claim_id), EdgeKind::Contradicts);
        let (path, index) = index_with(
            vec![(d2, c2), (d3, c3)],
            vec![(de, ce), (dc, cc)],
            vec![(d1, c1)],
        );
        let result = index.search(&query("widget")).unwrap();
        assert_eq!(result.claims.len(), 2);
        assert!(result.claims.iter().all(|c| c.digest != d1));
        let c2res = result.claims.iter().find(|c| c.digest == d2).unwrap();
        assert_eq!(c2res.contradictions.len(), 2);
        let sup = c2res.contradictions.iter().find(|c| c.supersedes).unwrap();
        assert_eq!(sup.digest, d1);
        assert_eq!(sup.text, "the widget is old");
        let con = c2res.contradictions.iter().find(|c| !c.supersedes).unwrap();
        assert_eq!(con.digest, d3);
        assert_eq!(con.text, "widget contradicts note");
        remove_db(&path);
    }

    #[test]
    fn lineage_dedup_keeps_one_claim_per_content() {
        let s = session();
        let id1 = Id128::generate();
        let id2 = Id128::generate();
        let (d1, c1) = make_claim(id1, "decision", "the widget is fast", s, 1);
        let (d2, c2) = make_claim(id2, "decision", "the widget is fast", s, 2);
        let (path, index) = index_with(vec![(d1, c1), (d2, c2)], vec![], vec![]);
        let result = index.search(&query("widget")).unwrap();
        assert_eq!(result.claims.len(), 1);
        // equal status and score: the tie-break is claim_id ascending
        assert!(id1.to_string() < id2.to_string());
        assert_eq!(result.claims[0].digest, d1);
        remove_db(&path);
    }

    #[test]
    fn entity_hits_are_boosted_above_pure_lexical() {
        let s = session();
        let (da, a) = make_claim(
            Id128::generate(),
            "decision",
            "abs widget rs are fast",
            s,
            1,
        );
        let (db, b) = make_claim(
            Id128::generate(),
            "decision",
            "see /abs/widget.rs fix",
            s,
            2,
        );
        let (path, index) = index_with(vec![(da, a), (db, b)], vec![], vec![]);
        let mut q = query("widget /abs/widget.rs");
        q.evidence_boost = 1.0;
        q.entity_boost = 3.0;
        let result = index.search(&q).unwrap();
        assert!(
            result
                .query_entities
                .iter()
                .any(|(k, kind)| k == "/abs/widget.rs" && *kind == EntityKind::Path)
        );
        assert_eq!(result.claims.len(), 2);
        assert_eq!(result.claims[0].digest, db);
        // configurability: the same claim at boost 1.0 scores a third as much
        q.entity_boost = 1.0;
        let result_plain = index.search(&q).unwrap();
        let boosted = result.claims.iter().find(|c| c.digest == db).unwrap().score;
        let plain = result_plain
            .claims
            .iter()
            .find(|c| c.digest == db)
            .unwrap()
            .score;
        assert!((boosted - plain * 3.0).abs() < 1e-9);
        remove_db(&path);
    }

    #[test]
    fn one_hop_expansion_shares_entities_and_edges() {
        let s = session();
        let (da, a) = make_claim(Id128::generate(), "decision", "widget at /abs/x.rs", s, 1);
        let (db, b) = make_claim(Id128::generate(), "decision", "/abs/x.rs alpha", s, 2);
        let (dc, c) = make_claim(Id128::generate(), "decision", "/abs/x.rs beta", s, 3);
        let (dd, d) = make_claim(Id128::generate(), "decision", "edge neighbor", s, 4);
        let (de, e) = make_edge(a.claim_id, Some(d.claim_id), EdgeKind::EvidenceFor);
        let (path, index) = index_with(
            vec![(da, a), (db, b), (dc, c), (dd, d)],
            vec![(de, e)],
            vec![],
        );
        let mut q = query("widget");
        q.evidence_boost = 1.0;
        let result = index.search(&q).unwrap();
        assert_eq!(result.expanded, 3);
        assert_eq!(result.claims.len(), 4);
        let seed = result.claims.iter().find(|c| c.digest == da).unwrap().score;
        for (digest, expected_text) in [
            (db, "/abs/x.rs alpha"),
            (dc, "/abs/x.rs beta"),
            (dd, "edge neighbor"),
        ] {
            let c = result.claims.iter().find(|c| c.digest == digest).unwrap();
            assert!(
                (c.score - seed * 0.4).abs() < 1e-9,
                "hop score for {expected_text}"
            );
            assert_eq!(c.text, expected_text);
        }
        // the max_hops budget is respected
        let mut qb = query("widget");
        qb.evidence_boost = 1.0;
        qb.max_hops = 1;
        let result_b = index.search(&qb).unwrap();
        assert_eq!(result_b.expanded, 1);
        assert_eq!(result_b.claims.len(), 2);
        remove_db(&path);
    }

    #[test]
    fn authority_rank_orders_equal_scores() {
        let mk = |status: ValidationStatus| Candidate {
            digest: "d".into(),
            claim_id: "c".into(),
            kind: "decision".into(),
            content: "content".into(),
            sensitivity: "public".into(),
            status,
            dedup_key: "k".into(),
            score: 1.0,
            source_events: Vec::new(),
            contradictions: Vec::new(),
        };
        assert_eq!(mk(ValidationStatus::Active).rank(), 3);
        assert_eq!(mk(ValidationStatus::Approved).rank(), 2);
        assert_eq!(mk(ValidationStatus::Proposed).rank(), 1);
        assert!(
            candidate_order(
                &mk(ValidationStatus::Active),
                &mk(ValidationStatus::Proposed)
            )
            .is_lt()
        );
        // End-to-end defensive filter: a fold that keeps a claim in `claims`
        // while carrying its supersedes edge derives Superseded and is never
        // returned, even when it lexically matches.
        let s = session();
        let (dx, x) = make_claim(
            Id128::generate(),
            "decision",
            "the widget is contested",
            s,
            1,
        );
        let (dy, y) = make_claim(Id128::generate(), "decision", "the widget is final", s, 2);
        let (de, e) = make_edge(x.claim_id, Some(y.claim_id), EdgeKind::Supersedes);
        let path = temp_db("authority");
        let mut index = MemoryIndex::open(&path).unwrap();
        let input = scope_input(
            MemoryScope::Lifetime,
            make_fold(vec![(dx, x), (dy, y)], vec![(de, e)], vec![]),
        );
        index
            .build(std::slice::from_ref(&input), "salience-v1")
            .unwrap();
        let result = index.search(&query("widget")).unwrap();
        assert_eq!(result.claims.len(), 1);
        assert_eq!(result.claims[0].digest, dy);
        remove_db(&path);
    }

    #[test]
    fn source_backed_claims_rerank_above_identical_scores() {
        let s = session();
        let (dm, m) = make_claim(Id128::generate(), "decision", "widget at /abs/x.rs", s, 1);
        let (da, a) = make_claim(Id128::generate(), "decision", "/abs/x.rs alpha", s, 2);
        let (db, b) =
            make_promoted_claim(Id128::generate(), "decision", "/abs/x.rs beta", s, 3, dm);
        let (path, index) = index_with(vec![(dm, m), (da, a), (db, b)], vec![], vec![]);
        let q = query("widget");
        let result = index.search(&q).unwrap();
        let a_score = result.claims.iter().find(|c| c.digest == da).unwrap().score;
        let b_score = result.claims.iter().find(|c| c.digest == db).unwrap().score;
        assert!((b_score - a_score * 1.2).abs() < 1e-9);
        // the promoted claim carries its source event; the ordinary claim none
        let b_claim = result.claims.iter().find(|c| c.digest == db).unwrap();
        assert_eq!(b_claim.source_events, vec![3]);
        let a_claim = result.claims.iter().find(|c| c.digest == da).unwrap();
        assert!(a_claim.source_events.is_empty());
        let order: Vec<Digest> = result.claims.iter().map(|c| c.digest).collect();
        let pos = |d: Digest| order.iter().position(|x| *x == d).unwrap();
        assert!(pos(db) < pos(da));
        remove_db(&path);
    }

    #[test]
    fn max_results_truncates() {
        let s = session();
        let claims: Vec<(Digest, Claim)> = (0..20)
            .map(|i| {
                make_claim(
                    Id128::generate(),
                    "decision",
                    &format!("widget item {i}"),
                    s,
                    i as u64 + 1,
                )
            })
            .collect();
        let (path, index) = index_with(claims, vec![], vec![]);
        let mut q = query("widget");
        q.max_results = 5;
        let result = index.search(&q).unwrap();
        assert_eq!(result.claims.len(), 5);
        remove_db(&path);
    }

    #[test]
    fn empty_scopes_return_no_results() {
        let s = session();
        let (da, a) = make_claim(Id128::generate(), "decision", "widget at /abs/x.rs", s, 1);
        let (path, index) = index_with(vec![(da, a)], vec![], vec![]);
        let q = SearchQuery {
            text: "widget /abs/x.rs".to_string(),
            ..Default::default()
        };
        let result = index.search(&q).unwrap();
        assert!(result.claims.is_empty());
        assert_eq!(result.query_entities.len(), 1);
        assert!(!result.fts_used);
        remove_db(&path);
    }

    #[test]
    fn scopes_filter_claims() {
        let s = session();
        let (da, a) = make_claim(Id128::generate(), "decision", "widget in lifetime", s, 1);
        let (path, index) = index_with(vec![(da, a)], vec![], vec![]);
        let mut q = query("widget");
        q.scopes = vec![MemoryScope::Project(Id128::generate())];
        let result = index.search(&q).unwrap();
        assert!(result.claims.is_empty());
        remove_db(&path);
    }
}
