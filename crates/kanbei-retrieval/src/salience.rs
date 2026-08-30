//! The deterministic active-memory projector (architecture.md "Active-memory
//! default"): a configurable weighted combination of causal recency, repeated
//! use, unresolved goals, pins, and bounded graph reachability. Activation
//! scores are disposable projections (R-12/F-S5): [`ActiveMemoryProjector::project`]
//! writes SQLite activation-log rows — never session-stream events. The
//! scoring module and version are recorded for replay and evaluation.

use std::collections::{HashMap, HashSet};

use kanbei_context::{ActiveMemoryView, OpenLoop};
use kanbei_core::Digest;
use kanbei_memory::{Claim, RootFold};

use crate::entities::normalize_query;
use crate::error::RetrievalError;
use crate::index::MemoryIndex;

/// The scoring module version, recorded in the activation log and the view.
pub const SALIENCE_VERSION: &str = "salience-v1";

/// The recency half-window: claims whose newest provenance event is more than
/// 512 causal seqs behind the frozen frontier score zero recency.
const RECENCY_WINDOW: f64 = 512.0;

/// The graph-reachability cap (visited claims).
const GRAPH_CAP: usize = 64;

/// The recommended default `top_n` (callers may override).
pub const DEFAULT_TOP_N: usize = 32;

/// The component weights; the default sums to 1.0.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SalienceWeights {
    pub recency: f64,
    pub usage: f64,
    pub goals: f64,
    pub pins: f64,
    pub graph: f64,
}

impl Default for SalienceWeights {
    fn default() -> Self {
        Self {
            recency: 0.35,
            usage: 0.15,
            goals: 0.25,
            pins: 0.15,
            graph: 0.10,
        }
    }
}

/// One salience projection input.
#[derive(Debug, Clone)]
pub struct SalienceInput {
    /// The frozen committed event seq at projection time.
    pub frozen_seq: u64,
    /// Event seqs of recent causal edges (passed through to the view).
    pub recent_causal: Vec<u64>,
    pub open_loops: Vec<OpenLoop>,
    /// Claim digests the run pinned.
    pub pins: Vec<Digest>,
    /// The active fold whose claims are scored.
    pub fold: RootFold,
    /// How many top claims to keep (recommended [`DEFAULT_TOP_N`]).
    pub top_n: usize,
}

/// The per-component salience breakdown of one scored claim.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SalienceBreakdown {
    pub recency: f64,
    pub usage: f64,
    pub goals: f64,
    pub pins: f64,
    pub graph: f64,
}

/// One scored active claim.
#[derive(Debug, Clone, PartialEq)]
pub struct ScoredClaim {
    pub digest: Digest,
    pub claim: Claim,
    pub score: f64,
    pub components: SalienceBreakdown,
}

/// The deterministic, versioned active-memory projector.
#[derive(Debug, Clone)]
pub struct ActiveMemoryProjector {
    pub weights: SalienceWeights,
    pub version: String,
}

impl ActiveMemoryProjector {
    pub fn new() -> Self {
        Self {
            weights: SalienceWeights::default(),
            version: SALIENCE_VERSION.to_string(),
        }
    }

