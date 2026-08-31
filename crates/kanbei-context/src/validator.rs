//! The kernel-owned final validator (architecture.md:139, 148-151): source
//! authority re-check, E-14 sensitivity, A-06 chronology, opaque-artifact
//! ban, token budget, and semantic ordering. Produces the
//! [`ValidProviderContext`] the session commits with (R-08/E-13).

use kanbei_core::Digest;
use serde::{Deserialize, Serialize};

use crate::error::{ProjectionError, sensitivity_rank};
use crate::fragment::{Fragment, FragmentKind, SourceRef, StabilityClass};
use crate::pipeline::{DropRecord, Projection, ProjectionStage, estimate_tokens};

/// The validated projection: ordered fragments plus the derived record
/// fields the session commits (architecture.md:141, R-08/E-13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ValidProviderContext {
    /// Fragments in semantic order.
    pub fragments: Vec<Fragment>,
    /// Fragment-list digest (R-08/E-13): blake3 over the canonical ordered
    /// list of (id, kind, stability, content_hash, dep_hashes, event_range,
    /// cache_eligible).
    pub projection_digest: Digest,
    pub total_tokens: u64,
    /// Sorted, deduped union of SessionEvent refs and event-range seqs.
    pub selected_events: Vec<u64>,
    pub event_ranges: Vec<(u64, u64)>,
    /// (lifetime, project) memory roots — from the MemoryStage fragments'
    /// dep_hashes[0]; `None` when absent. Pins for model-call
    /// records/snapshots.
    pub memory_roots: (Option<Digest>, Option<Digest>),
    pub dropped: Vec<DropRecord>,
}

/// The kernel-owned validator stage. `read` is the run's trajectory/memory
/// read capability; `apply` re-checks it (E-03) so custom stages cannot add
/// unauthorized sources.
pub struct ValidatorStage {
    read: Box<dyn Fn(&SourceRef) -> bool + Sync + Send>,
}

impl ValidatorStage {
    pub fn new(read: impl Fn(&SourceRef) -> bool + Sync + Send + 'static) -> Self {
        Self {
            read: Box::new(read),
        }
    }
}

impl ProjectionStage for ValidatorStage {
    fn name(&self) -> &str {
        "ValidatorStage"
    }

