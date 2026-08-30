//! The typed staged projection pipeline (architecture.md:129-148): the kernel
//! seeds the harness/schema/trigger fragments, the kernel-owned authority
//! filter runs first, the replaceable stages build and narrow the fragment
//! list, and the kernel validator runs last. Replaceable stages may only
//! narrow (filter, drop, summarize) — never add sources beyond the input
//! views (R-05/E-03); the validator re-checks every source ref.

use serde::{Deserialize, Serialize};

use crate::error::{ProjectionError, sensitivity_rank};
use crate::fragment::{Fragment, FragmentBuilder, FragmentKind, SourceRef, StabilityClass};
use crate::input::{ProjectionInput, RenderedEvent};
use crate::validator::ValidProviderContext;

/// Pipeline state passed between stages: the input views, the fragment list
/// under construction, and the drop ledger.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Projection {
    pub input: ProjectionInput,
    /// Fragments in semantic order after all stages run.
    pub fragments: Vec<Fragment>,
    pub dropped: Vec<DropRecord>,
}

/// One dropped fragment: which stage dropped it and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DropRecord {
    pub fragment_id: String,
    pub reason: String,
    pub at_stage: String,
}

/// One projection stage. Built-in stages return `Ok(None)`; the kernel
/// validator returns `Ok(Some(vpc))`.
pub trait ProjectionStage: Send + Sync {
    fn name(&self) -> &str;

    fn apply(&self, p: &mut Projection) -> Result<Option<ValidProviderContext>, ProjectionError>;
}

/// Token estimate heuristic: `max(1, bytes / 4)`. A documented approximation
/// shared by [`BudgetStage`] and the validator's token-limit check.
pub fn estimate_tokens(text: &str) -> u64 {
    (text.len() / 4).max(1) as u64
}

/// Run one projection (architecture.md:129-148): kernel seed fragments
/// (harness contract, schemas, current trigger), the mandatory kernel-owned
/// [`AuthorityFilter`] first, the caller's replaceable stages in order, then
/// the kernel-owned `validator` last. Fragments are re-sorted into semantic
/// order (order, id) before validation — stages may append in any order.
/// Returns the `ValidProviderContext` the validator produced.
pub fn run_pipeline(
    input: ProjectionInput,
    read: &(dyn Fn(&SourceRef) -> bool + Sync),
    stages: &[Box<dyn ProjectionStage>],
    validator: &dyn ProjectionStage,
) -> Result<ValidProviderContext, ProjectionError> {
    let fragments = seed_fragments(&input)?;
    let mut p = Projection {
        input,
        fragments,
        dropped: Vec::new(),
    };
    AuthorityFilter::new(read).apply(&mut p)?;
    for stage in stages {
        stage.apply(&mut p)?;
    }
    p.fragments
        .sort_by(|a, b| (a.order, &a.id).cmp(&(b.order, &b.id)));
    match validator.apply(&mut p)? {
        Some(vpc) => Ok(vpc),
        None => Err(ProjectionError::InvalidInput(
            "validator stage produced no context".into(),
        )),
    }
}

/// Kernel-owned materialization of the input into fragments: the harness
/// contract (order 0), one schema fragment per schema (order 10+i), and the
/// current trigger (order 99). Runs before the authority filter so
/// unauthorized sources are dropped before any replaceable stage.
fn seed_fragments(input: &ProjectionInput) -> Result<Vec<Fragment>, ProjectionError> {
    let mut out = Vec::with_capacity(2 + input.schemas.len());
    out.push(
        FragmentBuilder::new(
            "harness.contract",
            0,
            FragmentKind::HarnessContract,
            StabilityClass::Static,
        )
        .content(input.harness_contract.clone())
        .sensitivity("public")
        .cache_eligible(true)
        .source_refs(vec![SourceRef::Harness])
        .build()?,
    );
    for (i, schema) in input.schemas.iter().enumerate() {
        out.push(
            FragmentBuilder::new(
                format!("schema.{}", schema.id),
                10 + i as u32,
                FragmentKind::ModuleSchema,
                StabilityClass::Static,
            )
            .content(schema.text.clone())
            .sensitivity(schema.sensitivity.clone())
            .derived_max(schema.sensitivity.clone())
            .cache_eligible(true)
            .dep_hashes(vec![schema.digest])
            .source_refs(vec![SourceRef::ModuleSchema(schema.id.clone())])
            .build()?,
        );
    }
    out.push(
        FragmentBuilder::new(
            "trigger.current",
            99,
            FragmentKind::CurrentTrigger,
            StabilityClass::TurnVolatile,
        )
        .content(input.trigger.text.clone())
        .sensitivity(input.trigger.sensitivity.clone())
        .derived_max(input.trigger.sensitivity.clone())
        .cache_eligible(false)
        .build()?,
    );
    Ok(out)
}

