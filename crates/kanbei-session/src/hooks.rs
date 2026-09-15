//! T9 lifecycle hook seams (session half): decision parsing, the ordered
//! per-kind binding set rebuilt on composition change, and the sequential
//! evaluation that yields a decision plus accumulated annotations.
//!
//! Hooks are advisory: they only ever return a decision (`continue`/`deny`)
//! plus annotations. The kernel commits/executes — a hook can never allow a
//! tool past the capability/approval/retention path; `continue` simply
//! traverses the unmodified path. A hook fault (trap/timeout/invalid/gone)
//! degrades that binding only and applies the built-in default for the kind
//! (`on_tool_intent` → deny, fail-closed; `on_turn_start` → continue).
//!
//! Fault recovery (respawn + rebind + the canonical `module_fault` fact) is
//! wired by [`crate::Session`]; this module owns the pure decision/binding
//! logic so it is unit-testable without a live guest.

use kanbei_core::digest::Digest;
use kanbei_core::id::Id128;
use kanbei_modules::{HOOK_WAIT, HookError, ModuleManager};
use kanbei_scopes::contrib::HookKind;
use kanbei_scopes::registry::ContributionRegistry;
use serde_json::Value;

/// Canonical session-side name for a hook annotation (the kernel-visible
/// `{key, value}` pair; the type lives in kanbei-tools because it rides the
/// committed [`kanbei_tools::ToolIntent`] payload).
pub type HookAnnotation = kanbei_tools::ToolAnnotation;

/// Maximum length of a deny reason carried in memory (it never reaches a
/// canonical payload verbatim — only its decision digest does).
const DENY_REASON_MAX: usize = 240;
/// Maximum annotation key length.
const ANNOTATION_KEY_MAX: usize = 64;
/// Maximum annotation string-value length.
const ANNOTATION_VALUE_MAX: usize = 512;

/// A hook's decision. Deny short-circuits evaluation; the reason is
/// guest-authored memory-only text (canonical facts carry the decision
/// digest, never the raw string).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Continue,
    Deny { reason: String },
}

impl Decision {
    /// Canonical digest of the decision content (domain-separated JSON), the
    /// value canonical facts carry in place of the guest string.
    pub fn digest(&self) -> Digest {
        let canonical = match self {
            Decision::Continue => serde_json::json!({ "decision": "continue" }),
            Decision::Deny { reason } => {
                serde_json::json!({ "decision": "deny", "reason": reason })
            }
        };
        Digest::new(canonical.to_string().as_bytes())
    }
}

/// A parsed hook return: the decision plus any annotations it attached.
#[derive(Debug, Clone, PartialEq)]
pub struct HookDecision {
    pub decision: Decision,
    pub annotations: Vec<HookAnnotation>,
}

impl HookDecision {
    /// Parse the guest's JSON string. Malformed JSON, a non-object, a missing
    /// or unknown `decision`, or a malformed annotation all surface as
    /// [`HookError::Invalid`] (a fault, never a panic).
    pub fn parse(raw: &str) -> Result<Self, HookError> {
        let value: Value = serde_json::from_str(raw).map_err(|_| HookError::Invalid)?;
        let obj = value.as_object().ok_or(HookError::Invalid)?;
        let decision = match obj.get("decision").and_then(Value::as_str) {
            Some("continue") => Decision::Continue,
            Some("deny") => {
                let reason = obj
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("denied by hook");
                Decision::Deny {
                    reason: truncate(reason, DENY_REASON_MAX),
                }
            }
            _ => return Err(HookError::Invalid),
        };
        let annotations = match obj.get("annotations") {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(items)) => items
                .iter()
                .map(parse_annotation)
                .collect::<Result<Vec<_>, _>>()?,
            Some(_) => return Err(HookError::Invalid),
        };
        Ok(Self {
            decision,
            annotations,
        })
    }
}

