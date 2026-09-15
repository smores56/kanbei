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

use std::collections::HashSet;
use std::time::Instant;

use kanbei_capabilities::Grant;
use kanbei_core::digest::Digest;
use kanbei_core::id::Id128;
use kanbei_modules::{HOOK_WAIT, HookError, ModuleManager};
use kanbei_scopes::contrib::HookKind;
use kanbei_scopes::registry::ContributionRegistry;
use kanbei_services::ScopePath;
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
/// Maximum number of annotations a single hook return may attach (C).
const ANNOTATION_COUNT_MAX: usize = 32;
/// Maximum total serialized size (bytes) of a hook's annotation list (C). A
/// bound on nested structures, not just top-level strings.
const ANNOTATION_TOTAL_MAX: usize = 4096;

/// The kernel-authored constant reason a denied tool/turn carries. Never guest
/// text (C): the hook's identity rides canonical facts as ids/digests instead.
pub(crate) const DENIED_REASON: &str = "denied by a module hook";

/// Minimum respawn budget: below this the fault path degrades without a
/// synchronous drain (G).
const MIN_RESPAWN_BUDGET: std::time::Duration = std::time::Duration::from_millis(20);

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
            Some(Value::Array(items)) => {
                let mut parsed = items
                    .iter()
                    .map(parse_annotation)
                    .collect::<Result<Vec<_>, _>>()?;
                // C: bound BOTH the count and the total serialized size, so a
                // guest cannot smuggle unbounded nested data into a payload.
                parsed.truncate(ANNOTATION_COUNT_MAX);
                while !parsed.is_empty()
                    && serde_json::to_string(&parsed)
                        .map(|s| s.len())
                        .unwrap_or(usize::MAX)
                        > ANNOTATION_TOTAL_MAX
                {
                    parsed.pop();
                }
                parsed
            }
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
    /// The scope the contribution was registered in (E: part of the identity,
    /// so same-named hooks in different scopes never collide).
    pub scope: ScopePath,
    pub name: String,
    pub hook: HookKind,
    /// Whether a fault has degraded this binding (its decisions fall back to
    /// the built-in default until a composition rebind clears the backoff).
    pub degraded: bool,
    /// Cumulative fault count for this binding (across respawn generations and
    /// suppressed degraded decisions).
    pub faults: u64,
}

/// One recorded fault during an evaluation: the binding identity at the time
/// of the fault plus the structured error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookFault {
    /// Index into the kind's binding list (for seq bookkeeping).
    pub index: usize,
    pub binding: HookBinding,
    pub error: HookError,
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
                reason: DENIED_REASON.to_string(),
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
/// how the UI host rebinds its mounts): fault/degradation state is carried
/// across for a binding whose module identity is unchanged. A contribution
/// that is registered but whose generation no longer resolves is retained as
/// a DEGRADED binding (never dropped — the fail-closed default keeps
/// applying, A). `backoff` suppresses re-running (and re-respawning) a
/// deterministically-faulting binding until the next composition rebind (G).
#[derive(Debug, Clone, Default)]
pub struct HookSet {
    on_turn_start: Vec<HookBinding>,
    on_tool_intent: Vec<HookBinding>,
    /// `(scope, kind, name)` of bindings whose faults must be suppressed until
    /// the next composition rebind (G backoff).
    backoff: HashSet<(ScopePath, HookKind, String)>,
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
            backoff: HashSet::new(),
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

    /// Put a binding into backoff (G) so repeated decisions apply the default
    /// without a fault fact or a respawn until the next composition rebind.
    pub(crate) fn set_backoff(&mut self, scope: &ScopePath, kind: HookKind, name: &str) {
        self.backoff
            .insert((scope.clone(), kind, name.to_string()));
    }

    /// Whether a binding is currently backed off.
    pub(crate) fn is_backed_off(&self, scope: &ScopePath, kind: HookKind, name: &str) -> bool {
        self.backoff.contains(&(scope.clone(), kind, name.to_string()))
    }

    /// Clear all backoff (a composition rebind gives every binding a fresh
    /// chance).
    pub(crate) fn clear_backoff(&mut self) {
        self.backoff.clear();
    }