    /// Pure deterministic scoring of every active fold claim (no I/O):
    ///
    /// - recency: `1 - age/512` clamped, `age = frozen_seq - event`; an event
    ///   at seq 0 is the "no event" sentinel and scores 0.
    /// - usage: incident fold edges / the maximum incident count across
    ///   claims (0 when the fold has no edges).
    /// - goals: 1.0 when the claim shares an alnum token with any open loop.
    /// - pins: 1.0 when the claim digest is pinned.
    /// - graph: the fraction of the claim set reachable within two edge hops,
    ///   bounded at 64 visited claims.
    ///
    /// Sorted by score desc, tie-break claim_id ascending, truncated to
    /// `top_n`.
    pub fn score(&self, input: &SalienceInput) -> Vec<ScoredClaim> {
        if input.fold.claims.is_empty() {
            return Vec::new();
        }
        let mut incident: HashMap<String, u64> = HashMap::new();
        for (_, edge) in &input.fold.edges {
            *incident.entry(edge.from.to_string()).or_default() += 1;
            if let Some(to) = edge.to {
                *incident.entry(to.to_string()).or_default() += 1;
            }
        }
        let max_incident = input
            .fold
            .claims
            .iter()
            .map(|(_, c)| incident.get(&c.claim_id.to_string()).copied().unwrap_or(0))
            .max()
            .unwrap_or(0);
        let loop_tokens: Vec<HashSet<String>> = input
            .open_loops
            .iter()
            .map(|l| alnum_tokens(&l.text))
            .collect();
        let pins: HashSet<Digest> = input.pins.iter().copied().collect();
        let graph_denom = input.fold.claims.len().min(GRAPH_CAP) as f64;

        let mut scored: Vec<ScoredClaim> = Vec::with_capacity(input.fold.claims.len());
        for (digest, claim) in &input.fold.claims {
            let claim_id = claim.claim_id.to_string();
            let recency = recency(input.frozen_seq, claim.provenance.event);
            let usage = if max_incident == 0 {
                0.0
            } else {
                incident.get(&claim_id).copied().unwrap_or(0) as f64 / max_incident as f64
            };
            let goals = if goals_overlap(&loop_tokens, &alnum_tokens(&claim.content)) {
                1.0
            } else {
                0.0
            };
            let pins = if pins.contains(digest) { 1.0 } else { 0.0 };
            let graph = reachability(&input.fold, &claim_id, graph_denom);
            let components = SalienceBreakdown {
                recency,
                usage,
                goals,
                pins,
                graph,
            };
            let score = self.weights.recency * recency
                + self.weights.usage * usage
                + self.weights.goals * goals
                + self.weights.pins * pins
                + self.weights.graph * graph;
            scored.push(ScoredClaim {
                digest: *digest,
                claim: claim.clone(),
                score,
                components,
            });
        }
        scored.sort_by(|a, b| {
            b.score.total_cmp(&a.score).then_with(|| {
                a.claim
                    .claim_id
                    .to_string()
                    .cmp(&b.claim.claim_id.to_string())
            })
        });
        scored.truncate(input.top_n);
        scored
    }

    /// Scores the fold, writes disposable activation-log rows (claim digest,
    /// score, this projector's version, `frozen_seq`), and returns the
    /// [`ActiveMemoryView`] plus the top scored claims (a later session wave
    /// turns them into an evidence/activated-claims fragment).
    pub fn project(
        &self,
        input: &SalienceInput,
        index: &mut MemoryIndex,
    ) -> Result<(ActiveMemoryView, Vec<ScoredClaim>), RetrievalError> {
        let scored = self.score(input);
        let rows: Vec<(String, f64, String, u64)> = scored
            .iter()
            .map(|s| {
                (
                    s.digest.to_string(),
                    s.score,
                    self.version.clone(),
                    input.frozen_seq,
                )
            })
            .collect();
        index.write_activation_rows(&rows)?;
        let view = ActiveMemoryView {
            scorer: self.version.clone(),
            pins: input.pins.clone(),
            open_loops: input.open_loops.clone(),
            recent_causal: input.recent_causal.clone(),
        };
        Ok((view, scored))
    }
}

impl Default for ActiveMemoryProjector {
    fn default() -> Self {
        Self::new()
    }
}

/// Recency: 1.0 at the frozen frontier, linear decay over [`RECENCY_WINDOW`]
/// causal seqs, 0.0 beyond. Event seq 0 is the "no event" sentinel.
fn recency(frozen_seq: u64, event: u64) -> f64 {
    if event == 0 {
        return 0.0;
    }
    let age = frozen_seq.saturating_sub(event);
    (1.0 - age as f64 / RECENCY_WINDOW).clamp(0.0, 1.0)
}

/// Goals: 1.0 when the claim shares an alnum token with any open loop.
fn goals_overlap(loop_tokens: &[HashSet<String>], claim_tokens: &HashSet<String>) -> bool {
    if claim_tokens.is_empty() {
        return false;
    }
    loop_tokens
        .iter()
        .any(|lt| !lt.is_empty() && lt.iter().any(|t| claim_tokens.contains(t)))
}