fn parse_annotation(item: &Value) -> Result<HookAnnotation, HookError> {
    let obj = item.as_object().ok_or(HookError::Invalid)?;
    let key = obj
        .get("key")
        .and_then(Value::as_str)
        .ok_or(HookError::Invalid)?;
    let value = obj.get("value").cloned().ok_or(HookError::Invalid)?;
    Ok(HookAnnotation {
        key: truncate(key, ANNOTATION_KEY_MAX),
        value: cap_value(value),
    })
}

fn cap_value(value: Value) -> Value {
    match value {
        Value::String(s) if s.chars().count() > ANNOTATION_VALUE_MAX => {
            Value::String(truncate(&s, ANNOTATION_VALUE_MAX))
        }
        other => other,
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}

/// One ordered hook binding for a kind: the contributing module/generation
/// and its per-binding degradation state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookBinding {
    pub module_id: Id128,
    pub package_digest: Digest,
    pub generation: u64,
    pub name: String,
    pub hook: HookKind,
    pub entry: String,
    /// Whether a fault has degraded this binding (its decisions fall back to
    /// the built-in default until a respawn rebinds a fresh generation).
    pub degraded: bool,
    /// Number of faults suppressed while degraded (cumulative per binding).
    pub faults: u64,
    /// Commit seq of the `module_fault` fact recording the transition.
    pub last_fault_seq: Option<u64>,
}

impl HookBinding {
    fn identity(&self) -> (Id128, u64, HookKind, &str, &str) {
        (
            self.module_id,
            self.generation,
            self.hook,
            self.name.as_str(),
            self.entry.as_str(),
        )
    }
}

/// One recorded fault during an evaluation: the binding identity at the time
/// of the fault plus the structured error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookFault {
    /// Index into the kind's binding list (for seq bookkeeping).
    pub index: usize,
    pub binding: HookBinding,
    pub error: HookError,
    /// True when this fault transitioned the binding into `degraded` (the
    /// condition for committing a `module_fault` fact).
    pub transitioned: bool,
}

/// The result of evaluating one hook kind: the effective decision, the
/// annotations accumulated from hooks that ran before the outcome, the
/// denier (if any), and the faults recorded along the way.
#[derive(Debug, Clone, PartialEq)]
pub struct HookEvaluation {
    pub decision: Decision,
    pub annotations: Vec<HookAnnotation>,
    /// The binding whose decision ended evaluation (deny, or the degraded
    /// default denier).
    pub denier: Option<HookBinding>,
    pub faults: Vec<HookFault>,
}

impl HookEvaluation {
    /// The built-in default applied to a faulting or degraded binding.
    pub fn default_for(kind: HookKind) -> Decision {
        match kind {
            // Fail-closed: an unavailable intent gate must never permit.
            HookKind::OnToolIntent => Decision::Deny {
                reason: "denied: on_tool_intent hook unavailable".to_string(),
            },
            HookKind::OnTurnStart => Decision::Continue,
        }
    }

    /// The outcome when no hooks are configured for the kind: hooks are
    /// advisory/additive, so absence means the unmodified path (continue).
    pub fn default_only(_kind: HookKind) -> Self {
        Self {
            decision: Decision::Continue,
            annotations: Vec::new(),
            denier: None,
            faults: Vec::new(),
        }
    }
}

/// The ordered bindings per hook kind. Rebuilt on composition change (mirrors
/// how the UI host rebinds its mounts): degradation state is carried across
/// for a binding whose `(module_id, generation, hook, name, entry)` identity
/// is unchanged, so a failed respawn leaves the binding degraded.
#[derive(Debug, Clone, Default)]
pub struct HookSet {
    on_turn_start: Vec<HookBinding>,
    on_tool_intent: Vec<HookBinding>,
}