/// The built-in replaceable stage set, in run order:
/// [Trajectory, Cognitive, Evidence, Memory, Compression, Budget]. The
/// authority filter and validator are kernel-owned and added by
/// [`run_pipeline`] itself; config may substitute any stage here.
pub fn default_stages() -> Vec<Box<dyn ProjectionStage>> {
    vec![
        Box::new(TrajectoryStage),
        Box::new(CognitiveStage),
        Box::new(EvidenceStage),
        Box::new(MemoryStage),
        Box::new(CompressionStage),
        Box::new(BudgetStage),
    ]
}

/// Builds the trajectory fragments (architecture.md:143): one
/// `ConversationPrefix` (order 40, SessionStable, cache-eligible) covering
/// all selected ranges — content is the rendered events inside them, id
/// `conv.prefix.<min_start>.<max_end>`, event_range (min start, max end) —
/// and one `RecentEvents` (order 52, TurnVolatile) with every input event.
/// Both take their sensitivity from the max input event sensitivity
/// (derived_max over ALL input events — a documented simplification).
pub struct TrajectoryStage;

impl ProjectionStage for TrajectoryStage {
    fn name(&self) -> &str {
        "TrajectoryStage"
    }

    fn apply(&self, p: &mut Projection) -> Result<Option<ValidProviderContext>, ProjectionError> {
        let trajectory = &p.input.trajectory;
        if !trajectory.selected_ranges.is_empty() {
            let mut in_range: Vec<_> = trajectory
                .events
                .iter()
                .filter(|e| {
                    trajectory
                        .selected_ranges
                        .iter()
                        .any(|(s, x)| *s <= e.seq && e.seq <= *x)
                })
                .collect();
            in_range.sort_by_key(|e| e.seq);
            if !in_range.is_empty() {
                let (min_start, max_end) = trajectory
                    .selected_ranges
                    .iter()
                    .fold((u64::MAX, 0u64), |(lo, hi), (s, x)| {
                        (lo.min(*s), hi.max(*x))
                    });
                let sens =
                    max_sensitivity(trajectory.events.iter().map(|e| e.sensitivity.as_str()))
                        .unwrap_or_else(|| "internal".to_string());
                p.fragments.push(
                    FragmentBuilder::new(
                        format!("conv.prefix.{min_start}.{max_end}"),
                        40,
                        FragmentKind::ConversationPrefix,
                        StabilityClass::SessionStable,
                    )
                    .content(render_events(in_range.iter().copied()))
                    .sensitivity(sens.clone())
                    .derived_max(sens)
                    .event_range(Some((min_start, max_end)))
                    .cache_eligible(true)
                    .source_refs(
                        trajectory
                            .selected_ranges
                            .iter()
                            .map(|(s, x)| SourceRef::CompactionRange(*s, *x))
                            .collect(),
                    )
                    .build()?,
                );
            }
        }
        if !trajectory.events.is_empty() {
            let min = trajectory.events.iter().map(|e| e.seq).min().unwrap();
            let max = trajectory.events.iter().map(|e| e.seq).max().unwrap();
            let sens = max_sensitivity(trajectory.events.iter().map(|e| e.sensitivity.as_str()))
                .unwrap_or_else(|| "internal".to_string());
            p.fragments.push(
                FragmentBuilder::new(
                    "act.recent",
                    52,
                    FragmentKind::RecentEvents,
                    StabilityClass::TurnVolatile,
                )
                .content(render_events(trajectory.events.iter()))
                .sensitivity(sens.clone())
                .derived_max(sens)
                .event_range(Some((min, max)))
                .cache_eligible(false)
                .source_refs(
                    trajectory
                        .events
                        .iter()
                        .map(|e| SourceRef::SessionEvent(e.seq))
                        .collect(),
                )
                .build()?,
            );
        }
        Ok(None)
    }
}

/// Builds the `ActiveMemory` fragment (order 50, TurnVolatile): scorer line,
/// pins, open loops, and recent causal seqs, rendered deterministically.
/// Sensitivity/derived_max = max open-loop sensitivity (`internal` when no
/// loops). Source refs = the recent causal event seqs.
pub struct CognitiveStage;

impl ProjectionStage for CognitiveStage {
    fn name(&self) -> &str {
        "CognitiveStage"
    }