    /// Rebuild the ordered bindings from the composition registry. A
    /// contribution whose generation still resolves is bound (mirroring
    /// `rebind_ui`); one that is registered but unresolvable is retained as a
    /// DEGRADED binding when it was previously bound (A). The registry's
    /// `hooks_for` already orders by `(scope path, name)` and only ever
    /// contains trust-gated hooks (B).
    pub fn rebuild(&mut self, registry: &ContributionRegistry, manager: &ModuleManager) {
        let generations: std::collections::HashMap<u64, (Id128, Digest)> = manager
            .snapshot()
            .into_iter()
            .map(|(id, generation, package)| (generation, (id, package)))
            .collect();
        let old_turn = std::mem::take(&mut self.on_turn_start);
        let old_tool = std::mem::take(&mut self.on_tool_intent);
        for kind in [HookKind::OnTurnStart, HookKind::OnToolIntent] {
            let old = match kind {
                HookKind::OnTurnStart => &old_turn,
                HookKind::OnToolIntent => &old_tool,
            };
            let mut bindings = Vec::new();
            for (scope, contrib) in registry.hooks_for(kind) {
                let resolved = manager
                    .hook_generation(&scope, kind, &contrib.name)
                    .and_then(|generation| {
                        generations
                            .get(&generation)
                            .map(|(id, package)| (generation, *id, *package))
                    });
                let previous = old
                    .iter()
                    .find(|p| p.scope == scope && p.hook == kind && p.name == contrib.name);
                let mut binding = match resolved {
                    Some((generation, module_id, package_digest)) => HookBinding {
                        module_id,
                        package_digest,
                        generation,
                        scope: scope.clone(),
                        name: contrib.name.clone(),
                        hook: kind,
                        degraded: false,
                        faults: 0,
                    },
                    None => match previous {
                        // Registered but unresolvable: retain DEGRADED so the
                        // fail-closed default keeps applying (A).
                        Some(prev) => {
                            let mut prev = prev.clone();
                            prev.degraded = true;
                            prev
                        }
                        // Never bound (or trust-gated): nothing to fail closed
                        // on — an untrusted registration must not manufacture a
                        // deny (B).
                        None => continue,
                    },
                };
                // Carry the cumulative fault count across respawn generations
                // of the same binding (G: `count` accumulates).
                if let Some(prev) = previous {
                    binding.faults = binding.faults.max(prev.faults);
                }
                if self
                    .backoff
                    .contains(&(binding.scope.clone(), kind, binding.name.clone()))
                {
                    binding.degraded = true;
                }
                bindings.push(binding);
            }
            *self.bindings_mut(kind) = bindings;
        }
    }

    /// Evaluate the kind's bindings in order. Degraded bindings are skipped
    /// and their built-in default applies (their suppressed-fault counter
    /// advances); a deny ends evaluation immediately (annotations from
    /// preceding hooks — and the denier's own — are kept).
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
                // A degraded binding applies the built-in default; counting the
                // suppressed fault lets `count` accumulate across decisions (G).
                self.bindings_mut(kind)[index].faults += 1;
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
                // A kernel-side context bug must not degrade a healthy guest (H).
                Err(HookError::InvalidContext) => continue,
                Err(error) => {
                    let is_deny = matches!(
                        HookEvaluation::default_for(kind),
                        Decision::Deny { .. }
                    );
                    let fault = self.record_fault(kind, index, error);
                    faults.push(fault);
                    if is_deny {
                        return HookEvaluation {
                            decision: HookEvaluation::default_for(kind),
                            annotations,
                            denier: Some(self.bindings(kind)[index].clone()),
                            faults,
                        };
                    }
                }
                Ok(raw) => match HookDecision::parse(&raw) {
                    Err(error) => {
                        let is_deny = matches!(
                            HookEvaluation::default_for(kind),
                            Decision::Deny { .. }
                        );
                        let fault = self.record_fault(kind, index, error);
                        faults.push(fault);
                        if is_deny {
                            return HookEvaluation {
                                decision: HookEvaluation::default_for(kind),
                                annotations,
                                denier: Some(self.bindings(kind)[index].clone()),
                                faults,
                            };
                        }
                    }
                    Ok(decision) => {
                        self.bindings_mut(kind)[index].degraded = false;
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
        b.degraded = true;
        b.faults += 1;
        HookFault {
            index,
            binding: b.clone(),
            error,
        }
    }
}

/// `fault_class` wire value for the canonical `module_fault` fact.
fn fault_class(error: HookError) -> &'static str {
    match error {
        HookError::Trap => "trap",
        HookError::Timeout => "timeout",
        HookError::Invalid => "invalid",
        // Unreachable in practice (kernel context is always valid JSON); never
        // emitted as a guest fault anyway.
        HookError::InvalidContext => "invalid_context",
        HookError::Gone => "gone",
    }
}