impl HookSet {
    /// Build a set directly from ordered binding lists (tests + rebuild).
    #[cfg(test)]
    pub fn from_bindings(
        on_turn_start: Vec<HookBinding>,
        on_tool_intent: Vec<HookBinding>,
    ) -> Self {
        Self {
            on_turn_start,
            on_tool_intent,
        }
    }

    pub fn bindings(&self, kind: HookKind) -> &[HookBinding] {
        match kind {
            HookKind::OnTurnStart => &self.on_turn_start,
            HookKind::OnToolIntent => &self.on_tool_intent,
        }
    }

    fn bindings_mut(&mut self, kind: HookKind) -> &mut Vec<HookBinding> {
        match kind {
            HookKind::OnTurnStart => &mut self.on_turn_start,
            HookKind::OnToolIntent => &mut self.on_tool_intent,
        }
    }

    #[cfg(test)]
    pub fn is_empty(&self) -> bool {
        self.on_turn_start.is_empty() && self.on_tool_intent.is_empty()
    }

    /// Record the commit seq of the fault fact on the binding at `index`.
    pub(crate) fn mark_fault_seq(&mut self, kind: HookKind, index: usize, seq: u64) {
        if let Some(b) = self.bindings_mut(kind).get_mut(index) {
            b.last_fault_seq = Some(seq);
        }
    }

    /// Rebuild the ordered bindings from the composition registry. Only
    /// contributions whose generation still resolves are bound (mirroring
    /// `rebind_ui`). The registry's `hooks_for` already orders by
    /// `(scope path, name)`.
    pub fn rebuild(&mut self, registry: &ContributionRegistry, manager: &ModuleManager) {
        let generations: std::collections::HashMap<u64, (Id128, Digest)> = manager
            .snapshot()
            .into_iter()
            .map(|(id, generation, package)| (generation, (id, package)))
            .collect();
        let old = std::mem::take(self);
        for kind in [HookKind::OnTurnStart, HookKind::OnToolIntent] {
            let mut bindings = Vec::new();
            for (_, contrib) in registry.hooks_for(kind) {
                let Some(generation) = manager.hook_generation(kind, &contrib.name) else {
                    continue;
                };
                let Some((module_id, package_digest)) = generations.get(&generation) else {
                    continue;
                };
                let mut binding = HookBinding {
                    module_id: *module_id,
                    package_digest: *package_digest,
                    generation,
                    name: contrib.name,
                    hook: kind,
                    entry: contrib.entry,
                    degraded: false,
                    faults: 0,
                    last_fault_seq: None,
                };
                // Carry degradation state when the exact binding survives.
                if let Some(previous) = old
                    .bindings(kind)
                    .iter()
                    .find(|p| p.identity() == binding.identity())
                {
                    binding.degraded = previous.degraded;
                    binding.faults = previous.faults;
                    binding.last_fault_seq = previous.last_fault_seq;
                }
                bindings.push(binding);
            }
            *self.bindings_mut(kind) = bindings;
        }
    }

