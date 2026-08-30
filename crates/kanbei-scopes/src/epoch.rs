//! Epoch composition: the OCC protocol (R-26/C-09) that stages, validates,
//! and atomically publishes coherent contribution sets, plus the composition
//! digest (R-01): the epoch digest over the canonical JSON of the
//! composition's contribution set.

use kanbei_core::Digest;
use kanbei_services::ScopePath;
use serde_json::json;

use crate::contrib::Contribution;
use crate::errors::ScopeError;
use crate::registry::ContributionRegistry;

/// Domain-separation prefix for the composition digest (R-01; same pattern as
/// R-16/D-12 digest domains): a composition digest can never collide with a
/// digest of the same JSON without the marker.
const COMPOSITION_DOMAIN: &str = "composition-v1";

/// The current composition: an epoch counter, the epoch digest, and the
/// sorted contribution set it digests.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Composition {
    pub epoch: u64,
    pub digest: Digest,
    pub contributions: Vec<Contribution>,
}

impl Composition {
    /// The canonical bytes the epoch digest is computed over (R-01): the
    /// session pins them as an object so the digest ref is closure-valid.
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        composition_canonical_bytes(self.epoch, &self.contributions)
    }
}

/// A staged contribution set captured against a specific epoch (OCC).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedSet {
    pub against_epoch: u64,
    pub contributions: Vec<Contribution>,
}

/// Owns the current composition and the OCC publish protocol. The registry
/// passed to every method IS the current composition's materialized state:
/// the store is built from it and re-snapshots it after each publish, so
/// `registry.validate` validates against the current composition by
/// construction.
pub struct CompositionStore {
    current: Composition,
}

impl CompositionStore {
    /// A store whose current composition is the registry's state at epoch 0.
    pub fn new(registry: &ContributionRegistry) -> Self {
        let contributions = registry.snapshot();
        let digest = composition_digest(0, &contributions);
        Self {
            current: Composition {
                epoch: 0,
                digest,
                contributions,
            },
        }
    }

    pub fn current(&self) -> &Composition {
        &self.current
    }

    /// Captures the current epoch so a later [`Self::publish`] can detect a
    /// stale staged set (OCC).
    pub fn stage(&self, contributions: Vec<Contribution>) -> StagedSet {
        StagedSet {
            against_epoch: self.current.epoch,
            contributions,
        }
    }

    /// Validates `staged` against the current composition, applies it
    /// atomically, and advances the epoch. The direct path — no staleness
    /// check, since the set is validated against the current state here and
    /// now; the OCC path is [`Self::publish`].
    pub fn stage_publish(
        &mut self,
        staged: &[Contribution],
        registry: &mut ContributionRegistry,
    ) -> Result<(), ScopeError> {
        registry.validate(staged)?;
        let scope = staged
            .first()
            .map(|c| c.scope.clone())
            .unwrap_or_else(|| ScopePath(vec![]));
        registry.apply(&scope, staged)?;
        self.commit(registry);
        Ok(())
    }

    /// OCC publish of a previously staged set: a set staged against an epoch
    /// that is no longer current is rejected as stale — nothing is applied.
    pub fn publish(
        &mut self,
        staged: &StagedSet,
        registry: &mut ContributionRegistry,
    ) -> Result<(), ScopeError> {
        if staged.against_epoch != self.current.epoch {
            return Err(ScopeError::StaleEpoch {
                staged: staged.against_epoch,
                current: self.current.epoch,
            });
        }
        self.stage_publish(&staged.contributions, registry)
    }

    fn commit(&mut self, registry: &ContributionRegistry) {
        let epoch = self.current.epoch + 1;
        let contributions = registry.snapshot();
        let digest = composition_digest(epoch, &contributions);
        self.current = Composition {
            epoch,
            digest,
            contributions,
        };
    }
}

/// Digest over the canonical JSON of `(domain, epoch, sorted contributions)`
/// (R-01): the epoch digest is a pure function of epoch + contribution set,
/// so identical compositions digest identically and any contribution change
/// changes the digest.
fn composition_digest(epoch: u64, contributions: &[Contribution]) -> Digest {
    Digest::new(&composition_canonical_bytes(epoch, contributions))
}