    fn apply(&self, p: &mut Projection) -> Result<Option<ValidProviderContext>, ProjectionError> {
        let active = &p.input.active;
        let mut content = String::new();
        content.push_str(&format!("scorer: {}\n", active.scorer));
        for pin in &active.pins {
            content.push_str(&format!("pin: {pin}\n"));
        }
        for open_loop in &active.open_loops {
            content.push_str(&format!(
                "open_loop: {} | {} | created={}\n",
                open_loop.id, open_loop.text, open_loop.created_event
            ));
        }
        for seq in &active.recent_causal {
            content.push_str(&format!("causal: {seq}\n"));
        }
        let sens = max_sensitivity(active.open_loops.iter().map(|l| l.sensitivity.as_str()))
            .unwrap_or_else(|| "internal".to_string());
        p.fragments.push(
            FragmentBuilder::new(
                "act.salience",
                50,
                FragmentKind::ActiveMemory,
                StabilityClass::TurnVolatile,
            )
            .content(content)
            .sensitivity(sens.clone())
            .derived_max(sens)
            .cache_eligible(false)
            .source_refs(
                active
                    .recent_causal
                    .iter()
                    .map(|e| SourceRef::SessionEvent(*e))
                    .collect(),
            )
            .build()?,
        );
        Ok(None)
    }
}

/// Builds the `RetrievedEvidence` fragment (order 51, TurnVolatile): one
/// `kind | status | score=.. | text | contradictions=[..]` line per claim;
/// contradiction texts are prefixed with `supersedes:` when they supersede
/// (R-12/M-04). Source refs = MemoryClaim per claim and per contradiction.
/// Omitted when there are no claims.
pub struct EvidenceStage;

impl ProjectionStage for EvidenceStage {
    fn name(&self) -> &str {
        "EvidenceStage"
    }

    fn apply(&self, p: &mut Projection) -> Result<Option<ValidProviderContext>, ProjectionError> {
        if p.input.evidence.claims.is_empty() {
            return Ok(None);
        }
        let mut content = String::new();
        let mut refs = Vec::new();
        for claim in &p.input.evidence.claims {
            if !content.is_empty() {
                content.push('\n');
            }
            let contradictions = claim
                .contradictions
                .iter()
                .map(|c| {
                    if c.supersedes {
                        format!("supersedes: {}", c.text)
                    } else {
                        c.text.clone()
                    }
                })
                .collect::<Vec<_>>()
                .join(", ");
            content.push_str(&format!(
                "{} | {:?} | score={:.3} | {} | contradictions=[{}]",
                claim.kind, claim.status, claim.score, claim.text, contradictions
            ));
            refs.push(SourceRef::MemoryClaim(claim.digest));
            for c in &claim.contradictions {
                refs.push(SourceRef::MemoryClaim(c.digest));
            }
        }
        let sens = max_sensitivity(
            p.input
                .evidence
                .claims
                .iter()
                .map(|c| c.sensitivity.as_str()),
        )
        .unwrap_or_else(|| "internal".to_string());
        p.fragments.push(
            FragmentBuilder::new(
                "ev.memory",
                51,
                FragmentKind::RetrievedEvidence,
                StabilityClass::TurnVolatile,
            )
            .content(content)
            .sensitivity(sens.clone())
            .derived_max(sens)
            .cache_eligible(false)
            .source_refs(refs)
            .build()?,
        );
        Ok(None)
    }
}

/// Builds the lifetime/project memory fragments (orders 20/21, ScopeStable,
/// cache-eligible): content from the fold excerpt, dep_hashes = [root],
/// source refs = MemoryClaim per embedded claim digest. Absent sources
/// produce no fragments.
pub struct MemoryStage;

impl ProjectionStage for MemoryStage {
    fn name(&self) -> &str {
        "MemoryStage"
    }

    fn apply(&self, p: &mut Projection) -> Result<Option<ValidProviderContext>, ProjectionError> {
        if let Some(lifetime) = &p.input.lifetime {
            p.fragments.push(
                FragmentBuilder::new(
                    "mem.lifetime",
                    20,
                    FragmentKind::LifetimeMemory,
                    StabilityClass::ScopeStable,
                )
                .content(lifetime.text.clone())
                .sensitivity(lifetime.sensitivity.clone())
                .derived_max(lifetime.sensitivity.clone())
                .cache_eligible(true)
                .dep_hashes(vec![lifetime.root])
                .source_refs(
                    lifetime
                        .claim_digests
                        .iter()
                        .map(|d| SourceRef::MemoryClaim(*d))
                        .collect(),
                )
                .build()?,
            );
        }
        if let Some(project) = &p.input.project {
            p.fragments.push(
                FragmentBuilder::new(
                    "mem.project",
                    21,
                    FragmentKind::ProjectMemory,
                    StabilityClass::ScopeStable,
                )
                .content(project.text.clone())
                .sensitivity(project.sensitivity.clone())
                .derived_max(project.sensitivity.clone())
                .cache_eligible(true)
                .dep_hashes(vec![project.root])
                .source_refs(
                    project
                        .claim_digests
                        .iter()
                        .map(|d| SourceRef::MemoryClaim(*d))
                        .collect(),
                )
                .build()?,
            );
        }
        Ok(None)
    }
}