impl crate::Session {
    /// Rebuild the hook binding set from the composition registry (called on
    /// every composition change, mirroring [`crate::Session::rebind_ui`]).
    pub(crate) fn rebind_hooks(&mut self) {
        let Some(manager) = self.modules.as_ref() else {
            self.hooks = HookSet::default();
            self.hook_respawned.clear();
            return;
        };
        self.hooks.rebuild(&self.registry, manager);
        // NEW-7: a module that left the active set must not stay marked as
        // respawned — otherwise re-activating it could never respawn again.
        let active: HashSet<Id128> = manager
            .snapshot()
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        self.hook_respawned.retain(|id| active.contains(id));
    }

    /// Clear per-composition fault recovery state: every binding gets a fresh
    /// chance on the next composition rebind (G).
    pub(crate) fn reset_hook_recovery(&mut self) {
        self.hook_respawned.clear();
        self.hooks.clear_backoff();
    }

    /// Evaluate a hook kind end-to-end: run the ordered bindings, then apply
    /// the fault policy for any fault — commit one canonical `module_fault`
    /// per fault (ids/digests/counts only), then attempt a BOUNDED respawn and
    /// re-resolve the composition state. A failed/skipped respawn (or a failed
    /// fact commit) puts the binding into backoff: the built-in default keeps
    /// applying with no further fault fact or respawn until the next
    /// composition rebind (G).
    pub(crate) fn evaluate_hooks(
        &mut self,
        kind: HookKind,
        context_json: &str,
    ) -> HookEvaluation {
        let started = Instant::now();
        let evaluation = match self.modules.as_ref() {
            Some(manager) => self.hooks.evaluate(manager, kind, context_json),
            None => HookEvaluation::default_only(kind),
        };
        if evaluation.faults.is_empty() {
            return evaluation;
        }
        // Commit one fact per fault. A failed commit is NOT swallowed (H): the
        // binding is backed off and never respawned off a fact that never
        // landed.
        let mut fact_ok = vec![true; evaluation.faults.len()];
        for (i, fault) in evaluation.faults.iter().enumerate() {
            let payload = serde_json::json!({
                "module_id": fault.binding.module_id.to_string(),
                "package_digest": fault.binding.package_digest.to_string(),
                "generation": fault.binding.generation,
                "hook": fault.binding.hook.as_str(),
                "fault_class": fault_class(fault.error),
                "count": fault.binding.faults,
            });
            if self
                .commit(
                    vec![crate::NewEvent {
                        kind: "module_fault".into(),
                        payload_schema: 1,
                        payload,
                        objects: Vec::new(),
                        refs: Vec::new(),
                    }],
                    None,
                )
                .is_err()
            {
                fact_ok[i] = false;
                self.hooks.set_backoff(
                    &fault.binding.scope,
                    kind,
                    &fault.binding.name,
                );
            }
        }
        let mut respawned_any = false;
        let mut respawned_ids: Vec<Id128> = Vec::new();
        for (i, fault) in evaluation.faults.iter().enumerate() {
            if !fact_ok[i] {
                continue;
            }
            let (scope, name) = (fault.binding.scope.clone(), fault.binding.name.clone());
            if self.hooks.is_backed_off(&scope, kind, &name) {
                continue;
            }
            // NEW-4: recompute the per-decision budget every iteration — the
            // fact commits and every earlier respawn consumed wall time, so N
            // faulty bindings must not each get the full budget.
            let remaining = HOOK_WAIT.saturating_sub(started.elapsed());
            // Backoff when the drain cannot complete in the remaining budget,
            // or when this module already consumed its one respawn since the
            // last composition rebind (no per-decision storm, G).
            if remaining < MIN_RESPAWN_BUDGET
                || self.hook_respawned.contains(&fault.binding.module_id)
            {
                self.hooks.set_backoff(&scope, kind, &name);
                continue;
            }
            // F: capture the dead generation's grants before the respawn's
            // teardown retires them, so they can be re-pinned to the fresh
            // generation below.
            let rescued = self.broker.grants_for_generation(fault.binding.generation);
            let old_generation = fault.binding.generation;
            let respawned = match self.modules.as_mut() {
                Some(manager) => {
                    manager.respawn_bounded(fault.binding.module_id, remaining)
                }
                None => continue,
            };
            match respawned {
                Ok(generation) => {
                    self.hook_respawned.insert(fault.binding.module_id);
                    self.refresh_respawned_grants(&rescued, old_generation, generation);
                    respawned_any = true;
                    respawned_ids.push(fault.binding.module_id);
                }
                Err(_) => {
                    // The module is gone either way: drop its dead-generation
                    // grants rather than leave them pinned forever.
                    self.broker.retire_generation(old_generation);
                    self.hooks.set_backoff(&scope, kind, &name);
                }
            }
        }
        if respawned_any {
            self.rebind_after_respawn(&respawned_ids);
        }
        evaluation
    }