/// Lowercased alnum tokens (normalize_query + alnum filtering).
fn alnum_tokens(text: &str) -> HashSet<String> {
    normalize_query(text)
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
        })
        .filter(|w| !w.is_empty())
        .collect()
}

/// Bounded 2-hop reachability: the fraction of the claim set reachable from
/// the claim within two edge hops, capped at [`GRAPH_CAP`] visited claims
/// (the start claim itself is not counted). Deterministic BFS over fold-edge
/// order.
fn reachability(fold: &RootFold, claim_id: &str, denom: f64) -> f64 {
    let mut adjacency: HashMap<String, Vec<String>> = HashMap::new();
    for (_, edge) in &fold.edges {
        let from = edge.from.to_string();
        if let Some(to) = edge.to {
            let to = to.to_string();
            adjacency.entry(from.clone()).or_default().push(to.clone());
            adjacency.entry(to).or_default().push(from);
        }
    }
    let mut visited: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::from([claim_id.to_string()]);
    let mut frontier: Vec<String> = vec![claim_id.to_string()];
    for _ in 0..2 {
        if visited.len() >= GRAPH_CAP {
            break;
        }
        let mut next: Vec<String> = Vec::new();
        for node in &frontier {
            if visited.len() >= GRAPH_CAP {
                break;
            }
            if let Some(neighbors) = adjacency.get(node) {
                for neighbor in neighbors {
                    if visited.len() >= GRAPH_CAP {
                        break;
                    }
                    if seen.insert(neighbor.clone()) {
                        visited.push(neighbor.clone());
                        next.push(neighbor.clone());
                    }
                }
            }
        }
        frontier = next;
    }
    visited.len() as f64 / denom
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::{
        make_claim, make_edge, make_fold, remove_db, scope_input, session, temp_db,
    };
    use kanbei_core::Id128;
    use kanbei_memory::{EdgeKind, MemoryScope};

    fn input(
        fold: RootFold,
        frozen_seq: u64,
        open_loops: Vec<OpenLoop>,
        pins: Vec<Digest>,
        top_n: usize,
    ) -> SalienceInput {
        SalienceInput {
            frozen_seq,
            recent_causal: Vec::new(),
            open_loops,
            pins,
            fold,
            top_n,
        }
    }

    #[test]
    fn scoring_is_deterministic() {
        let s = session();
        let (d1, c1) = make_claim(Id128::generate(), "decision", "widget alpha", s, 100);
        let (d2, c2) = make_claim(Id128::generate(), "decision", "widget beta", s, 200);
        let (d3, c3) = make_claim(Id128::generate(), "preference", "prefer gamma", s, 300);
        let fold = make_fold(vec![(d1, c1), (d2, c2), (d3, c3)], vec![], vec![]);
        let inp = input(fold, 512, Vec::new(), Vec::new(), 8);
        let projector = ActiveMemoryProjector::new();
        let a = projector.score(&inp);
        let b = projector.score(&inp);
        assert_eq!(a, b);
        assert_eq!(a.len(), 3);
    }

    #[test]
    fn recency_decays_with_event_age() {
        let s = session();
        let (d1, c1) = make_claim(Id128::generate(), "decision", "recent widget", s, 1024);
        let (d2, c2) = make_claim(Id128::generate(), "decision", "old widget", s, 512);
        let (d3, c3) = make_claim(Id128::generate(), "decision", "no-event widget", s, 0);
        let fold = make_fold(vec![(d1, c1), (d2, c2), (d3, c3)], vec![], vec![]);
        let inp = input(fold, 1024, Vec::new(), Vec::new(), 8);
        let projector = ActiveMemoryProjector {
            weights: SalienceWeights {
                recency: 1.0,
                usage: 0.0,
                goals: 0.0,
                pins: 0.0,
                graph: 0.0,
            },
            version: SALIENCE_VERSION.to_string(),
        };
        let scored = projector.score(&inp);
        let by = |d: Digest| scored.iter().find(|x| x.digest == d).unwrap();
        assert!((by(d1).score - 1.0).abs() < 1e-12);
        assert!((by(d2).score - 0.0).abs() < 1e-12);
        assert!((by(d3).score - 0.0).abs() < 1e-12);
        assert!((by(d1).components.recency - 1.0).abs() < 1e-12);
        assert!((by(d2).components.recency - 0.0).abs() < 1e-12);
    }

    #[test]
    fn pins_boost_pinned_claims() {
        let s = session();
        let (d1, c1) = make_claim(Id128::generate(), "decision", "widget alpha", s, 1);
        let (d2, c2) = make_claim(Id128::generate(), "decision", "widget beta", s, 2);
        let fold = make_fold(vec![(d1, c1), (d2, c2)], vec![], vec![]);
        let inp = input(fold, 100, Vec::new(), vec![d1], 8);
        let projector = ActiveMemoryProjector {
            weights: SalienceWeights {
                recency: 0.0,
                usage: 0.0,
                goals: 0.0,
                pins: 1.0,
                graph: 0.0,
            },
            version: SALIENCE_VERSION.to_string(),
        };
        let scored = projector.score(&inp);
        let by = |d: Digest| scored.iter().find(|x| x.digest == d).unwrap();
        assert!((by(d1).score - 1.0).abs() < 1e-12);
        assert!((by(d2).score - 0.0).abs() < 1e-12);
        assert!((by(d1).components.pins - 1.0).abs() < 1e-12);
    }

    #[test]
    fn goals_fire_on_open_loop_token_overlap() {
        let s = session();
        let (d1, c1) = make_claim(Id128::generate(), "decision", "the widget is broken", s, 1);
        let (d2, c2) = make_claim(Id128::generate(), "decision", "unrelated note", s, 2);
        let fold = make_fold(vec![(d1, c1), (d2, c2)], vec![], vec![]);
        let loops = vec![OpenLoop {
            id: "ol-1".into(),
            text: "fix the widget build".into(),
            created_event: 1,
            sensitivity: "public".into(),
        }];
        let inp = input(fold, 100, loops, Vec::new(), 8);
        let projector = ActiveMemoryProjector {
            weights: SalienceWeights {
                recency: 0.0,
                usage: 0.0,
                goals: 1.0,
                pins: 0.0,
                graph: 0.0,
            },
            version: SALIENCE_VERSION.to_string(),
        };
        let scored = projector.score(&inp);
        let by = |d: Digest| scored.iter().find(|x| x.digest == d).unwrap();
        assert!((by(d1).score - 1.0).abs() < 1e-12);
        assert!((by(d2).score - 0.0).abs() < 1e-12);
    }

    #[test]
    fn usage_scales_with_incident_edges() {
        let s = session();
        let (da, a) = make_claim(Id128::generate(), "decision", "hub widget", s, 1);
        let (db, b) = make_claim(Id128::generate(), "decision", "spoke one", s, 2);
        let (dc, c) = make_claim(Id128::generate(), "decision", "spoke two", s, 3);
        let (e1, ce1) = make_edge(a.claim_id, Some(b.claim_id), EdgeKind::EvidenceFor);
        let (e2, ce2) = make_edge(a.claim_id, Some(c.claim_id), EdgeKind::Supports);
        let fold = make_fold(
            vec![(da, a), (db, b), (dc, c)],
            vec![(e1, ce1), (e2, ce2)],
            vec![],
        );
        let inp = input(fold, 100, Vec::new(), Vec::new(), 8);
        let projector = ActiveMemoryProjector {
            weights: SalienceWeights {
                recency: 0.0,
                usage: 1.0,
                goals: 0.0,
                pins: 0.0,
                graph: 0.0,
            },
            version: SALIENCE_VERSION.to_string(),
        };
        let scored = projector.score(&inp);
        let by = |d: Digest| scored.iter().find(|x| x.digest == d).unwrap();
        assert!((by(da).score - 1.0).abs() < 1e-12);
        assert!((by(db).score - 0.5).abs() < 1e-12);
        assert!((by(dc).score - 0.5).abs() < 1e-12);
    }

    #[test]
    fn graph_reachability_counts_two_hop_neighbors() {
        let s = session();
        let (da, a) = make_claim(Id128::generate(), "decision", "node a", s, 1);
        let (db, b) = make_claim(Id128::generate(), "decision", "node b", s, 2);
        let (dc, c) = make_claim(Id128::generate(), "decision", "node c", s, 3);
        let (e1, ce1) = make_edge(a.claim_id, Some(b.claim_id), EdgeKind::EvidenceFor);
        let (e2, ce2) = make_edge(b.claim_id, Some(c.claim_id), EdgeKind::EvidenceFor);
        let fold = make_fold(
            vec![(da, a), (db, b), (dc, c)],
            vec![(e1, ce1), (e2, ce2)],
            vec![],
        );
        let inp = input(fold, 100, Vec::new(), Vec::new(), 8);
        let projector = ActiveMemoryProjector {
            weights: SalienceWeights {
                recency: 0.0,
                usage: 0.0,
                goals: 0.0,
                pins: 0.0,
                graph: 1.0,
            },
            version: SALIENCE_VERSION.to_string(),
        };
        let scored = projector.score(&inp);
        let by = |d: Digest| scored.iter().find(|x| x.digest == d).unwrap();
        // a reaches {b, c} within two hops: 2/3; c reaches {b, a}: 2/3
        assert!((by(da).components.graph - 2.0 / 3.0).abs() < 1e-12);
        assert!((by(dc).components.graph - 2.0 / 3.0).abs() < 1e-12);
        // b reaches {a, c}: 2/3 as well
        assert!((by(db).components.graph - 2.0 / 3.0).abs() < 1e-12);
    }

    #[test]
    fn top_n_truncates() {
        let s = session();
        let claims: Vec<(Digest, Claim)> = (0..5)
            .map(|i| {
                make_claim(
                    Id128::generate(),
                    "decision",
                    &format!("widget {i}"),
                    s,
                    i as u64 + 1,
                )
            })
            .collect();
        let fold = make_fold(claims, vec![], vec![]);
        let inp = input(fold, 100, Vec::new(), Vec::new(), 2);
        let scored = ActiveMemoryProjector::new().score(&inp);
        assert_eq!(scored.len(), 2);
    }

    #[test]
    fn project_writes_activation_log_and_returns_view() {
        let s = session();
        let (da, a) = make_claim(Id128::generate(), "decision", "widget alpha", s, 100);
        let (db, b) = make_claim(Id128::generate(), "decision", "widget beta", s, 200);
        let path = temp_db("project");
        let mut index = MemoryIndex::open(&path).unwrap();
        let fold = make_fold(vec![(da, a), (db, b)], vec![], vec![]);
        let input = scope_input(MemoryScope::Lifetime, fold.clone());
        index
            .build(std::slice::from_ref(&input), SALIENCE_VERSION)
            .unwrap();
        let projector = ActiveMemoryProjector::new();
        let pins = vec![db];
        let sal = SalienceInput {
            frozen_seq: 512,
            recent_causal: vec![500],
            open_loops: vec![OpenLoop {
                id: "ol-1".into(),
                text: "fix the widget".into(),
                created_event: 300,
                sensitivity: "public".into(),
            }],
            pins: pins.clone(),
            fold,
            top_n: 8,
        };
        let (view, scored) = projector.project(&sal, &mut index).unwrap();
        assert_eq!(view.scorer, SALIENCE_VERSION);
        assert_eq!(view.pins, pins);
        assert_eq!(view.recent_causal, vec![500]);
        assert_eq!(view.open_loops.len(), 1);
        assert_eq!(scored.len(), 2);
        // pinned claim leads
        assert_eq!(scored[0].digest, db);
        assert_eq!(scored, projector.score(&sal));
        // activation log rows carry score, scorer version, frozen seq
        let rows = index.activation_rows(&db).unwrap();
        assert_eq!(rows.len(), 1);
        assert!((rows[0].0 - scored[0].score).abs() < 1e-12);
        assert_eq!(rows[0].1, SALIENCE_VERSION.to_string());
        assert_eq!(rows[0].2, 512);
        // a rebuild clears the disposable activation log
        index
            .build(
                std::slice::from_ref(&scope_input(
                    MemoryScope::Lifetime,
                    make_fold(vec![], vec![], vec![]),
                )),
                SALIENCE_VERSION,
            )
            .unwrap();
        assert!(index.activation_rows(&db).unwrap().is_empty());
        remove_db(&path);
    }
}
