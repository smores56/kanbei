//! Provider lowering (architecture.md:145-150): the longest legal stable
//! prefix becomes System-role cacheable messages, the rest become User-role
//! messages, and the suppression ban (R-05/E-05) guarantees every selected
//! fragment is lowered exactly once, in order.

use kanbei_core::Digest;
use kanbei_provider::{CachePlan, Message, Role};
use serde::{Deserialize, Serialize};

use crate::error::ProjectionError;
use crate::fragment::StabilityClass;
use crate::validator::ValidProviderContext;

/// The provider request materialized from a validated projection: messages
/// plus the cache plan for this call (R-08/E-13).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lowering {
    pub messages: Vec<Message>,
    pub cache_plan: CachePlan,
}

/// Lower a validated projection into provider messages (architecture.md:145).
///
/// Walks `vpc.fragments` in semantic order; a fragment joins the stable
/// prefix while it is non-volatile, cache-eligible, and caching is
/// supported. The prefix stops at the FIRST fragment failing any condition —
/// later stable-eligible fragments still go to the tail, so the lowering
/// never reorders semantics. Prefix fragments become `Role::System`
/// messages (the session may re-map roles in a later wave); tail fragments
/// become `Role::User`.
///
/// When the prefix is non-empty, `cache_plan` is
/// [`CachePlan::StablePrefix`] with a digest over the canonical
/// concatenation of prefix fragments' (id, content_hash, dep_hashes);
/// otherwise [`CachePlan::None`].
pub fn lower(
    vpc: &ValidProviderContext,
    cache_supported: bool,
) -> Result<Lowering, ProjectionError> {
    if vpc.fragments.is_empty() {
        return Err(ProjectionError::InvalidInput("empty projection".into()));
    }
    let mut messages = Vec::with_capacity(vpc.fragments.len());
    let mut ids = Vec::with_capacity(vpc.fragments.len());
    let mut prefix: Vec<(String, Digest, Vec<Digest>)> = Vec::new();
    let mut in_prefix = cache_supported;
    for f in &vpc.fragments {
        let joins = in_prefix && f.stability != StabilityClass::TurnVolatile && f.cache_eligible;
        if joins {
            prefix.push((f.id.clone(), f.content_hash, f.dep_hashes.clone()));
        } else {
            in_prefix = false;
        }
        messages.push(Message {
            role: if joins { Role::System } else { Role::User },
            content: f.content.clone(),
            tool_call_id: None,
        });
        ids.push(f.id.clone());
    }
    let cache_plan = if prefix.is_empty() {
        CachePlan::None
    } else {
        let canonical = serde_json::to_vec(&prefix).expect("canonical serialization cannot fail");
        CachePlan::StablePrefix {
            digest: Digest::new(&canonical),
        }
    };
    // R-05/E-05 suppression ban: the message count matches the fragment
    // count and the id sequence matches, by construction. Unreachable —
    // kept as a defensive check and tested.
    debug_assert_eq!(messages.len(), vpc.fragments.len());
    if messages.len() != vpc.fragments.len()
        || ids
            != vpc
                .fragments
                .iter()
                .map(|f| f.id.clone())
                .collect::<Vec<_>>()
    {
        return Err(ProjectionError::InvalidInput(
            "lowering suppressed a fragment".into(),
        ));
    }
    Ok(Lowering {
        messages,
        cache_plan,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fragment::{Fragment, FragmentBuilder, FragmentKind, StabilityClass};
    use kanbei_core::Digest;
    use kanbei_provider::Role;

    fn frag(id: &str, order: u32, stability: StabilityClass, eligible: bool) -> Fragment {
        FragmentBuilder::new(id, order, FragmentKind::ConversationPrefix, stability)
            .content(id)
            .cache_eligible(eligible)
            .build()
            .unwrap()
    }

    fn vpc(fragments: Vec<Fragment>) -> ValidProviderContext {
        ValidProviderContext {
            fragments,
            projection_digest: Digest::new(b"vpc"),
            total_tokens: 0,
            selected_events: Vec::new(),
            event_ranges: Vec::new(),
            memory_roots: (None, None),
            dropped: Vec::new(),
        }
    }

    #[test]
    fn longest_stable_prefix_stops_at_first_volatile() {
        let v = vpc(vec![
            frag("harness", 0, StabilityClass::Static, true),
            frag("schema", 10, StabilityClass::Static, true),
            frag("active", 50, StabilityClass::TurnVolatile, false),
            frag("late.stable", 60, StabilityClass::SessionStable, true),
            frag("trigger", 99, StabilityClass::TurnVolatile, false),
        ]);
        let low = lower(&v, true).unwrap();
        let roles: Vec<Role> = low.messages.iter().map(|m| m.role.clone()).collect();
        assert_eq!(
            roles,
            vec![
                Role::System,
                Role::System,
                Role::User,
                Role::User,
                Role::User
            ]
        );
        assert!(matches!(low.cache_plan, CachePlan::StablePrefix { .. }));
        // deterministic prefix digest
        let low2 = lower(&v, true).unwrap();
        assert_eq!(low.cache_plan, low2.cache_plan);
        // caching unsupported → no prefix at all
        let low3 = lower(&v, false).unwrap();
        assert_eq!(low3.cache_plan, CachePlan::None);
        assert!(low3.messages.iter().all(|m| m.role == Role::User));
    }

    #[test]
    fn prefix_empty_when_leading_fragment_not_eligible() {
        let v = vpc(vec![
            frag("volatile.first", 0, StabilityClass::TurnVolatile, false),
            frag("stable.later", 10, StabilityClass::Static, true),
        ]);
        let low = lower(&v, true).unwrap();
        assert_eq!(low.cache_plan, CachePlan::None);
        assert!(low.messages.iter().all(|m| m.role == Role::User));
    }

    #[test]
    fn every_fragment_lowered_exactly_once_in_order() {
        let v = vpc(vec![
            frag("a", 0, StabilityClass::Static, true),
            frag("b", 50, StabilityClass::TurnVolatile, false),
        ]);
        let low = lower(&v, true).unwrap();
        assert_eq!(low.messages.len(), v.fragments.len());
        assert_eq!(low.messages[0].role, Role::System);
        assert_eq!(low.messages[1].role, Role::User);
        for (m, f) in low.messages.iter().zip(&v.fragments) {
            assert_eq!(m.content, f.content);
            assert_eq!(m.tool_call_id, None);
        }
    }

    #[test]
    fn empty_projection_rejected() {
        let err = lower(&vpc(Vec::new()), true).unwrap_err();
        assert!(matches!(err, ProjectionError::InvalidInput(m) if m == "empty projection"));
    }
}