    /// Re-pin grants that named a respawned module's dead generation (F): the
    /// session broker is not the module host's broker, so its generation-bound
    /// grants (e.g. the builtin UI's append/cancel) survive a respawn still
    /// naming the dead generation and deny the fresh one. Retire the old pin,
    /// then re-issue the grants under the fresh generation through the same
    /// respawn choke point as the rebind. A failed re-add stays fail-closed
    /// (the module simply has no grant).
    fn refresh_respawned_grants(
        &mut self,
        rescued: &[Grant],
        old_generation: u64,
        generation: u64,
    ) {
        self.broker.retire_generation(old_generation);
        for grant in rescued {
            let mut grant = grant.clone();
            grant.principal.generation = generation;
            grant.module_generation = generation;
            grant.grant_digest = grant.derive_digest();
            let _ = self.broker.add_grant(grant);
        }
    }

    /// Re-resolve every generation-bound session state after a respawn (F):
    /// refresh the affected `ConfigLayer.generation`s, then run the ONE
    /// composition-change choke point (`rebind_ui`, which also rebinds hooks).
    fn rebind_after_respawn(&mut self, module_ids: &[Id128]) {
        if let Some(manager) = self.modules.as_ref() {
            let current: std::collections::HashMap<Id128, u64> = manager
                .snapshot()
                .into_iter()
                .map(|(id, generation, _)| (id, generation))
                .collect();
            for layer in &mut self.config_layers {
                if module_ids.contains(&layer.module_id)
                    && let Some(generation) = current.get(&layer.module_id)
                {
                    layer.generation = *generation;
                }
            }
        }
        let _ = self.rebind_ui(0);
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
            scope: ScopePath(vec!["root".into()]),
            name: name.to_string(),
            hook,
            degraded: false,
            faults: 0,
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
    fn annotations_are_bounded_in_count_and_total_size() {
        let many: Vec<Value> = (0..(ANNOTATION_COUNT_MAX + 10))
            .map(|i| json!({ "key": format!("k{i}"), "value": "v" }))
            .collect();
        let raw = json!({ "decision": "continue", "annotations": many }).to_string();
        let d = HookDecision::parse(&raw).unwrap();
        assert!(d.annotations.len() <= ANNOTATION_COUNT_MAX);

        // Nested structure counts toward the total-size cap, not only strings.
        let nested: Vec<Value> = (0..500)
            .map(|i| json!({ "key": format!("key-{i}"), "value": { "deep": [1, 2, 3, 4, 5] } }))
            .collect();
        let raw = json!({ "decision": "continue", "annotations": nested }).to_string();
        let d = HookDecision::parse(&raw).unwrap();
        let size = serde_json::to_string(&d.annotations).unwrap().len();
        assert!(size <= ANNOTATION_TOTAL_MAX, "total size {size}");
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
        // The deny reason is kernel-authored (C).
        match HookEvaluation::default_for(HookKind::OnToolIntent) {
            Decision::Deny { reason } => assert_eq!(reason, DENIED_REASON),
            other => panic!("expected deny, got {other:?}"),
        }
    }
}