fn composition_canonical_bytes(epoch: u64, contributions: &[Contribution]) -> Vec<u8> {
    serde_json::to_vec(&json!({
        "domain": COMPOSITION_DOMAIN,
        "epoch": epoch,
        "contributions": contributions,
    }))
    .expect("contribution kinds are always serializable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contrib::{
        CommandContribution, Contribution, ContributionKind, GuardContribution, KeymapContribution,
        ProjectionStageContribution, ServiceContribution, ThemeContribution, ToolContribution,
        UiMountContribution,
    };
    use crate::registry::ContributionRegistry;
    use kanbei_core::Id128;
    use kanbei_services::{ServiceContract, ServiceKey, ServiceProvider, ServiceRegistry};
    use serde_json::json;
    use std::sync::{Arc, Mutex};

    fn scope(name: &str) -> ScopePath {
        ScopePath(vec![name.to_string()])
    }

    fn provider(name: &str, version: u32) -> ServiceProvider {
        ServiceProvider {
            module_id: Id128::generate(),
            generation: 1,
            contract: ServiceContract {
                name: name.to_string(),
                version,
            },
        }
    }

    fn full_set(s: &ScopePath) -> Vec<Contribution> {
        vec![
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::Command(CommandContribution {
                    name: "cmd".into(),
                    handler: "cmd_h".into(),
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::Tool(ToolContribution {
                    name: "tool".into(),
                    manifest: json!({"replay_relevant": true}),
                    handler: "tool_h".into(),
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::Service(ServiceContribution {
                    key: ServiceKey {
                        scope: s.clone(),
                        name: "svc".into(),
                    },
                    provider: provider("svc", 1),
                    deps: vec![],
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::Keymap(KeymapContribution {
                    key: "k".into(),
                    action: "a".into(),
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::Theme(ThemeContribution {
                    name: "t".into(),
                    overlay: json!({"colors": {"bg": "#000"}}),
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::ProjectionStage(ProjectionStageContribution {
                    slot: "main".into(),
                    ordering: 10,
                    handler: "stage_h".into(),
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::UiMount(UiMountContribution {
                    name: "header".into(),
                    component: "Header".into(),
                    slot: None,
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::Guard(GuardContribution {
                    name: "g".into(),
                    predicate: "pred".into(),
                    monotonic: false,
                }),
            },
        ]
    }

    /// A second set with distinct names, for publishing into a non-empty registry.
    fn variant_set(s: &ScopePath) -> Vec<Contribution> {
        vec![
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::Command(CommandContribution {
                    name: "cmd2".into(),
                    handler: "cmd_h".into(),
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::Tool(ToolContribution {
                    name: "tool2".into(),
                    manifest: json!({"replay_relevant": false}),
                    handler: "tool_h".into(),
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::Service(ServiceContribution {
                    key: ServiceKey {
                        scope: s.clone(),
                        name: "svc2".into(),
                    },
                    provider: provider("svc2", 2),
                    deps: vec![],
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::Keymap(KeymapContribution {
                    key: "k2".into(),
                    action: "a".into(),
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::Theme(ThemeContribution {
                    name: "t2".into(),
                    overlay: json!({"fonts": {"size": 14}}),
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::ProjectionStage(ProjectionStageContribution {
                    slot: "main".into(),
                    ordering: 20,
                    handler: "stage_h".into(),
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::UiMount(UiMountContribution {
                    name: "footer".into(),
                    component: "Footer".into(),
                    slot: None,
                }),
            },
            Contribution {
                scope: s.clone(),
                kind: ContributionKind::Guard(GuardContribution {
                    name: "g2".into(),
                    predicate: "pred".into(),
                    monotonic: true,
                }),
            },
        ]
    }

    #[test]
    fn stage_publish_full_set_bumps_epoch_and_digests() {
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let mut store = CompositionStore::new(&registry);
        assert_eq!(store.current().epoch, 0);
        assert!(store.current().contributions.is_empty());

        let s = scope("app");
        let set = full_set(&s);
        store.stage_publish(&set, &mut registry).unwrap();
        assert_eq!(store.current().epoch, 1);
        assert_eq!(store.current().contributions.len(), 8);
        // the composition holds the published contributions
        assert!(store.current().contributions.iter().any(|c| matches!(
            &c.kind,
            ContributionKind::Command(cmd) if cmd.name == "cmd"
        )));

        // identical compositions at the same epoch digest identically
        let mut registry2 = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let mut store2 = CompositionStore::new(&registry2);
        store2.stage_publish(&set, &mut registry2).unwrap();
        assert_eq!(store2.current().epoch, store.current().epoch);
        assert_eq!(store2.current().digest, store.current().digest);

        // a changed contribution changes the digest
        let mut set2 = full_set(&s);
        set2[0] = Contribution {
            scope: s.clone(),
            kind: ContributionKind::Command(CommandContribution {
                name: "cmd2".into(),
                handler: "cmd_h".into(),
            }),
        };
        let mut registry3 = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let mut store3 = CompositionStore::new(&registry3);
        store3.stage_publish(&set2, &mut registry3).unwrap();
        assert_ne!(store3.current().digest, store.current().digest);
    }

    #[test]
    fn occ_stale_epoch_rejected() {
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let mut store = CompositionStore::new(&registry);
        let s = scope("app");
        let staged = store.stage(full_set(&s)); // against epoch 0

        // someone else publishes first: epoch 0 -> 1
        store
            .stage_publish(&variant_set(&s), &mut registry)
            .unwrap();
        assert_eq!(store.current().epoch, 1);

        // the stale set is rejected; nothing is applied
        let err = store.publish(&staged, &mut registry).unwrap_err();
        assert_eq!(
            err,
            ScopeError::StaleEpoch {
                staged: 0,
                current: 1,
            }
        );
        assert_eq!(store.current().epoch, 1);
        assert!(
            registry
                .snapshot()
                .iter()
                .all(|c| !matches!(&c.kind, ContributionKind::Command(cmd) if cmd.name == "cmd"))
        );

        // a fresh stage against the current epoch publishes (full_set is
        // disjoint from variant_set, so it does not conflict)
        let fresh = store.stage(full_set(&s));
        store.publish(&fresh, &mut registry).unwrap();
        assert_eq!(store.current().epoch, 2);
    }

    #[test]
    fn digest_is_domain_separated() {
        let mut registry = ContributionRegistry::new(Arc::new(Mutex::new(ServiceRegistry::new())));
        let mut store = CompositionStore::new(&registry);
        let s = scope("app");
        store.stage_publish(&full_set(&s), &mut registry).unwrap();
        let epoch = store.current().epoch;
        assert_eq!(epoch, 1);

        // the same epoch + contributions WITHOUT the composition-v1 domain
        // marker must not produce the same digest (R-01 domain separation)
        let unprefixed = serde_json::to_vec(&json!({
            "epoch": epoch,
            "contributions": store.current().contributions,
        }))
        .unwrap();
        assert_ne!(store.current().digest, Digest::new(&unprefixed));
    }
}