/// Builds the `CompactionSummary` fragment (order 30, SessionStable,
/// cache-eligible) from the frozen compaction input (R-18/E-06); a no-op
/// when no compaction is available.
pub struct CompressionStage;

impl ProjectionStage for CompressionStage {
    fn name(&self) -> &str {
        "CompressionStage"
    }

    fn apply(&self, p: &mut Projection) -> Result<Option<ValidProviderContext>, ProjectionError> {
        if let Some(compaction) = &p.input.compaction {
            p.fragments.push(
                FragmentBuilder::new(
                    "conv.summary",
                    30,
                    FragmentKind::CompactionSummary,
                    StabilityClass::SessionStable,
                )
                .content(compaction.text.clone())
                .sensitivity(compaction.sensitivity.clone())
                .derived_max(compaction.sensitivity.clone())
                .event_range(Some(compaction.range))
                .cache_eligible(true)
                .source_refs(vec![SourceRef::CompactionRange(
                    compaction.range.0,
                    compaction.range.1,
                )])
                .build()?,
            );
        }
        Ok(None)
    }
}

/// Trims the fragment list to the budgets (architecture.md:136): drops the
/// LAST TurnVolatile fragment while over the total budget, then trims
/// volatile tokens to `max_volatile_tokens`. Stable fragments are never
/// dropped; if even the stable-only total exceeds the total budget →
/// [`ProjectionError::OverBudget`].
pub struct BudgetStage;

impl ProjectionStage for BudgetStage {
    fn name(&self) -> &str {
        "BudgetStage"
    }

    fn apply(&self, p: &mut Projection) -> Result<Option<ValidProviderContext>, ProjectionError> {
        let max_total = p.input.budgets.max_total_tokens;
        let max_volatile = p.input.budgets.max_volatile_tokens;
        let mut total: u64 = p
            .fragments
            .iter()
            .map(|f| estimate_tokens(&f.content))
            .sum();
        while total > max_total {
            let Some(i) = last_volatile(&p.fragments) else {
                return Err(ProjectionError::OverBudget {
                    needed: total,
                    budget: max_total,
                });
            };
            let f = p.fragments.remove(i);
            total -= estimate_tokens(&f.content);
            p.dropped.push(DropRecord {
                fragment_id: f.id,
                reason: "budget".into(),
                at_stage: self.name().into(),
            });
        }
        let mut volatile_total: u64 = p
            .fragments
            .iter()
            .filter(|f| f.stability == StabilityClass::TurnVolatile)
            .map(|f| estimate_tokens(&f.content))
            .sum();
        while volatile_total > max_volatile {
            let Some(i) = last_volatile(&p.fragments) else {
                break; // no volatiles left: volatile_total is 0 ≤ budget
            };
            let f = p.fragments.remove(i);
            volatile_total = volatile_total.saturating_sub(estimate_tokens(&f.content));
            p.dropped.push(DropRecord {
                fragment_id: f.id,
                reason: "budget".into(),
                at_stage: self.name().into(),
            });
        }
        Ok(None)
    }
}

/// Kernel-owned authority filter (R-05/E-03, architecture.md:148): drops any
/// fragment with a source ref the run's read capability denies, recording a
/// drop per fragment. `run_pipeline` runs it first, before any replaceable
/// stage; it is not part of [`default_stages`].
pub struct AuthorityFilter<'a> {
    read: &'a (dyn Fn(&SourceRef) -> bool + Sync),
}

impl<'a> AuthorityFilter<'a> {
    pub fn new(read: &'a (dyn Fn(&SourceRef) -> bool + Sync)) -> Self {
        Self { read }
    }
}

impl ProjectionStage for AuthorityFilter<'_> {
    fn name(&self) -> &str {
        "AuthorityFilter"
    }

    fn apply(&self, p: &mut Projection) -> Result<Option<ValidProviderContext>, ProjectionError> {
        let mut kept = Vec::with_capacity(p.fragments.len());
        for f in p.fragments.drain(..) {
            if f.source_refs.iter().all(|r| (self.read)(r)) {
                kept.push(f);
            } else {
                p.dropped.push(DropRecord {
                    fragment_id: f.id,
                    reason: "authority".into(),
                    at_stage: self.name().into(),
                });
            }
        }
        p.fragments = kept;
        Ok(None)
    }
}