    /// Evaluate the kind's bindings in order. Degraded bindings are skipped
    /// and their built-in default applies; a deny ends evaluation
    /// immediately (annotations from preceding hooks — and the denier's own —
    /// are kept).
    pub fn evaluate(
        &mut self,
        manager: &ModuleManager,
        kind: HookKind,
        context_json: &str,
    ) -> HookEvaluation {
        let mut annotations: Vec<HookAnnotation> = Vec::new();
        let mut faults: Vec<HookFault> = Vec::new();
        let count = self.bindings(kind).len();
        for index in 0..count {
            let binding = self.bindings(kind)[index].clone();
            if binding.degraded {
                // A degraded binding applies the built-in default.
                if let Decision::Deny { reason } = HookEvaluation::default_for(kind) {
                    return HookEvaluation {
                        decision: Decision::Deny { reason },
                        annotations,
                        denier: Some(binding),
                        faults,
                    };
                }
                continue;
            }
            match manager.call_hook(binding.generation, kind, context_json, HOOK_WAIT) {
                Err(error) => {
                    let fault = self.record_fault(kind, index, error);
                    faults.push(fault);
                    if let Decision::Deny { reason } = HookEvaluation::default_for(kind) {
                        return HookEvaluation {
                            decision: Decision::Deny { reason },
                            annotations,
                            denier: Some(self.bindings(kind)[index].clone()),
                            faults,
                        };
                    }
                }
                Ok(raw) => match HookDecision::parse(&raw) {
                    Err(error) => {
                        let fault = self.record_fault(kind, index, error);
                        faults.push(fault);
                        if let Decision::Deny { reason } = HookEvaluation::default_for(kind) {
                            return HookEvaluation {
                                decision: Decision::Deny { reason },
                                annotations,
                                denier: Some(self.bindings(kind)[index].clone()),
                                faults,
                            };
                        }
                    }
                    Ok(decision) => {
                        {
                            let b = &mut self.bindings_mut(kind)[index];
                            b.degraded = false;
                        }
                        annotations.extend(decision.annotations);
                        if let Decision::Deny { .. } = decision.decision {
                            return HookEvaluation {
                                decision: decision.decision,
                                annotations,
                                denier: Some(self.bindings(kind)[index].clone()),
                                faults,
                            };
                        }
                    }
                },
            }
        }
        HookEvaluation {
            decision: Decision::Continue,
            annotations,
            denier: None,
            faults,
        }
    }

    fn record_fault(&mut self, kind: HookKind, index: usize, error: HookError) -> HookFault {
        let b = &mut self.bindings_mut(kind)[index];
        let transitioned = !b.degraded;
        b.degraded = true;
        b.faults += 1;
        HookFault {
            index,
            binding: b.clone(),
            error,
            transitioned,
        }
    }
}

/// `fault_class` wire value for the canonical `module_fault` fact.
fn fault_class(error: HookError) -> &'static str {
    match error {
        HookError::Trap => "trap",
        HookError::Timeout => "timeout",
        HookError::Invalid => "invalid",
        HookError::Gone => "gone",
    }
}

impl crate::Session {
    /// Rebuild the hook binding set from the composition registry (called on
    /// every composition change, mirroring [`crate::Session::rebind_ui`]).
    pub(crate) fn rebind_hooks(&mut self) {
        let Some(manager) = self.modules.as_ref() else {
            self.hooks = HookSet::default();
            return;
        };
        self.hooks.rebuild(&self.registry, manager);
    }