    fn apply(&self, p: &mut Projection) -> Result<Option<ValidProviderContext>, ProjectionError> {
        // 1. authority re-check (E-03, architecture.md:148)
        for f in &p.fragments {
            if !f.source_refs.iter().all(|r| (self.read)(r)) {
                return Err(ProjectionError::AuthorityDenied(f.id.clone()));
            }
        }
        // 2. sensitivity non-escalation (E-14, architecture.md:149) — the
        //    builder enforced it; the validator re-checks so custom stages
        //    cannot bypass it.
        for f in &p.fragments {
            if let Some(derived_max) = &f.derived_max
                && sensitivity_rank(&f.sensitivity) < sensitivity_rank(derived_max)
            {
                return Err(ProjectionError::SensitivityViolation(
                    f.id.clone(),
                    f.sensitivity.clone(),
                    derived_max.clone(),
                ));
            }
        }
        // 3. chronology (A-06, architecture.md:151): any event beyond the
        //    frozen prefix forces the fragment to be TurnVolatile.
        let frozen_seq = p.input.trajectory.frozen_seq;
        for f in &p.fragments {
            let offending = f
                .event_range
                .map(|(_, end)| end)
                .into_iter()
                .chain(f.source_refs.iter().filter_map(|r| match r {
                    SourceRef::SessionEvent(e) => Some(*e),
                    // A compaction range covers session events through its
                    // end — the range's tail is the chronology fact (A-06).
                    SourceRef::CompactionRange(_, end) => Some(*end),
                    _ => None,
                }))
                .filter(|e| *e > frozen_seq)
                .max();
            if let Some(event) = offending
                && f.stability != StabilityClass::TurnVolatile
            {
                return Err(ProjectionError::ChronologyViolation(
                    f.id.clone(),
                    event,
                    frozen_seq,
                ));
            }
        }
        // 4. opaque artifacts (E-07, architecture.md:152): untransferable
        //    artifact payloads must never reach the provider.
        for f in &p.fragments {
            if f.content.contains("artifact://") {
                return Err(ProjectionError::OpaqueArtifact(f.id.clone()));
            }
        }
        // 5. token limit (architecture.md:139)
        let total_tokens: u64 = p
            .fragments
            .iter()
            .map(|f| estimate_tokens(&f.content))
            .sum();
        let budget = p.input.budgets.max_total_tokens;
        if total_tokens > budget {
            return Err(ProjectionError::OverBudget {
                needed: total_tokens,
                budget,
            });
        }
        // 6. semantic ordering (architecture.md:143): run_pipeline re-sorts
        //    before this stage; anything else is invalid input.
        if !p
            .fragments
            .windows(2)
            .all(|w| (w[0].order, &w[0].id) <= (w[1].order, &w[1].id))
        {
            return Err(ProjectionError::InvalidInput(
                "fragments out of semantic order".into(),
            ));
        }

        // derive the context
        let mut selected_events = Vec::new();
        let mut event_ranges = Vec::new();
        for f in &p.fragments {
            for r in &f.source_refs {
                if let SourceRef::SessionEvent(e) = r {
                    selected_events.push(*e);
                }
            }
            if let Some((start, end)) = f.event_range {
                event_ranges.push((start, end));
                let (lo, hi) = (start.min(end), end.max(start));
                selected_events.extend(lo..=hi);
            }
        }
        selected_events.sort_unstable();
        selected_events.dedup();
        let mut lifetime_root = None;
        let mut project_root = None;
        for f in &p.fragments {
            match f.kind {
                FragmentKind::LifetimeMemory => lifetime_root = f.dep_hashes.first().copied(),
                FragmentKind::ProjectMemory => project_root = f.dep_hashes.first().copied(),
                _ => {}
            }
        }
        let canonical = p
            .fragments
            .iter()
            .map(|f| {
                (
                    f.id.clone(),
                    f.kind,
                    f.stability,
                    f.content_hash,
                    f.dep_hashes.clone(),
                    f.event_range,
                    f.cache_eligible,
                )
            })
            .collect::<Vec<_>>();
        let projection_digest = Digest::new(
            &serde_json::to_vec(&canonical).expect("canonical serialization cannot fail"),
        );
        Ok(Some(ValidProviderContext {
            fragments: p.fragments.clone(),
            projection_digest,
            total_tokens,
            selected_events,
            event_ranges,
            memory_roots: (lifetime_root, project_root),
            dropped: p.dropped.clone(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fragment::{Fragment, FragmentKind, SourceRef, StabilityClass};
    use crate::input::ProjectionInput;
    use kanbei_core::Digest;

    fn frag(id: &str, order: u32, stability: StabilityClass) -> Fragment {
        Fragment {
            id: id.into(),
            order,
            kind: FragmentKind::HarnessContract,
            stability,
            content: id.into(),
            content_hash: Digest::new(id.as_bytes()),
            dep_hashes: Vec::new(),
            sensitivity: "internal".into(),
            derived_max: None,
            event_range: None,
            cache_eligible: true,
            source_refs: Vec::new(),
        }
    }

    fn project(fragments: Vec<Fragment>) -> Projection {
        Projection {
            input: ProjectionInput::new("harness"),
            fragments,
            dropped: Vec::new(),
        }
    }

    #[test]
    fn authority_recheck_fails_on_denied_source() {
        let mut p = project(vec![Fragment {
            source_refs: vec![SourceRef::MemoryClaim(Digest::new(b"x"))],
            ..frag("mem", 20, StabilityClass::ScopeStable)
        }]);
        let v = ValidatorStage::new(|r| !matches!(r, SourceRef::MemoryClaim(_)));
        let err = v.apply(&mut p).unwrap_err();
        assert!(matches!(err, ProjectionError::AuthorityDenied(id) if id == "mem"));
    }

    #[test]
    fn sensitivity_violation_bypassing_builder_rejected() {
        let mut p = project(vec![Fragment {
            sensitivity: "public".into(),
            derived_max: Some("secret".into()),
            ..frag("sneaky", 50, StabilityClass::TurnVolatile)
        }]);
        let v = ValidatorStage::new(|_| true);
        let err = v.apply(&mut p).unwrap_err();
        assert!(matches!(
            err,
            ProjectionError::SensitivityViolation(id, _, _) if id == "sneaky"
        ));
    }

    #[test]
    fn chronology_violation_beyond_frozen_prefix() {
        let mut p = project(vec![Fragment {
            event_range: Some((10, 20)),
            ..frag("conv", 40, StabilityClass::SessionStable)
        }]);
        p.input.trajectory.frozen_seq = 5;
        let v = ValidatorStage::new(|_| true);
        let err = v.apply(&mut p).unwrap_err();
        assert!(matches!(
            err,
            ProjectionError::ChronologyViolation(id, 20, 5) if id == "conv"
        ));
    }

    #[test]
    fn volatile_fragment_may_reference_beyond_frozen_prefix() {
        let mut p = project(vec![Fragment {
            event_range: Some((10, 20)),
            ..frag("recent", 52, StabilityClass::TurnVolatile)
        }]);
        p.input.trajectory.frozen_seq = 5;
        let v = ValidatorStage::new(|_| true);
        assert!(v.apply(&mut p).is_ok());
    }

    #[test]
    fn opaque_artifact_rejected() {
        let mut p = project(vec![Fragment {
            content: "see artifact://opaque/1".into(),
            ..frag("ev", 51, StabilityClass::TurnVolatile)
        }]);
        let v = ValidatorStage::new(|_| true);
        let err = v.apply(&mut p).unwrap_err();
        assert!(matches!(err, ProjectionError::OpaqueArtifact(id) if id == "ev"));
    }

    #[test]
    fn out_of_order_fragments_rejected() {
        let mut p = project(vec![
            frag("late", 50, StabilityClass::TurnVolatile),
            frag("early", 10, StabilityClass::Static),
        ]);
        let v = ValidatorStage::new(|_| true);
        let err = v.apply(&mut p).unwrap_err();
        assert!(matches!(
            err,
            ProjectionError::InvalidInput(m) if m == "fragments out of semantic order"
        ));
    }

    #[test]
    fn valid_projection_yields_context() {
        let mut p = project(vec![
            Fragment {
                event_range: Some((1, 3)),
                ..frag("conv", 40, StabilityClass::SessionStable)
            },
            Fragment {
                source_refs: vec![SourceRef::SessionEvent(3), SourceRef::SessionEvent(2)],
                ..frag("act", 50, StabilityClass::TurnVolatile)
            },
        ]);
        p.input.trajectory.frozen_seq = 5;
        let v = ValidatorStage::new(|_| true);
        let Some(vpc) = v.apply(&mut p).unwrap() else {
            panic!("validator produced no context");
        };
        assert_eq!(vpc.selected_events, vec![1, 2, 3]);
        assert_eq!(vpc.event_ranges, vec![(1, 3)]);
        assert_eq!(vpc.memory_roots, (None, None));
        assert_eq!(vpc.total_tokens, 2);
        assert_eq!(vpc.fragments.len(), 2);
    }
}