/// `seq kind: text\n` lines, in the given order.
fn render_events<'a>(events: impl Iterator<Item = &'a RenderedEvent>) -> String {
    events
        .map(|e| format!("{} {}: {}\n", e.seq, e.kind, e.text))
        .collect()
}

/// The label with the highest [`sensitivity_rank`] (last one wins ties).
fn max_sensitivity<'a>(labels: impl Iterator<Item = &'a str>) -> Option<String> {
    labels
        .max_by_key(|l| sensitivity_rank(l))
        .map(str::to_string)
}

/// Index of the last TurnVolatile fragment, if any.
fn last_volatile(fragments: &[Fragment]) -> Option<usize> {
    fragments
        .iter()
        .rposition(|f| f.stability == StabilityClass::TurnVolatile)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{
        ActiveMemoryView, CompactionSummarySource, Contradiction, EvidenceClaim,
        MemoryFragmentSource, OpenLoop, RenderedEvent, RetrievedEvidence, SchemaFragment,
        TrajectoryView, TriggerFragment,
    };
    use crate::lower::lower;
    use crate::validator::ValidatorStage;
    use kanbei_core::Digest;
    use kanbei_memory::ValidationStatus;
    use kanbei_provider::{CachePlan, Role};

    #[test]
    fn estimate_tokens_never_zero() {
        assert_eq!(estimate_tokens(""), 1);
        assert_eq!(estimate_tokens("a"), 1);
        assert_eq!(estimate_tokens("hello"), 1);
        assert_eq!(estimate_tokens(&"x".repeat(8)), 2);
        assert_eq!(estimate_tokens(&"x".repeat(9)), 2);
    }

    #[test]
    fn authority_filter_drops_unauthorized_sources() {
        let read = |r: &SourceRef| !matches!(r, SourceRef::MemoryClaim(_));
        let mut p = Projection {
            input: ProjectionInput::new(""),
            fragments: vec![
                FragmentBuilder::new(
                    "ok",
                    0,
                    FragmentKind::HarnessContract,
                    StabilityClass::Static,
                )
                .content("harness")
                .source_refs(vec![SourceRef::Harness])
                .build()
                .unwrap(),
                FragmentBuilder::new(
                    "mem",
                    20,
                    FragmentKind::LifetimeMemory,
                    StabilityClass::ScopeStable,
                )
                .content("memory")
                .source_refs(vec![SourceRef::MemoryClaim(Digest::new(b"m"))])
                .build()
                .unwrap(),
            ],
            dropped: Vec::new(),
        };
        AuthorityFilter::new(&read).apply(&mut p).unwrap();
        assert_eq!(p.fragments.len(), 1);
        assert_eq!(p.fragments[0].id, "ok");
        assert_eq!(p.dropped.len(), 1);
        assert_eq!(p.dropped[0].fragment_id, "mem");
        assert_eq!(p.dropped[0].reason, "authority");
        assert_eq!(p.dropped[0].at_stage, "AuthorityFilter");
    }

    #[test]
    fn trajectory_stage_builds_prefix_and_recent() {
        let mut input = ProjectionInput::new("harness");
        input.trajectory = TrajectoryView {
            frozen_seq: 10,
            selected_ranges: vec![(1, 2)],
            selected_events: Vec::new(),
            events: vec![
                RenderedEvent {
                    seq: 1,
                    kind: "user_message".into(),
                    text: "a".into(),
                    sensitivity: "public".into(),
                },
                RenderedEvent {
                    seq: 2,
                    kind: "assistant_message".into(),
                    text: "b".into(),
                    sensitivity: "internal".into(),
                },
                RenderedEvent {
                    seq: 3,
                    kind: "user_message".into(),
                    text: "c".into(),
                    sensitivity: "secret".into(),
                },
            ],
        };
        let mut p = Projection {
            input,
            fragments: Vec::new(),
            dropped: Vec::new(),
        };
        TrajectoryStage.apply(&mut p).unwrap();
        assert_eq!(p.fragments.len(), 2);
        let conv = &p.fragments[0];
        assert_eq!(conv.id, "conv.prefix.1.2");
        assert_eq!(conv.kind, FragmentKind::ConversationPrefix);
        assert_eq!(conv.stability, StabilityClass::SessionStable);
        assert!(conv.cache_eligible);
        assert_eq!(conv.order, 40);
        assert_eq!(conv.event_range, Some((1, 2)));
        assert_eq!(conv.content, "1 user_message: a\n2 assistant_message: b\n");
        let recent = &p.fragments[1];
        assert_eq!(recent.id, "act.recent");
        assert_eq!(recent.kind, FragmentKind::RecentEvents);
        assert_eq!(recent.stability, StabilityClass::TurnVolatile);
        assert!(!recent.cache_eligible);
        assert_eq!(recent.order, 52);
        assert_eq!(recent.event_range, Some((1, 3)));
    }

    #[test]
    fn memory_stage_builds_scope_stable_fragments() {
        let mut input = ProjectionInput::new("harness");
        input.lifetime = Some(MemoryFragmentSource {
            root: Digest::new(b"lroot"),
            text: "lifetime text".into(),
            sensitivity: "internal".into(),
            claim_digests: vec![Digest::new(b"lc1"), Digest::new(b"lc2")],
        });
        input.project = Some(MemoryFragmentSource {
            root: Digest::new(b"proot"),
            text: "project text".into(),
            sensitivity: "secret".into(),
            claim_digests: Vec::new(),
        });
        let mut p = Projection {
            input,
            fragments: Vec::new(),
            dropped: Vec::new(),
        };
        MemoryStage.apply(&mut p).unwrap();
        assert_eq!(p.fragments.len(), 2);
        let lifetime = &p.fragments[0];
        assert_eq!(lifetime.id, "mem.lifetime");
        assert_eq!(lifetime.kind, FragmentKind::LifetimeMemory);
        assert_eq!(lifetime.stability, StabilityClass::ScopeStable);
        assert!(lifetime.cache_eligible);
        assert_eq!(lifetime.order, 20);
        assert_eq!(lifetime.dep_hashes, vec![Digest::new(b"lroot")]);
        assert_eq!(
            lifetime.source_refs,
            vec![
                SourceRef::MemoryClaim(Digest::new(b"lc1")),
                SourceRef::MemoryClaim(Digest::new(b"lc2")),
            ]
        );
        assert_eq!(lifetime.sensitivity, "internal");
        let project = &p.fragments[1];
        assert_eq!(project.id, "mem.project");
        assert_eq!(project.order, 21);
        assert_eq!(project.dep_hashes, vec![Digest::new(b"proot")]);
        assert!(project.source_refs.is_empty());
        assert_eq!(project.sensitivity, "secret");
    }

    #[test]
    fn memory_stage_skips_absent_sources() {
        let mut p = Projection {
            input: ProjectionInput::new("harness"),
            fragments: Vec::new(),
            dropped: Vec::new(),
        };
        MemoryStage.apply(&mut p).unwrap();
        assert!(p.fragments.is_empty());
    }

    fn token_frag(id: &str, order: u32, stability: StabilityClass, bytes: usize) -> Fragment {
        FragmentBuilder::new(id, order, FragmentKind::ConversationPrefix, stability)
            .content("x".repeat(bytes))
            .cache_eligible(true)
            .build()
            .unwrap()
    }

    #[test]
    fn budget_stage_drops_last_volatile_first() {
        let mut input = ProjectionInput::new("harness");
        input.budgets.max_total_tokens = 5;
        input.budgets.max_volatile_tokens = 100;
        let mut p = Projection {
            input,
            fragments: vec![
                token_frag("stable", 0, StabilityClass::Static, 8),
                token_frag("v1", 50, StabilityClass::TurnVolatile, 8),
                token_frag("v2", 52, StabilityClass::TurnVolatile, 8),
            ],
            dropped: Vec::new(),
        };
        BudgetStage.apply(&mut p).unwrap();
        let ids: Vec<&str> = p.fragments.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, vec!["stable", "v1"]);
        assert_eq!(p.dropped.len(), 1);
        assert_eq!(p.dropped[0].fragment_id, "v2");
        assert_eq!(p.dropped[0].reason, "budget");
        assert_eq!(p.dropped[0].at_stage, "BudgetStage");
    }

    #[test]
    fn budget_stage_never_drops_stable_and_reports_overbudget() {
        let mut input = ProjectionInput::new("harness");
        input.budgets.max_total_tokens = 2;
        let mut p = Projection {
            input,
            fragments: vec![token_frag("stable", 0, StabilityClass::Static, 16)],
            dropped: Vec::new(),
        };
        let err = BudgetStage.apply(&mut p).unwrap_err();
        assert!(matches!(
            err,
            ProjectionError::OverBudget {
                needed: 4,
                budget: 2,
            }
        ));
        assert_eq!(p.fragments.len(), 1); // stable never dropped
    }

    #[test]
    fn budget_stage_trims_volatile_tokens_separately() {
        let mut input = ProjectionInput::new("harness");
        input.budgets.max_total_tokens = 100;
        input.budgets.max_volatile_tokens = 2;
        let mut p = Projection {
            input,
            fragments: vec![
                token_frag("stable", 0, StabilityClass::Static, 8),
                token_frag("v1", 50, StabilityClass::TurnVolatile, 8),
                token_frag("v2", 52, StabilityClass::TurnVolatile, 8),
            ],
            dropped: Vec::new(),
        };
        BudgetStage.apply(&mut p).unwrap();
        let ids: Vec<&str> = p.fragments.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, vec!["stable", "v1"]);
    }

    fn full_input() -> ProjectionInput {
        let mut input = ProjectionInput::new("You are Kanbei. Follow the harness.");
        input.schemas = vec![
            SchemaFragment {
                id: "tools".into(),
                digest: Digest::new(b"tools"),
                text: "tool schemas".into(),
                sensitivity: "public".into(),
            },
            SchemaFragment {
                id: "modules".into(),
                digest: Digest::new(b"modules"),
                text: "module schemas".into(),
                sensitivity: "internal".into(),
            },
        ];
        input.lifetime = Some(MemoryFragmentSource {
            root: Digest::new(b"lroot"),
            text: "lifetime memory".into(),
            sensitivity: "internal".into(),
            claim_digests: vec![Digest::new(b"lc")],
        });
        input.project = Some(MemoryFragmentSource {
            root: Digest::new(b"proot"),
            text: "project memory".into(),
            sensitivity: "secret".into(),
            claim_digests: Vec::new(),
        });
        input.compaction = Some(CompactionSummarySource {
            range: (1, 4),
            text: "compaction summary".into(),
            sensitivity: "internal".into(),
        });
        input.trajectory = TrajectoryView {
            frozen_seq: 10,
            selected_ranges: vec![(1, 4)],
            selected_events: Vec::new(),
            events: vec![
                RenderedEvent {
                    seq: 1,
                    kind: "user_message".into(),
                    text: "hello".into(),
                    sensitivity: "public".into(),
                },
                RenderedEvent {
                    seq: 2,
                    kind: "assistant_message".into(),
                    text: "hi".into(),
                    sensitivity: "internal".into(),
                },
                RenderedEvent {
                    seq: 3,
                    kind: "user_message".into(),
                    text: "more".into(),
                    sensitivity: "secret".into(),
                },
            ],
        };
        input.active = ActiveMemoryView {
            scorer: "salience-v1".into(),
            pins: vec![Digest::new(b"pin")],
            open_loops: vec![OpenLoop {
                id: "l1".into(),
                text: "follow up".into(),
                created_event: 2,
                sensitivity: "internal".into(),
            }],
            recent_causal: vec![3],
        };
        input.evidence = RetrievedEvidence {
            claims: vec![EvidenceClaim {
                digest: Digest::new(b"claim1"),
                text: "user prefers X".into(),
                kind: "preference".into(),
                sensitivity: "secret".into(),
                status: ValidationStatus::Active,
                score: 0.9,
                contradictions: vec![Contradiction {
                    digest: Digest::new(b"c2"),
                    text: "later says not X".into(),
                    supersedes: true,
                }],
                source_events: vec![3],
            }],
        };
        input.trigger = TriggerFragment {
            kind: "user_message".into(),
            text: "continue".into(),
            sensitivity: "public".into(),
        };
        input
    }

    #[test]
    fn end_to_end_pipeline_and_lowering() {
        let read = |_: &SourceRef| true;
        let validator = ValidatorStage::new(read);
        let stages = default_stages();

        let vpc = run_pipeline(full_input(), &read, &stages, &validator).unwrap();
        let ids: Vec<&str> = vpc.fragments.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(
            ids,
            vec![
                "harness.contract",
                "schema.tools",
                "schema.modules",
                "mem.lifetime",
                "mem.project",
                "conv.summary",
                "conv.prefix.1.4",
                "act.salience",
                "ev.memory",
                "act.recent",
                "trigger.current",
            ]
        );
        assert_eq!(
            vpc.memory_roots,
            (Some(Digest::new(b"lroot")), Some(Digest::new(b"proot")))
        );
        assert_eq!(vpc.event_ranges, vec![(1, 4), (1, 4), (1, 3)]);
        assert_eq!(vpc.selected_events, vec![1, 2, 3, 4]);
        assert!(vpc.total_tokens > 0);
        assert!(vpc.dropped.is_empty());

        // projection digest deterministic; changes when evidence changes
        let again = run_pipeline(full_input(), &read, &stages, &validator).unwrap();
        assert_eq!(again.projection_digest, vpc.projection_digest);
        let mut changed = full_input();
        changed.evidence.claims[0].text = "user prefers Y".into();
        let changed_vpc = run_pipeline(changed, &read, &stages, &validator).unwrap();
        assert_ne!(changed_vpc.projection_digest, vpc.projection_digest);

        // lowering: prefix = harness+schema+memory+compaction+conversation,
        // tail = active+evidence+recent+trigger
        let low = lower(&vpc, true).unwrap();
        assert_eq!(low.messages.len(), vpc.fragments.len());
        for (i, m) in low.messages.iter().enumerate() {
            let expect_system = i < 7;
            assert_eq!(
                m.role,
                if expect_system {
                    Role::System
                } else {
                    Role::User
                }
            );
            assert_eq!(m.content, vpc.fragments[i].content);
            assert_eq!(m.tool_call_id, None);
        }
        assert!(matches!(low.cache_plan, CachePlan::StablePrefix { .. }));
    }

    struct DropEvidence;

    impl ProjectionStage for DropEvidence {
        fn name(&self) -> &str {
            "DropEvidence"
        }

        fn apply(
            &self,
            p: &mut Projection,
        ) -> Result<Option<ValidProviderContext>, ProjectionError> {
            let dropped_ids: Vec<String> = p
                .fragments
                .iter()
                .filter(|f| f.kind == FragmentKind::RetrievedEvidence)
                .map(|f| f.id.clone())
                .collect();
            p.fragments
                .retain(|f| f.kind != FragmentKind::RetrievedEvidence);
            for id in dropped_ids {
                p.dropped.push(DropRecord {
                    fragment_id: id,
                    reason: "narrowing".into(),
                    at_stage: self.name().into(),
                });
            }
            Ok(None)
        }
    }

    #[test]
    fn custom_stage_may_narrow() {
        let read = |_: &SourceRef| true;
        let validator = ValidatorStage::new(read);
        let stages: Vec<Box<dyn ProjectionStage>> = vec![
            Box::new(TrajectoryStage),
            Box::new(CognitiveStage),
            Box::new(EvidenceStage),
            Box::new(MemoryStage),
            Box::new(DropEvidence),
            Box::new(CompressionStage),
            Box::new(BudgetStage),
        ];
        let vpc = run_pipeline(full_input(), &read, &stages, &validator).unwrap();
        assert!(
            vpc.fragments
                .iter()
                .all(|f| f.kind != FragmentKind::RetrievedEvidence)
        );
        assert!(vpc.dropped.iter().any(|d| d.at_stage == "DropEvidence"));
        let kinds: Vec<FragmentKind> = vpc.fragments.iter().map(|f| f.kind).collect();
        assert!(kinds.contains(&FragmentKind::HarnessContract));
        assert!(kinds.contains(&FragmentKind::ConversationPrefix));
        assert!(kinds.contains(&FragmentKind::CurrentTrigger));
    }

    struct AddUnauthorized;

    impl ProjectionStage for AddUnauthorized {
        fn name(&self) -> &str {
            "AddUnauthorized"
        }

        fn apply(
            &self,
            p: &mut Projection,
        ) -> Result<Option<ValidProviderContext>, ProjectionError> {
            p.fragments.push(
                FragmentBuilder::new(
                    "sneaky",
                    60,
                    FragmentKind::RetrievedEvidence,
                    StabilityClass::TurnVolatile,
                )
                .content("sneaky content")
                .source_refs(vec![SourceRef::MemoryClaim(Digest::new(b"nope"))])
                .build()
                .unwrap(),
            );
            Ok(None)
        }
    }

    #[test]
    fn custom_stage_cannot_add_unauthorized_sources() {
        let read = |r: &SourceRef| matches!(r, SourceRef::Harness);
        let validator = ValidatorStage::new(read);
        let stages: Vec<Box<dyn ProjectionStage>> = vec![Box::new(AddUnauthorized)];
        let mut input = ProjectionInput::new("harness");
        input.trigger = TriggerFragment {
            kind: "user_message".into(),
            text: "go".into(),
            sensitivity: "public".into(),
        };
        let err = run_pipeline(input, &read, &stages, &validator).unwrap_err();
        assert!(matches!(err, ProjectionError::AuthorityDenied(id) if id == "sneaky"));
    }
}