    /// Evaluate a hook kind end-to-end: run the ordered bindings, then apply
    /// the fault policy for any fault — commit one canonical `module_fault`
    /// per transition into degraded (ids/digests/counts only), respawn the
    /// faulty module, and rebuild the binding set so a fresh generation's
    /// hooks are used. A failed respawn leaves the binding degraded (its
    /// built-in default keeps applying) rather than blocking.
    pub(crate) fn evaluate_hooks(
        &mut self,
        kind: HookKind,
        context_json: &str,
    ) -> HookEvaluation {
        let evaluation = match self.modules.as_ref() {
            Some(manager) => self.hooks.evaluate(manager, kind, context_json),
            None => HookEvaluation::default_only(kind),
        };
        if evaluation.faults.is_empty() {
            return evaluation;
        }
        for fault in &evaluation.faults {
            if fault.transitioned {
                let payload = serde_json::json!({
                    "module_id": fault.binding.module_id.to_string(),
                    "package_digest": fault.binding.package_digest.to_string(),
                    "generation": fault.binding.generation,
                    "hook": fault.binding.hook.as_str(),
                    "entry": fault.binding.entry,
                    "fault_class": fault_class(fault.error),
                    "count": fault.binding.faults,
                });
                if let Ok(receipt) = self.commit(
                    vec![crate::NewEvent {
                        kind: "module_fault".into(),
                        payload_schema: 1,
                        payload,
                        objects: Vec::new(),
                        refs: Vec::new(),
                    }],
                    None,
                ) {
                    self.hooks.mark_fault_seq(kind, fault.index, receipt.last_seq);
                }
            }
            if let Some(manager) = self.modules.as_mut() {
                let _ = manager.respawn(fault.binding.module_id);
            }
        }
        self.rebind_hooks();
        evaluation
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn binding(name: &str, hook: HookKind) -> HookBinding {
        HookBinding {
            module_id: Id128::generate(),
            package_digest: Digest::new(name.as_bytes()),
            generation: 1,
            name: name.to_string(),
            hook,
            entry: format!("kb_{}", hook.as_str()),
            degraded: false,
            faults: 0,
            last_fault_seq: None,
        }
    }

    #[test]
    fn parses_continue() {
        let d = HookDecision::parse(r#"{"decision":"continue"}"#).unwrap();
        assert_eq!(d.decision, Decision::Continue);
        assert!(d.annotations.is_empty());
    }

    #[test]
    fn parses_deny_and_caps_the_reason() {
        let long = "x".repeat(DENY_REASON_MAX + 50);
        let raw = json!({ "decision": "deny", "reason": long }).to_string();
        let d = HookDecision::parse(&raw).unwrap();
        match d.decision {
            Decision::Deny { reason } => assert_eq!(reason.chars().count(), DENY_REASON_MAX),
            other => panic!("expected deny, got {other:?}"),
        }
        // A deny without a reason gets the kernel default string.
        let d = HookDecision::parse(r#"{"decision":"deny"}"#).unwrap();
        assert_eq!(
            d.decision,
            Decision::Deny {
                reason: "denied by hook".into()
            }
        );
    }

    #[test]
    fn parses_annotations() {
        let raw = r#"{"decision":"continue","annotations":[{"key":"risk","value":"low"},{"key":"n","value":3}]}"#;
        let d = HookDecision::parse(raw).unwrap();
        assert_eq!(d.annotations.len(), 2);
        assert_eq!(d.annotations[0].key, "risk");
        assert_eq!(d.annotations[0].value, json!("low"));
        assert_eq!(d.annotations[1].value, json!(3));
    }

    #[test]
    fn malformed_decisions_are_invalid() {
        for raw in [
            "not json",
            "[]",
            "{}",
            r#"{"decision":"maybe"}"#,
            r#"{"decision":1}"#,
            r#"{"decision":"continue","annotations":"nope"}"#,
            r#"{"decision":"continue","annotations":[{"value":1}]}"#,
        ] {
            assert_eq!(HookDecision::parse(raw), Err(HookError::Invalid), "raw={raw}");
        }
    }

    #[test]
    fn decision_digests_distinguish_outcomes() {
        let cont = Decision::Continue.digest();
        let deny = Decision::Deny { reason: "x".into() }.digest();
        assert_ne!(cont, deny);
        assert_eq!(cont, Decision::Continue.digest());
    }

    #[test]
    fn bindings_keep_order() {
        let set = HookSet::from_bindings(
            vec![binding("a", HookKind::OnTurnStart), binding("b", HookKind::OnTurnStart)],
            vec![binding("c", HookKind::OnToolIntent)],
        );
        let names: Vec<_> = set
            .bindings(HookKind::OnTurnStart)
            .iter()
            .map(|b| b.name.clone())
            .collect();
        assert_eq!(names, vec!["a", "b"]);
        assert_eq!(set.bindings(HookKind::OnToolIntent)[0].name, "c");
        assert!(!set.is_empty());
        assert!(HookSet::default().is_empty());
    }

    #[test]
    fn default_is_fail_closed_for_tool_intent() {
        assert!(matches!(
            HookEvaluation::default_for(HookKind::OnToolIntent),
            Decision::Deny { .. }
        ));
        assert_eq!(
            HookEvaluation::default_for(HookKind::OnTurnStart),
            Decision::Continue
        );
    }
}
