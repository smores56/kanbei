//! M3 agent spine (architecture.md R-09/E-09/E-10, R-17/E-02, R-18/E-01):
//! the run FSM commit paths, model/tool intent+outcome records with
//! origin_snapshot provenance (R-08, R-02/C-03), bounded approval queue
//! (R-17/H-05), dispatch re-verification (R-16/D-11/C-10), responder
//! priority (R-09/E-10), and interrupted/ambiguous classification at
//! recovery (B-05).

use kanbei_capabilities::{Capability, Principal};
use kanbei_context::pipeline::{default_stages, run_pipeline};
use kanbei_context::validator::ValidatorStage;
use kanbei_context::{
    BudgetSpec, Contradiction, EvidenceClaim, MemoryFragmentSource, ProjectionInput,
    ReasoningContinuity, RenderedEvent, RetrievedEvidence, SchemaFragment, SourceRef,
    TrajectoryView, TriggerFragment, lower, sensitivity_rank,
};
use kanbei_core::digest::Digest;
use kanbei_core::envelope::Envelope;
use kanbei_core::id::Id128;
use kanbei_memory::{
    Claim, ClaimEdge, ClaimProvenance, EdgeKind, IdempotencyKey, MEMORY_CLAIM_SCHEMA,
    MEMORY_EDGE_SCHEMA, MEMORY_ROOT_SCHEMA, MEMORY_TRANSITION_SCHEMA, MemoryError, MemoryScope,
    MemoryTransition, RootFold, RootManifest, TransitionKind, TransitionOutcome,
    derive_validation_status,
};
use kanbei_provider::{
    CacheOutcome, CachePlan, EgressEntry, FinishReason, Message, ModelCallRecord, Role,
};
use kanbei_retrieval::{ActiveMemoryProjector, SalienceInput, ScopeIndexInput, SearchQuery};
use kanbei_scopes::contrib::HookKind;
use kanbei_scheduler::{
    Budgets, FailureKind, RunId, RunKind, RunOutcome, RunStart, RunUsage, StepCommand, StepContext,
    StepResult, TerminalOutcome, Trigger, WakeDecision,
};
use kanbei_tools::{
    AWAITING_APPROVAL, ApprovalParked, OutcomeClassification, ToolIntent, ToolOutcome,
    approval_for, execute_tool, tool_call_id,
};
use std::sync::Arc;

use serde_json::{Value, json};

use crate::{NewEvent, ProjectionState, Session, SessionError};
use crate::hooks::Decision;

/// Truncate a string to at most `max` BYTES without splitting a UTF-8 code
/// point (C: `String::truncate` panics on a non-char boundary, and a guest
/// hook name/value can be multibyte).
fn truncate_utf8(s: &mut String, max: usize) {
    if s.len() <= max {
        return;
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
}

/// Tools whose execution is a consequential side effect: the committed
/// intent is flushed to durable storage before these run (fast/balanced
/// profiles otherwise acknowledge kernel-buffered writes).
fn consequential_tool(tool: &str) -> bool {
    matches!(
        tool,
        "fs.write"
            | "fs.patch"
            | "process.exec"
            | "child.spawn"
            | "memory.propose"
            | "memory.review"
            | "memory.promote"
    )
}

/// One accepted wake: the run FSM entry (R-09/E-10 — every accepted wake
/// creates exactly one RunId). The caller commits the canonical record and
/// then drives the run.
pub struct AcceptedRun {
    pub run_id: RunId,
    pub kind: RunKind,
    pub trigger: Trigger,
}

impl Session {
    // ---------- wake acceptance (R-09/E-09/E-10) ----------

    /// Observe a trigger (ephemeral — the scheduler batches it). Commit
    /// happens at accept time.
    pub fn observe_trigger(&mut self, trigger: Trigger) {
        self.scheduler.observe(trigger);
    }

    /// Accept the next wake under the policy, committing the canonical
    /// `wake_acceptance` (or `wake_denied` with the responsible constraint)
    /// record. Coalescing priority: when several batches are pending, a
    /// responder batch outranks background cognition (built-in policy); a
    /// running command is never preempted.
    pub fn accept_wake(&mut self) -> Result<Option<AcceptedRun>, SessionError> {
        self.fault(crate::FaultPoint::BeforeWakeAccept);
        let decision = self.scheduler.accept_wake(false);
        match decision {
            WakeDecision::Accepted(a) => {
                let trigger_kind = a.trigger_kind;
                let run = AcceptedRun {
                    run_id: a.run_id,
                    kind: a.kind,
                    trigger: Trigger {
                        kind: trigger_kind,
                        referent: a.trigger_digests.first().copied(),
                    },
                };
                self.commit(
                    vec![NewEvent {
                        kind: "wake_acceptance".into(),
                        payload_schema: 1,
                        payload: serde_json::to_value(&a).map_err(|e| {
                            SessionError::InvalidInput(format!("wake payload: {e}"))
                        })?,
                        objects: Vec::new(),
                        refs: Vec::new(),
                    }],
                    None,
                )?;
                self.fault(crate::FaultPoint::AfterWakeAccept);
                Ok(Some(run))
            }
            WakeDecision::Denied(d) => {
                self.commit(
                    vec![NewEvent {
                        kind: "wake_denied".into(),
                        payload_schema: 1,
                        payload: serde_json::to_value(&d).map_err(|e| {
                            SessionError::InvalidInput(format!("denial payload: {e}"))
                        })?,
                        objects: Vec::new(),
                        refs: Vec::new(),
                    }],
                    None,
                )?;
                Ok(None)
            }
        }
    }

    /// Commit the run-start record. Run genesis pins a manifest (R-08: run
    /// genesis is a state-changing transition): the manifest captures the
    /// environment the run executes under — module generations, memory roots,
    /// tool registry, provider/policy pins — which private updates advanced
    /// without pinning. The pin is content-addressed, so an unchanged
    /// environment dedups to the manifest already pinned.
    pub fn run_start(&mut self, run_id: RunId) -> Result<RunStart, SessionError> {
        self.fault(crate::FaultPoint::BeforeRunStart);
        let start = self.scheduler.run_start(run_id)?;
        #[cfg(feature = "otel")]
        self.telemetry_open_run(run_id);
        self.commit(
            vec![NewEvent {
                kind: "run_start".into(),
                payload_schema: 1,
                payload: serde_json::to_value(&start)
                    .map_err(|e| SessionError::InvalidInput(format!("run start payload: {e}")))?,
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            Some(self.composition.current().digest),
        )?;
        self.fault(crate::FaultPoint::AfterRunStart);
        Ok(start)
    }

    /// Commit the run terminal outcome; when a circuit breaker fires, commit
    /// the canonical `breaker_tripped` record and pause cognition until
    /// explicit user resume (R-17/E-02). Budget exhaustion is recorded as an
    /// explicit `Blocked` outcome (R-17/H-05).
    pub fn run_outcome(
        &mut self,
        run_id: RunId,
        outcome: TerminalOutcome,
        usage: RunUsage,
        action_digests: &[Digest],
    ) -> Result<Option<kanbei_scheduler::BreakerTrip>, SessionError> {
        self.run_outcome_with_reason(run_id, outcome, usage, action_digests, None)
    }

    /// The terminal-outcome path with a user-visible reason (Blocked's
    /// responsible constraint, R-17).
    pub fn run_outcome_with_reason(
        &mut self,
        run_id: RunId,
        outcome: TerminalOutcome,
        usage: RunUsage,
        action_digests: &[Digest],
        reason: Option<String>,
    ) -> Result<Option<kanbei_scheduler::BreakerTrip>, SessionError> {
        self.fault(crate::FaultPoint::BeforeRunOutcome);
        // A completed goal resolves the run's open loops (layer-2, R-12):
        // the promises the user made are addressed; failures leave them open.
        let completed_goal = outcome == TerminalOutcome::CompletedGoal;
        let (record, trip) = self.scheduler.record_outcome_reason(
            run_id,
            outcome,
            usage,
            action_digests,
            reason,
        )?;
        let mut events = vec![NewEvent {
            kind: "run_outcome".into(),
            payload_schema: 1,
            payload: serde_json::to_value(&record)
                .map_err(|e| SessionError::InvalidInput(format!("run outcome payload: {e}")))?,
            objects: Vec::new(),
            refs: Vec::new(),
        }];
        if let Some(t) = trip {
            events.push(NewEvent {
                kind: "breaker_tripped".into(),
                payload_schema: 1,
                payload: serde_json::to_value(t)
                    .map_err(|e| SessionError::InvalidInput(format!("breaker payload: {e}")))?,
                objects: Vec::new(),
                refs: Vec::new(),
            });
        }
        self.commit(events, None)?;
        if completed_goal {
            self.open_loops.clear();
        }
        #[cfg(feature = "otel")]
        self.telemetry_close_run(outcome, usage);
        #[cfg(feature = "otel")]
        self.telemetry_storage()?;
        #[cfg(feature = "otel")]
        self.telemetry_flush()?;
        self.fault(crate::FaultPoint::AfterRunOutcome);
        Ok(trip)
    }

    /// The active run's current usage (tests + crash child).
    pub fn scheduler_usage(&self, run_id: RunId) -> RunUsage {
        self.scheduler.current_usage(run_id)
    }

    /// Override the scheduler budgets (tests; the kernel config owns the
    /// production values).
    pub fn scheduler_budget_tokens_override(&mut self, tokens: u64) {
        self.scheduler.set_budgets(Budgets {
            tokens: Some(tokens),
            ..self.scheduler.budgets()
        });
    }

    /// Explicit user resume after a breaker trip (R-17/E-02).
    pub fn resume_cognition(&mut self) -> Result<(), SessionError> {
        self.scheduler.resume()?;
        self.commit(
            vec![NewEvent {
                kind: "cognition_resumed".into(),
                payload_schema: 1,
                payload: json!({}),
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            None,
        )?;
        Ok(())
    }

    /// Explicit cancellation (R-09/E-10): records the in-flight run's
    /// terminal outcome immediately as `Failed(UserCancelled)` — no boundary
    /// wait — and releases the run slot; committed intents are never rolled
    /// back. The Ctrl-C path is the
    /// [`SessionConfig::cancel_flag`] checked at stream/step boundaries in
    /// `cognition_loop`; this method is the immediate form (UI teardown,
    /// driver error cleanup). Single-owner-at-a-time — a wake arriving during
    /// a run is queued, never preempted. Returns the cancelled run's outcome
    /// record when one was active.
    pub fn cancel_active_run(&mut self) -> Result<Option<RunOutcome>, SessionError> {
        self.fail_active_run(
            kanbei_scheduler::FailureKind::UserCancelled,
            "cancelled by user".into(),
        )
    }

    /// Records the active run's terminal outcome with an explicit failure
    /// kind and reason, releasing the run slot. Used when a mid-run error is
    /// not a user cancel (e.g. a provider failure) so the canonical record
    /// classifies it truthfully rather than as `UserCancelled`. Returns the
    /// run's outcome record when one was active.
    pub fn fail_active_run(
        &mut self,
        kind: kanbei_scheduler::FailureKind,
        reason: String,
    ) -> Result<Option<RunOutcome>, SessionError> {
        let Some(run_id) = self.scheduler.active_run() else {
            return Ok(None);
        };
        let usage = self.scheduler.current_usage(run_id);
        let outcome = TerminalOutcome::Failed(kind);
        let (record, _) = self.scheduler.record_outcome_reason(
            run_id,
            outcome,
            usage,
            &[],
            Some(reason),
        )?;
        self.commit(
            vec![NewEvent {
                kind: "run_outcome".into(),
                payload_schema: 1,
                payload: serde_json::to_value(&record)
                    .map_err(|e| SessionError::InvalidInput(format!("run outcome payload: {e}")))?,
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            None,
        )?;
        #[cfg(feature = "otel")]
        self.telemetry_close_run(outcome, usage);
        Ok(Some(record))
    }

    // ---------- model calls (R-08/E-13 intent provenance) ----------

    /// One model call: commits the `model_call` intent record (rendered
    /// context hash + params + cache plan), invokes the provider engine, and
    /// commits the `model_outcome` record repeating the rendered digest
    /// (validated equal at outcome commit) plus the canonical egress entry
    /// (R-15). Usage is recorded against the run for budget/breaker
    /// accounting.
    pub fn model_call(
        &mut self,
        run_id: RunId,
        messages: Vec<Message>,
        selected_events: Vec<u64>,
        rendered: &str,
    ) -> Result<Value, SessionError> {
        self.fault(crate::FaultPoint::BeforeModelCall);
        let provider = self
            .provider_config
            .as_ref()
            .map(|c| c.provider.clone())
            .unwrap_or_else(|| "unknown".into());
        let model = self
            .provider_config
            .as_ref()
            .map(|c| c.model.clone())
            .unwrap_or_else(|| "unknown".into());
        let params = self
            .provider_config
            .as_ref()
            .map(|c| {
                json!({
                    "temperature": c.temperature,
                    "max_tokens": c.max_tokens,
                })
            })
            .unwrap_or(json!({}));
        let rendered_hash = Digest::new(rendered.as_bytes());
        // M4 staged projection pins: the fragment-list digest, the lowering's
        // cache plan, and the exact memory roots the request was built
        // against (R-08/E-13, R-11).
        let projection_hashes = self
            .projection_state
            .as_ref()
            .map(|p| vec![p.projection_digest])
            .unwrap_or_default();
        let cache_plan = self
            .projection_state
            .as_ref()
            .map(|p| p.cache_plan)
            .unwrap_or(CachePlan::None);
        let memory_roots = self
            .projection_state
            .as_ref()
            .map(|p| p.memory_roots.clone())
            .unwrap_or_default();
        let intent = ModelCallRecord {
            provider: provider.clone(),
            model: model.clone(),
            projection_hashes,
            module_hashes: Vec::new(),
            selected_events: selected_events.clone(),
            rendered_hash,
            params: params.clone(),
            cache_plan,
            cache_outcome: CacheOutcome::Miss,
            memory_roots,
            input_tokens: 0,
            output_tokens: 0,
            finish_reason: FinishReason::Stop,
        };
        let origin_snapshot = self.current_snapshot;
        // All provider/params data captured; release the engine borrow before
        // the intent commit (commit borrows self mutably).
        let provider_id = provider.clone();
        let model_id = model.clone();
        let temperature = self.provider_config.as_ref().and_then(|c| c.temperature);
        let max_tokens = self.provider_config.as_ref().and_then(|c| c.max_tokens);
        self.commit(
            vec![NewEvent {
                kind: "model_call".into(),
                payload_schema: 1,
                payload: serde_json::to_value(&intent)
                    .map_err(|e| SessionError::InvalidInput(format!("model call payload: {e}")))?,
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            None,
        )?;
        let Some(engine) = self.provider.as_ref() else {
            return Err(SessionError::InvalidInput("no provider configured".into()));
        };
        let request = kanbei_provider::CompletionRequest {
            model: model_id.clone(),
            messages,
            tools: self
                .tool_registry
                .canonical_json()
                .as_array()
                .cloned()
                .unwrap_or_default(),
            temperature,
            max_tokens,
            // R-18/E-07: opaque reasoning artifacts replay only to the same
            // provider (transferability default: NONE, architecture E-07).
            opaque_artifacts: match &self.last_opaque {
                Some((prev, artifacts)) if *prev == provider => Some(artifacts.clone()),
                _ => None,
            },
            tool_calls: Vec::new(),
        };
        // Streaming call (decision 13): the provider reads its SSE stream
        // incrementally so a user cancel lands at a stream boundary, rather
        // than waiting for the whole (blocking) response. Engines without a
        // stream fall back to one `complete` call via the trait default.
        let cancel = self.cancel_flag.clone();
        let no_cancel = std::sync::atomic::AtomicBool::new(false);
        let cancel_ref = cancel.as_deref().unwrap_or(&no_cancel);
        let listener = self.delta_listener.clone();
        // Decision 30: partials have a home in the transcript projection; they
        // are non-canonical and never committed. The transcript-view observer
        // fires per delta so the in-flight partial is observable.
        let transcript_listener = self.transcript_listener.clone();
        let transcript = &mut self.transcript;
        let mut on_delta = |fragment: &str| {
            transcript.apply_delta(fragment);
            crate::transcript::fire_transcript_view(transcript.as_ref(), &transcript_listener);
            if let Some(listener) = &listener {
                listener(fragment);
            }
        };
        let streamed = engine.complete_stream(&request, cancel_ref, &mut on_delta);
        // The stream ended (success or cancel/error): clear the partial.
        self.transcript.end_stream();
        self.notify_transcript();
        let response = streamed.map_err(|e| match e {
            kanbei_provider::ProviderError::Cancelled { .. } => SessionError::Cancelled,
            e => SessionError::Provider(e.to_string()),
        })?;
        self.fault(crate::FaultPoint::AfterModelCall);

        let result = json!({
            "content": response.content,
            "tool_calls": response.tool_calls,
            "finish_reason": response.finish_reason,
            "usage": response.usage,
        });
        // M4 cache outcome: the provider served the cached stable prefix only
        // when the plan digest AND the live memory roots still match the last
        // call's pins; a root change invalidates the cached prefix even when
        // the plan digest is unchanged (the projection may be stale relative
        // to the actors).
        let cache_outcome = match self.projection_state.as_ref() {
            None => CacheOutcome::Miss,
            Some(p) => match p.cache_plan {
                CachePlan::None => CacheOutcome::Miss,
                CachePlan::StablePrefix { digest } => match &self.last_cache {
                    Some((Some(prev), prev_roots)) if *prev == digest => {
                        // M6 pinned-at follow: the roots the projection was
                        // built against are the pinned roots, not the live
                        // heads — a pinned projection is stable across actor
                        // transitions and the cache stays valid while the
                        // pinned roots are unchanged.
                        let live: Vec<Digest> = match &self.pinned_roots {
                            // same order as project_context's memory_roots:
                            // lifetime first, then the project root.
                            Some(p) => [Some(p.lifetime), p.project]
                                .into_iter()
                                .flatten()
                                .collect(),
                            None => [
                                self.memory_lifetime.head(),
                                self.memory_project.as_ref().and_then(|a| a.head()),
                            ]
                            .into_iter()
                            .flatten()
                            .collect(),
                        };
                        if live == *prev_roots {
                            CacheOutcome::Hit
                        } else {
                            CacheOutcome::Invalidated {
                                reason: "memory root changed".into(),
                            }
                        }
                    }
                    _ => CacheOutcome::Miss,
                },
            },
        };
        // R-18/E-07: reasoning continuity — Broken on the first call after a
        // provider change, at this outcome event's seq (the next commit is
        // the outcome; single writer).
        let at_event = self.next_seq;
        // R-18/E-07: the model's own discontinuity flag takes precedence over
        // the provider-change heuristic — its reasoning does not follow from
        // the projection even on the same provider.
        let continuity = if let Some(flag) = &response.discontinuity {
            ReasoningContinuity::Broken {
                from_provider: provider.clone(),
                at_event,
                reason: Some(flag.clone()),
            }
        } else {
            match &self.last_provider {
                Some(prev) if *prev == provider => ReasoningContinuity::Continuous,
                _ => ReasoningContinuity::Broken {
                    from_provider: self.last_provider.clone().unwrap_or_else(|| "none".into()),
                    at_event,
                    reason: None,
                },
            }
        };
        let outcome = json!({
            "provider": provider_id,
            "model": model_id,
            // R-08/E-13: the outcome repeats the intent's rendered digest;
            // equality is enforced by construction — both serialize the same
            // `rendered_hash` the session itself rendered (M3 behavior).
            "rendered_hash": rendered_hash.to_string(),
            "result": result,
            "cache_outcome": serde_json::to_value(&cache_outcome)
                .map_err(|e| SessionError::InvalidInput(format!("cache outcome: {e}")))?,
            "reasoning_continuity": serde_json::to_value(&continuity)
                .map_err(|e| SessionError::InvalidInput(format!("continuity: {e}")))?,
            // R-18/E-07: the opaque artifact round-trip is byte-exact — the
            // base64 string is recorded verbatim (S9 acceptance); the raw
            // discontinuity flag is kept for audit. Artifacts never enter
            // the projection/context.
            "opaque_artifacts": response.opaque_artifacts,
            "discontinuity": response.discontinuity,
            "egress": EgressEntry {
                provider: provider.clone(),
                sensitivity_classes: vec!["call".into()],
                origin_snapshot,
                input_tokens: response.usage.input_tokens,
                output_tokens: response.usage.output_tokens,
            },
        });
        self.commit(
            vec![NewEvent {
                kind: "model_outcome".into(),
                payload_schema: 1,
                payload: outcome,
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            None,
        )?;
        // The next call resolves its cache outcome against these pins.
        self.last_cache = Some((
            self.projection_state
                .as_ref()
                .and_then(|p| match p.cache_plan {
                    CachePlan::StablePrefix { digest } => Some(digest),
                    CachePlan::None => None,
                }),
            self.projection_state
                .as_ref()
                .map(|p| p.memory_roots.clone())
                .unwrap_or_default(),
        ));
        // R-18/E-07: keep the last artifacts paired with their provider — a
        // later same-provider call replays them even when an intervening call
        // emitted none; cross-provider transfer stays prohibited.
        if let Some(artifacts) = &response.opaque_artifacts {
            self.last_opaque = Some((provider.clone(), artifacts.clone()));
        }
        self.last_provider = Some(provider);
        self.scheduler.record_usage(
            run_id,
            RunUsage {
                tokens: response.usage.input_tokens + response.usage.output_tokens,
                tools: 0,
                children: 0,
                started_at_secs: 0,
            },
        )?;
        Ok(result)
    }

    // ---------- tool FSM (B-05, R-16/D-11/C-12, R-16/D-12) ----------

    /// Commit a tool intent, run the approval gate, dispatch with
    /// re-verification, and commit the tool outcome. The intent is committed
    /// BEFORE dispatch (B-05); a committed intent without an outcome is the
    /// sufficient condition for interrupted/ambiguous classification at
    /// recovery.
    pub fn tool_call(
        &mut self,
        run_id: RunId,
        principal: Principal,
        tool: &str,
        args: Value,
    ) -> Result<ToolOutcome, SessionError> {
        let schema = self
            .tool_registry
            .get(tool)
            .ok_or_else(|| SessionError::InvalidInput(format!("unknown tool {tool}")))?;
        let _ = schema;
        let call_id = tool_call_id();
        let mut intent = ToolIntent {
            call_id,
            run_id,
            principal: principal.clone(),
            tool: tool.into(),
            args: kanbei_tools::canonicalize(args),
            approval: None,
            origin_snapshot: self.current_snapshot,
            intent_event: None,
            annotations: Vec::new(),
        };
        // T9: `on_tool_intent` hooks run after intent construction and BEFORE
        // the approval gate/dispatch. Annotations ride the committed intent;
        // a deny short-circuits (the broker is never consulted).
        let hook_context = json!({
            "hook": HookKind::OnToolIntent.as_str(),
            "tool": intent.tool,
            "args": intent.args,
        });
        let hooks = self.evaluate_hooks(HookKind::OnToolIntent, &hook_context.to_string());
        intent.annotations = hooks.annotations.clone();
        self.fault(crate::FaultPoint::BeforeToolIntentCommit);
        let intent_payload = serde_json::to_value(&intent)
            .map_err(|e| SessionError::InvalidInput(format!("tool intent payload: {e}")))?;
        let receipt = self.commit(
            vec![NewEvent {
                kind: "tool_intent".into(),
                payload_schema: 1,
                payload: intent_payload,
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            None,
        )?;
        self.fault(crate::FaultPoint::AfterToolIntentCommit);
        // The committed intent's seq is the provenance anchor for the memory
        // proposal it leads to (R-11); the committed payload itself carries
        // null (the seq is unknowable before the commit).
        intent.intent_event = Some(receipt.last_seq);

        // T9: a hook deny is terminal for the intent — commit the canonical
        // denied outcome (constant kernel reason; the denying hook's
        // kernel-derived ids/digests) and never touch the approval/dispatch
        // path.
        if let Decision::Deny { .. } = hooks.decision {
            let outcome = ToolOutcome {
                call_id: intent.call_id,
                tool: intent.tool,
                result: Value::Null,
                error: None,
                classification: OutcomeClassification::Denied(
                    crate::hooks::DENIED_REASON.to_string(),
                ),
                origin_snapshot: intent.origin_snapshot,
                commit_snapshot: self.current_snapshot,
                retained: None,
                hook_denied: hooks.denier.as_ref().map(|denier| kanbei_tools::HookDenier {
                    module_id: denier.module_id.to_string(),
                    package_digest: denier.package_digest.to_string(),
                    hook: denier.hook.as_str().to_string(),
                    decision_digest: hooks.decision.digest().to_string(),
                }),
            };
            self.commit_tool_outcome(&outcome)?;
            return Ok(outcome);
        }

        // Approval gate: consequential tools require an approval intent;
        // the gate parks the intent-shaped approval (R-16/D-12: the digest
        // binds the exact committed intent) and the tool resolves
        // `Interrupted` until the user resolves it — the caller may NEVER
        // self-approve and dispatch in the same breath.
        let want = Capability::new(tool.into(), vec!["call".into()]);
        let approval_digest = match self.check_approval(&principal, &want, &intent) {
            Ok(Some(d)) => Some(d),
            Ok(None) => None,
            Err(e) => {
                return Ok(self.outcome_interrupted(intent, format!("approval denied: {e}")));
            }
        };
        if let Some(digest) = approval_digest {
            // A parked gate is a resolution point, not a green light: the
            // committed intent stays put (B-05 — its outcome arrives when
            // `resolve_approval` dispatches, or recovery classifies it).
            return Ok(ToolOutcome {
                call_id: intent.call_id,
                tool: intent.tool,
                result: Value::Null,
                error: None,
                classification: OutcomeClassification::Interrupted(format!(
                    "{AWAITING_APPROVAL}: {digest}"
                )),
                origin_snapshot: intent.origin_snapshot,
                commit_snapshot: self.current_snapshot,
                retained: None,
                hook_denied: None,
            });
        }
        intent.approval = None;

        // Dispatch-time re-verification (R-16/D-11/C-10): revoked ⇒ the
        // intent resolves `interrupted` with a user-visible reason.
        self.fault(crate::FaultPoint::BeforeToolDispatch);
        let outcome = self.dispatch_tool(run_id, &intent, principal)?;
        self.fault(crate::FaultPoint::AfterToolDispatch);
        Ok(outcome)
    }

    
/// A driver-side resolver decision on the newest parked approval (the
/// cognition-loop path): `None` = no resolver or it declined — the intent
/// stays parked for explicit `resolve_approval`; otherwise the resolved
/// outcome (already dispatched + committed by the resolve).
fn resolve_parked_via_driver(&mut self) -> Result<Option<ToolOutcome>, SessionError> {
    let Some(last) = self.approvals.back().cloned() else {
        return Ok(None);
    };
    let digest = last.approval.digest;
    let Some(resolver) = self.approval_resolver.clone() else {
        return Ok(None);
    };
    // Present the parked approval before the resolver blocks the driver, so
    // the user sees the gate and the action it binds.
    self.fire_present_hook();
    if !resolver(&last) {
        return Ok(None);
    }
    self.resolve_approval(&digest, true)
}

/// The approval gate: tools whose capability requires approval park in
    /// the bounded queue; `Ok(Some(digest))` = gated (parked, awaiting the
    /// user); `Ok(None)` = not gated. `Err` = explicitly denied by policy.
    /// The parked approval binds the exact committed intent (R-16/D-12) plus
    /// the policy/grant version snapshot the dispatch-time re-verification
    /// compares against (R-16/D-11/C-10).
    fn check_approval(
        &mut self,
        principal: &Principal,
        want: &Capability,
        intent: &ToolIntent,
    ) -> Result<Option<Digest>, String> {
        if self.approval_bound == 0 {
            return Ok(None);
        }
        let policy_version = self.broker.policy_version();
        let grants_version = self.broker.grants_version();
        match self.broker.check(principal, want, policy_version) {
            Ok(effective) => {
                if !effective.requires_approval {
                    return Ok(None);
                }
                // Exact-approval digest (R-16/D-12): the parked intent binds
                // the committed canonical args, not the empty default the
                // broker placeholder used — a changed intent cannot ride a
                // parked approval.
                let approval = approval_for(
                    principal,
                    &intent.tool,
                    &intent.args,
                    None,
                    kanbei_capabilities::GrantScope::Run,
                    None,
                );
                let digest = approval.digest;
                self.approvals.push_back(ApprovalParked {
                    intent: intent.clone(),
                    approval,
                    policy_version,
                    grants_version,
                });
                while self.approvals.len() > self.approval_bound {
                    // Overflow resolves the evicted intent `Interrupted`
                    // (R-17/H-05): the entry is gone, so its resolution is
                    // the next dispatch's rejection — re-approval is a NEW
                    // intent, never a resurrected digest.
                    self.approvals.pop_front();
                }
                Ok(Some(digest))
            }
            Err(e) => Err(e.to_string()),
        }
    }

    /// The digests of parked approval intents awaiting user resolution
    /// (R-16/D-12; re-approval is a new intent, never a resurrected one).
    pub fn pending_approvals(&self) -> Vec<Digest> {
        self.approvals.iter().map(|p| p.approval.digest).collect()
    }

    /// Resolves a parked approval (the user's approve/deny decision).
    /// `None` = the digest is not parked anymore (already resolved, or
    /// evicted by the bound — re-approval is a new intent).
    /// Approval re-derives the digest from the parked committed intent,
    /// re-runs the guards, and verifies the policy/grant versions captured
    /// at park time are unchanged (R-16/D-11/C-10); a mismatch resolves the
    /// intent `Interrupted` with the user-visible reason. The approved
    /// dispatch commits its outcome like any other tool outcome.
    pub fn resolve_approval(
        &mut self,
        digest: &Digest,
        approve: bool,
    ) -> Result<Option<ToolOutcome>, SessionError> {
        let position = self
            .approvals
            .iter()
            .position(|p| p.approval.digest == *digest);
        let Some(position) = position else {
            return Ok(None);
        };
        let parked = self.approvals[position].clone();
        if !approve {
            self.approvals.remove(position);
            return Ok(Some(
                self.outcome_interrupted(parked.intent, "approval denied by user".into()),
            ));
        }
        // Dispatch-time re-verification (R-16/D-11/C-10): the digest is
        // re-derived from the parked committed intent; the version snapshot
        // is the one captured when the gate parked it.
        let expected = approval_for(
            &parked.approval.principal,
            &parked.intent.tool,
            &parked.intent.args,
            None,
            kanbei_capabilities::GrantScope::Run,
            None,
        );
        if expected.digest != parked.approval.digest {
            return Ok(Some(self.outcome_interrupted(
                parked.intent,
                "approval stale: the committed intent no longer matches the approved digest".into(),
            )));
        }
        if let Err(e) = self.broker.recheck(
            &parked.approval,
            parked.grants_version,
            parked.policy_version,
        ) {
            return Ok(Some(
                self.outcome_interrupted(parked.intent, format!("approval recheck: {e}")),
            ));
        }
        // A parked approval outlives its initiating run: the queue holds
        // intents between runs, and resolving one whose run has closed
        // still commits the outcome as a fact (B-02: outcomes of already-
        // dispatched host work are always committed) — run usage accounting
        // simply has nothing to add to anymore (dispatch is NotActiveRun
        // tolerant).
        let mut intent = parked.intent;
        intent.approval = Some(parked.approval.digest);
        let principal = intent.principal.clone();
        let run_id = intent.run_id;
        // The parked entry stays in place through the dispatch (the
        // dispatch re-check validates its presence + digest), then the
        // resolution is one-shot: the entry is removed on both paths.
        let outcome = self.dispatch_tool(run_id, &intent, principal)?;
        if let Some(position) = self.approvals.iter().position(|p| p.approval.digest == *digest) {
            self.approvals.remove(position);
        }
        self.commit_tool_outcome(&outcome)?;
        Ok(Some(outcome))
    }

    fn dispatch_tool(
        &mut self,
        run_id: RunId,
        intent: &ToolIntent,
        principal: Principal,
    ) -> Result<ToolOutcome, SessionError> {
        // fsync-before-consequential-effect (architecture.md:408): under
        // every durability profile, the committed intent frame is durable
        // before a consequential effect runs — a crash between effect and
        // outcome must classify, never orphan.
        if consequential_tool(&intent.tool) {
            self.flush()?;
        }
        // Dispatch-time re-verification (R-16/D-11/C-10): re-run the broker
        // guard set against the current composition; revoked ⇒ the intent
        // resolves `interrupted` with a user-visible reason. Approval-gated
        // tools additionally require their parked approval entry to still be
        // valid at dispatch (re-approval is a new intent).
        let want = Capability::new(intent.tool.clone(), vec!["call".into()]);
        match self
            .broker
            .check(&principal, &want, self.broker.policy_version())
        {
            Ok(effective) => {
                if effective.requires_approval && intent.approval.is_none() {
                    return Ok(self.outcome_interrupted(
                        intent.clone(),
                        "approval required but missing at dispatch".into(),
                    ));
                }
            }
            Err(e) => {
                return Ok(
                    self.outcome_interrupted(intent.clone(), format!("dispatch recheck: {e}"))
                );
            }
        }
        if let Some(approval) = intent.approval {
            let parked = self
                .approvals
                .iter()
                .find(|p| p.approval.digest == approval);
            match parked {
                Some(p) if p.approval.validate() => {}
                _ => {
                    return Ok(self.outcome_interrupted(
                        intent.clone(),
                        "approval revoked or stale at dispatch".into(),
                    ));
                }
            }
        }

        // M4 memory substrate + child-run routing: the kernel-owned
        // dispatchers, never the native tool path.
        match intent.tool.as_str() {
            "memory.query" => return self.dispatch_memory_query(run_id, intent, principal),
            "memory.propose" => return self.dispatch_memory_propose(run_id, intent, principal),
            "memory.review" => return self.dispatch_memory_review(run_id, intent, principal),
            "memory.promote" => return self.dispatch_memory_promote(run_id, intent, principal),
            "child.spawn" => return self.dispatch_child_spawn(run_id, intent, principal),
            _ => {}
        }

        let result = match execute_tool(
            &mut self.native_tools,
            &self.tool_registry,
            &intent.tool,
            &intent.args,
            &self.fs_root,
        ) {
            Ok(v) => v,
            Err(e) => {
                return Ok(ToolOutcome {
                    call_id: intent.call_id.clone(),
                    tool: intent.tool.clone(),
                    result: Value::Null,
                    error: Some(e.to_string()),
                    classification: OutcomeClassification::Normal,
                    origin_snapshot: intent.origin_snapshot,
                    commit_snapshot: self.current_snapshot,
                    retained: None,
                    hook_denied: None,
                });
            }
        };
        // Usage accounting is NotActiveRun-tolerant at dispatch: an
        // approval resolved after its run closed still commits the fact.
        if let Err(e) = self.scheduler.record_usage(
            run_id,
            RunUsage {
                tokens: 0,
                tools: 1,
                children: 0,
                started_at_secs: 0,
            },
        ) {
            match e {
                kanbei_scheduler::SchedulerError::NotActiveRun(_) => {}
                other => return Err(other.into()),
            }
        }
        Ok(ToolOutcome {
            call_id: intent.call_id.clone(),
            tool: intent.tool.clone(),
            result,
            error: None,
            classification: OutcomeClassification::Normal,
            origin_snapshot: intent.origin_snapshot,
            commit_snapshot: self.current_snapshot,
            retained: None,
            hook_denied: None,
        })
    }

    fn outcome_interrupted(&self, intent: ToolIntent, reason: String) -> ToolOutcome {
        ToolOutcome {
            call_id: intent.call_id,
            tool: intent.tool,
            result: Value::Null,
            error: None,
            classification: OutcomeClassification::Interrupted(reason),
            origin_snapshot: intent.origin_snapshot,
            commit_snapshot: self.current_snapshot,
            retained: None,
            hook_denied: None,
        }
    }

    /// Commit a tool outcome record (the committed intent's fact — R-02).
    /// The outcome's result runs the retention gate FIRST (R-28/D-S1: tools
    /// emit candidates; policy decides before persistence); a boundary fact
    /// commits alongside the outcome. The gate's bytes are what commit: a
    /// `Transform` redaction must reach storage, not just a fee note
    /// (R-04/D-07). A policy error is fail-closed — unclassified candidate
    /// bytes never commit; the outcome classifies `Interrupted`.
    pub fn commit_tool_outcome(&mut self, outcome: &ToolOutcome) -> Result<(), SessionError> {
        self.fault(crate::FaultPoint::BeforeToolOutcomeCommit);
        let mut outcome = outcome.clone();
        let mut boundary: Option<kanbei_policy::BoundaryFact> = None;
        if outcome.error.is_none() && outcome.result != Value::Null {
            let candidate = kanbei_policy::Candidate {
                role: kanbei_policy::CandidateRole::ToolOutput,
                content: serde_json::to_vec(&outcome.result)
                    .map_err(|e| SessionError::InvalidInput(format!("candidate: {e}")))?,
                replay_relevant: self
                    .policy
                    .replay_relevant(kanbei_policy::CandidateRole::ToolOutput, None),
                sensitivity: None,
                media: Some("application/json".into()),
            };
            match self.policy.admit(candidate) {
                Ok(admission) => {
                    boundary = self.policy.boundary_fact(&admission);
                    match admission {
                        kanbei_policy::Admission::Stored { bytes } => {
                            // The gate stores exactly these bytes; the
                            // canonical outcome carries them, not the raw
                            // result a Transform replaced.
                            outcome.result = serde_json::from_slice(&bytes).unwrap_or_else(|_| {
                                Value::String(String::from_utf8_lossy(&bytes).to_string())
                            });
                            outcome.retained = Some(true);
                        }
                        kanbei_policy::Admission::Dropped { .. } => {
                            // A permitted Drop never stores the candidate.
                            outcome.result = Value::Null;
                            outcome.retained = Some(false);
                        }
                        _ => {}
                    }
                }
                Err(e) => {
                    outcome.result = Value::Null;
                    outcome.retained = None;
                    outcome.classification =
                        OutcomeClassification::Interrupted(format!("retention gate failed closed: {e}"));
                }
            }
        }
        let mut events = Vec::new();
        if let Some(fact) = boundary {
            events.push(NewEvent {
                kind: "retention_boundary".into(),
                payload_schema: 1,
                payload: json!({
                    "reason": fact.reason,
                    "replay_relevant": fact.replay_relevant,
                    "kind": match fact.kind {
                        kanbei_policy::BoundaryKind::NonResumable => "non_resumable",
                        kanbei_policy::BoundaryKind::Rejected => "rejected",
                    },
                }),
                objects: Vec::new(),
                refs: Vec::new(),
            });
        }
        events.push(NewEvent {
            kind: "tool_outcome".into(),
            payload_schema: 1,
            payload: serde_json::to_value(outcome)
                .map_err(|e| SessionError::InvalidInput(format!("tool outcome payload: {e}")))?,
            objects: Vec::new(),
            refs: Vec::new(),
        });
        self.commit(events, None)?;
        self.fault(crate::FaultPoint::AfterToolOutcomeCommit);
        Ok(())
    }

    // ---------- M4 context projection (staged pipeline) ----------

    /// Materialize the typed staged projection for one run: the harness
    /// contract, canonical tool schemas, the bounded recent-event trajectory
    /// (ranges cover the full canonical history; the CONTENT is the ring
    /// render), salience-scored memory evidence, the scope-stable memory
    /// fragments, and the current trigger — run through the kernel pipeline
    /// and lowered into provider messages (the model-call request source).
    /// Children project against the project fold only (attenuation); their
    /// budget comes from the clamped ChildRun record.
    pub fn project_context(
        &mut self,
        run_id: RunId,
        trigger: &Trigger,
    ) -> Result<StepContext, SessionError> {
        const HARNESS_CONTRACT: &str = "kanbei kernel harness contract v1\nstable harness contract; deterministic tool/module schemas; stable memory; conversation prefix; volatile active memory; current trigger.";
        let frozen_seq = self.next_seq.saturating_sub(1);
        let is_child = self.scheduler.run_kind(run_id) == Some(RunKind::Child);

        // Deterministic canonical tool/module schemas (sorted; each fragment
        // is the schema object's canonical bytes).
        let mut schemas = Vec::new();
        if let Some(arr) = self.tool_registry.canonical_json().as_array() {
            for schema in arr {
                let name = schema
                    .get("name")
                    .and_then(|n| n.as_str())
                    .unwrap_or_default()
                    .to_string();
                let bytes = serde_json::to_vec(schema).map_err(|e| {
                    SessionError::InvalidInput(format!("schema serialization: {e}"))
                })?;
                let text = String::from_utf8(bytes.clone())
                    .map_err(|_| SessionError::InvalidInput("schema bytes are not utf-8".into()))?;
                schemas.push(SchemaFragment {
                    id: name,
                    digest: Digest::new(&bytes),
                    text,
                    sensitivity: "public".into(),
                });
            }
        }

        // Trajectory: the frozen prefix; content is the bounded ring render
        // (compact payload JSON, truncated), filtered to the current
        // branch's path — abandoned tails never enter the projection (M6).
        let mut events: Vec<RenderedEvent> = self
            .recent_events
            .iter()
            .filter(|(seq, _, _)| self.on_path(*seq))
            .map(|(seq, kind, payload)| {
                let mut text = serde_json::to_string(payload).unwrap_or_default();
                truncate_utf8(&mut text, 512);
                RenderedEvent {
                    seq: *seq,
                    kind: kind.clone(),
                    text,
                    sensitivity: "internal".into(),
                }
            })
            .collect();
        if frozen_seq == 0 {
            events = Vec::new();
        }
        // The conv.prefix coverage: the current branch's path ranges
        // intersected with the frozen prefix (the prefix never claims
        // coverage over an abandoned tail, R-05 chronology).
        let selected_ranges = if frozen_seq >= 1 {
            self.path_ranges()
                .into_iter()
                .filter_map(|(start, end)| {
                    let start = start.max(1);
                    let end = end.min(frozen_seq);
                    (start <= end).then_some((start, end))
                })
                .collect::<Vec<_>>()
        } else {
            Vec::new()
        };

        // Memory folds. M6 pinned-at follow: with pinned roots the folds
        // resolve against the checkpoint-era roots (folding the pinned root
        // yields the historical claim set); otherwise the live actor heads.
        let lifetime_root = match &self.pinned_roots {
            Some(p) => Some(p.lifetime),
            None => self.memory_lifetime.head(),
        };
        let lifetime_fold = match lifetime_root {
            Some(root) => Some(
                self.memory_lifetime
                    .fold(Some(root))
                    .map_err(SessionError::Memory)?,
            ),
            None => None,
        };
        let project_root = match &self.pinned_roots {
            Some(p) => p.project,
            None => self.memory_project.as_ref().and_then(|a| a.head()),
        };
        let project_fold = match project_root {
            Some(root) => Some(
                self.memory_project
                    .as_ref()
                    .expect("project root implies actor")
                    .fold(Some(root))
                    .map_err(SessionError::Memory)?,
            ),
            None => None,
        };

        // Salience: children see the project fold only (empty without one);
        // parents prefer the project fold, falling back to lifetime.
        let salience_fold = if is_child {
            project_fold.clone().unwrap_or_else(empty_fold)
        } else {
            project_fold
                .clone()
                .unwrap_or_else(|| lifetime_fold.clone().unwrap_or_else(empty_fold))
        };
        let recent_causal: Vec<u64> = self
            .recent_events
            .iter()
            .filter(|(seq, _, _)| self.on_path(*seq))
            .map(|(seq, _, _)| *seq)
            .collect();
        let projector = ActiveMemoryProjector::new();
        let (active_view, scored) = projector
            .project(
                &SalienceInput {
                    frozen_seq,
                    recent_causal,
                    // R-12/F-S5 layer-2: the canonical pins/open loops derived
                    // from committed facts make the goals/pins salience
                    // components live instead of permanently zero.
                    open_loops: self.open_loops.clone(),
                    pins: self.active_pins.clone(),
                    fold: salience_fold.clone(),
                    top_n: 32,
                },
                &mut self.memory_index,
            )
            .map_err(SessionError::Retrieval)?;
        let active_ids: Vec<Id128> = salience_fold
            .claims
            .iter()
            .map(|(_, c)| c.claim_id)
            .collect();
        let edges: Vec<ClaimEdge> = salience_fold.edges.iter().map(|(_, e)| e.clone()).collect();
        let evidence: Vec<EvidenceClaim> = scored
            .iter()
            .map(|sc| {
                let claim = &sc.claim;
                let mut contradictions = Vec::new();
                for (_, edge) in &salience_fold.edges {
                    if edge.to == Some(claim.claim_id)
                        && matches!(edge.kind, EdgeKind::Contradicts | EdgeKind::Supersedes)
                        && let Some((digest, from_claim)) = salience_fold
                            .claims
                            .iter()
                            .chain(salience_fold.retracted.iter())
                            .find(|(_, c)| c.claim_id == edge.from)
                    {
                        contradictions.push(Contradiction {
                            digest: *digest,
                            text: from_claim.content.clone(),
                            supersedes: edge.kind == EdgeKind::Supersedes,
                        });
                    }
                }
                EvidenceClaim {
                    digest: sc.digest,
                    text: claim.content.clone(),
                    kind: claim.kind.clone(),
                    sensitivity: claim.sensitivity.clone(),
                    status: derive_validation_status(claim.claim_id, &active_ids, &edges),
                    score: sc.score,
                    contradictions,
                    source_events: if claim.provenance.event > 0 {
                        vec![claim.provenance.event]
                    } else {
                        Vec::new()
                    },
                }
            })
            .collect();

        // Scope-stable memory fragments: lifetime always when non-empty,
        // project when bound. Children get NO lifetime fragment (R-12:
        // children do not see user/lifetime memory by default — the
        // projection must not hand what memory.query's scope resolution
        // would refuse; the child fold attenuates salience the same way).
        let lifetime = match (&lifetime_root, &lifetime_fold) {
            (Some(root), Some(fold))
                if !fold.claims.is_empty() && !is_child =>
            {
                Some(render_memory_source(*root, fold))
            }
            _ => None,
        };
        let project = match (&project_root, &project_fold) {
            (Some(root), Some(fold)) if !fold.claims.is_empty() => {
                Some(render_memory_source(*root, fold))
            }
            _ => None,
        };

        let trigger_fragment = TriggerFragment {
            kind: format!("{:?}", trigger.kind),
            // The referent text when present, else the kind name — the
            // fragment builder rejects empty content, so a referent-less
            // trigger still materializes a non-empty fragment.
            text: trigger
                .referent
                .map(|d| d.to_string())
                .unwrap_or_else(|| format!("{:?}", trigger.kind)),
            sensitivity: "internal".into(),
        };
        // MVP projection budgets (documented constants).
        let budgets = BudgetSpec {
            max_total_tokens: 8192,
            max_volatile_tokens: 4096,
        };
        let read = move |src: &SourceRef| match src {
            SourceRef::Harness => true,
            SourceRef::ModuleSchema(_) => true,
            SourceRef::SessionEvent(seq) => *seq <= frozen_seq,
            SourceRef::MemoryClaim(_) => true,
            SourceRef::CompactionRange(..) => true,
        };
        let input = ProjectionInput {
            harness_contract: HARNESS_CONTRACT.into(),
            schemas,
            lifetime,
            project,
            compaction: None,
            trajectory: TrajectoryView {
                frozen_seq,
                selected_ranges,
                selected_events: Vec::new(),
                events,
            },
            active: active_view,
            evidence: RetrievedEvidence { claims: evidence },
            trigger: trigger_fragment,
            budgets,
        };
        let vpc = run_pipeline(input, &read, &default_stages(), &ValidatorStage::new(read))
            .map_err(SessionError::Context)?;
        let lowering = lower(&vpc, true).map_err(SessionError::Context)?;
        let memory_roots: Vec<Digest> = [lifetime_root, project_root]
            .into_iter()
            .flatten()
            .collect();
        let projection_state = ProjectionState {
            projection_digest: vpc.projection_digest,
            cache_plan: lowering.cache_plan,
            memory_roots: memory_roots.clone(),
            lowered: lowering.messages.clone(),
        };
        let rendered = lowering
            .messages
            .iter()
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n\n");
        let rendered_hash = Digest::new(rendered.as_bytes());
        self.projection_state = Some(projection_state);
        let budget = if is_child {
            self.scheduler
                .child(run_id)
                .map(|c| c.budgets)
                .unwrap_or_else(|| self.scheduler.budgets())
        } else {
            self.scheduler.budgets()
        };
        Ok(StepContext {
            rendered,
            rendered_hash,
            selected_events: vpc.selected_events,
            budget,
            projection_digest: Some(vpc.projection_digest),
            memory_roots,
        })
    }

    // ---------- M4 memory tools + child runs (R-11/R-12, R-09) ----------

    /// Installs a claim/edge object into a scope's objects dir through an
    /// independent store handle (the actor's `store()` is read-only; the
    /// caller's duty is to install before proposing — the actor verifies
    /// refs, R-12/M-01). The dirsync is barriered before returning so the
    /// object is durable before any referencing frame.
    fn memory_install(&self, scope: &MemoryScope, bytes: &[u8]) -> Result<Digest, SessionError> {
        let memory_root = self.memory_root.clone();
        let objects_dir = memory_root.join(scope.dir_name()).join("objects");
        let queue = Arc::new(kanbei_core::queue::DurabilityQueue::start("kb-mem-install"));
        let mut store = kanbei_objects::ObjectStore::open(&objects_dir, Arc::clone(&queue))
            .map_err(|e| {
                SessionError::Memory(MemoryError::InvalidInput(format!("install store: {e}")))
            })?;
        let digest = store.install(bytes).map_err(|e| {
            SessionError::Memory(MemoryError::InvalidInput(format!("install: {e}")))
        })?;
        store.flush().map_err(|e| {
            SessionError::Memory(MemoryError::InvalidInput(format!("install flush: {e}")))
        })?;
        drop(store);
        if let Ok(q) = Arc::try_unwrap(queue) {
            let _ = q.shutdown();
        }
        Ok(digest)
    }

    /// A rejected M4 dispatch resolves as a Normal outcome carrying the
    /// error text (mirrors the native-tool error path — caller-authored
    /// input never surfaces as a SessionError).
    fn memory_outcome_error(&self, intent: &ToolIntent, error: String) -> ToolOutcome {
        ToolOutcome {
            call_id: intent.call_id.clone(),
            tool: intent.tool.clone(),
            result: Value::Null,
            error: Some(error),
            classification: OutcomeClassification::Normal,
            origin_snapshot: intent.origin_snapshot,
            commit_snapshot: self.current_snapshot,
            retained: None,
            hook_denied: None,
        }
    }

    /// The bound project's scope.
    fn project_scope(&self) -> MemoryScope {
        match self.memory_project.as_ref().expect("project bound").scope() {
            MemoryScope::Project(id) => MemoryScope::Project(*id),
            MemoryScope::Lifetime => unreachable!("project actor owns a project scope"),
        }
    }

    /// memory.query: search the committed claim DAG scoped by the run's read
    /// capability (children see project claims only; parents add the
    /// lifetime scope). The projection index is reconciled from the live
    /// folds first, so queries always see committed memory.
    fn dispatch_memory_query(
        &mut self,
        run_id: RunId,
        intent: &ToolIntent,
        _principal: Principal,
    ) -> Result<ToolOutcome, SessionError> {
        let _ = _principal;
        // Usage accounting is NotActiveRun-tolerant at dispatch: an
        // approval resolved after its run closed still commits the fact.
        if let Err(e) = self.scheduler.record_usage(
            run_id,
            RunUsage {
                tokens: 0,
                tools: 1,
                children: 0,
                started_at_secs: 0,
            },
        ) {
            match e {
                kanbei_scheduler::SchedulerError::NotActiveRun(_) => {}
                other => return Err(other.into()),
            }
        }
        let Some(query) = intent.args.get("query").and_then(|q| q.as_str()) else {
            return Ok(
                self.memory_outcome_error(intent, "memory.query requires a string query".into())
            );
        };
        let requested: Option<Vec<String>> = intent
            .args
            .get("scopes")
            .and_then(|s| s.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            });
        let project_bound = self.memory_project.is_some();
        let is_child = self.scheduler.run_kind(run_id) == Some(RunKind::Child);
        let mut allowed: Vec<&str> = Vec::new();
        if project_bound {
            allowed.push("project");
        }
        if !is_child {
            allowed.push("lifetime");
        }
        let mut scopes: Vec<MemoryScope> = Vec::new();
        match requested {
            Some(list) => {
                for name in list {
                    match name.as_str() {
                        "project" if allowed.contains(&"project") => {
                            scopes.push(self.project_scope());
                        }
                        "lifetime" if allowed.contains(&"lifetime") => {
                            scopes.push(MemoryScope::Lifetime);
                        }
                        _ => {}
                    }
                }
            }
            None => {
                if allowed.contains(&"project") {
                    scopes.push(self.project_scope());
                }
                if allowed.contains(&"lifetime") {
                    scopes.push(MemoryScope::Lifetime);
                }
            }
        }
        let max_results = intent
            .args
            .get("max_results")
            .and_then(|m| m.as_u64())
            .unwrap_or(16);
        // Reconcile the disposable index from the folds — the pinned roots
        // when the branch follows PinnedAt (the checkpoint-era claim set),
        // else the live heads.
        let mut index_inputs: Vec<ScopeIndexInput> = Vec::new();
        let lifetime_root = match &self.pinned_roots {
            Some(p) => Some(p.lifetime),
            None => self.memory_lifetime.head(),
        };
        if let Some(root) = lifetime_root {
            let fold = self
                .memory_lifetime
                .fold(Some(root))
                .map_err(SessionError::Memory)?;
            index_inputs.push(ScopeIndexInput {
                scope: MemoryScope::Lifetime,
                root: Some(root),
                fold,
            });
        }
        let project_root = match &self.pinned_roots {
            Some(p) => p.project,
            None => self.memory_project.as_ref().and_then(|a| a.head()),
        };
        if let Some(actor) = self.memory_project.as_ref()
            && let Some(root) = project_root
        {
            let fold = actor.fold(Some(root)).map_err(SessionError::Memory)?;
            index_inputs.push(ScopeIndexInput {
                scope: self.project_scope(),
                root: Some(root),
                fold,
            });
        }
        self.memory_index
            .reconcile(&index_inputs)
            .map_err(SessionError::Retrieval)?;
        let result = match self.memory_index.search(&SearchQuery {
            text: query.to_string(),
            scopes,
            max_results,
            ..Default::default()
        }) {
            Ok(r) => r,
            Err(e) => {
                return Ok(self.memory_outcome_error(intent, format!("memory.query failed: {e}")));
            }
        };
        let claims: Vec<Value> = result
            .claims
            .iter()
            .map(|c| {
                json!({
                    "digest": c.digest.to_string(),
                    "text": c.text,
                    "kind": c.kind,
                    "sensitivity": c.sensitivity,
                    "status": serde_json::to_value(&c.status).unwrap_or(Value::Null),
                    "score": c.score,
                    "contradictions": c.contradictions.iter().map(|x| json!({
                        "digest": x.digest.to_string(),
                        "text": x.text,
                        "supersedes": x.supersedes,
                    })).collect::<Vec<_>>(),
                    "source_events": c.source_events,
                })
            })
            .collect();
        let payload = json!({
            "claims": claims,
            "query_entities": result.query_entities.iter().map(|(k, kind)| json!({
                "key": k,
                "kind": format!("{kind:?}"),
            })).collect::<Vec<_>>(),
            "fts_used": result.fts_used,
            "expanded": result.expanded,
        });
        // The returned claims are this run's activated memories: they pin for
        // the next model-call boundary's salience scoring (R-12/F-S5).
        self.active_pins = claims
            .iter()
            .filter_map(|c| c.get("digest").and_then(|d| d.as_str()))
            .filter_map(|d| d.parse::<Digest>().ok())
            .collect();
        Ok(ToolOutcome {
            call_id: intent.call_id.clone(),
            tool: intent.tool.clone(),
            result: payload,
            error: None,
            classification: OutcomeClassification::Normal,
            origin_snapshot: intent.origin_snapshot,
            commit_snapshot: self.current_snapshot,
            retained: None,
            hook_denied: None,
        })
    }

    /// One approval-anchored root transition on `scope`: commits the
    /// `origin_kind` origin event (a typed root-approval/promotion fact), then
    /// proposes with a ≤3-attempt CAS rebase (stale expected roots rebase onto
    /// the actor's actual head; idempotency is keyed on the approval event).
    /// On exhaustion the deferred facts are committed and `("deferred", None)`
    /// returned. The session-side manifest mirrors the actor's internal
    /// construction byte-for-byte (same schema/parent/scope/order fields; the
    /// actor derives `retracted` from Supersedes edges itself).
    #[allow(clippy::too_many_arguments)]
    fn propose_transition(
        &mut self,
        scope: MemoryScope,
        kind: TransitionKind,
        origin_kind: &str,
        origin_payload: Value,
        principal: &Principal,
        decision_digest: Digest,
        added_claims: &[Digest],
        added_edges: &[Digest],
        retracted: &[Digest],
        deferred_claim: Digest,
        expected_root: Option<Digest>,
    ) -> Result<(String, Option<Id128>), SessionError> {
        let receipt = self.commit(
            vec![NewEvent {
                kind: origin_kind.into(),
                payload_schema: 1,
                payload: origin_payload,
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            None,
        )?;
        let approval_event = receipt.last_seq;
        let mut expected = expected_root;
        for attempt in 0..3u32 {
            let outcome = {
                let actor = match &scope {
                    MemoryScope::Lifetime => &mut self.memory_lifetime,
                    MemoryScope::Project(_) => {
                        self.memory_project.as_mut().expect("project bound")
                    }
                };
                let manifest = RootManifest {
                    schema: MEMORY_ROOT_SCHEMA,
                    parent: expected,
                    scope: scope.clone(),
                    added_claims: added_claims.to_vec(),
                    added_edges: added_edges.to_vec(),
                    retracted: retracted.to_vec(),
                    transition_id: Id128::generate(),
                };
                let manifest_digest = manifest.digest();
                let transition = MemoryTransition {
                    schema: MEMORY_TRANSITION_SCHEMA,
                    transition_id: manifest.transition_id,
                    scope: scope.clone(),
                    kind: kind.clone(),
                    expected_old_root: expected,
                    accepted_new_root: manifest_digest,
                    origin_session: self.session_id,
                    origin_event: approval_event,
                    origin_kind: origin_kind.into(),
                    decision_principal: principal.clone(),
                    decision_digest,
                    idempotency_key: IdempotencyKey {
                        session: self.session_id,
                        event: approval_event,
                        decision: decision_digest,
                    },
                };
                actor.propose(transition, added_claims, added_edges)
            };
            match outcome.map_err(SessionError::Memory)? {
                TransitionOutcome::Committed { transition_id, .. } => {
                    // R-08: the actor just advanced a memory root, so the
                    // backlink is a state-changing commit — pin the post-state
                    // manifest (its memory-root pin captures the new head) so
                    // `current_snapshot` advances and a resume re-derives it.
                    self.commit(
                        vec![NewEvent {
                            kind: "memory_transition_backlink".into(),
                            payload_schema: 1,
                            payload: json!({
                                "transition_id": transition_id.to_string(),
                                "scope": serde_json::to_value(scope.clone())
                                    .expect("scope serialization cannot fail"),
                            }),
                            objects: Vec::new(),
                            refs: Vec::new(),
                        }],
                        Some(self.composition.current().digest),
                    )?;
                    return Ok(("approved".into(), Some(transition_id)));
                }
                TransitionOutcome::CasFailed { actual, .. } if attempt < 2 => {
                    expected = actual;
                }
                TransitionOutcome::CasFailed { installed, .. } => {
                    self.commit(
                        vec![
                            NewEvent {
                                kind: "promotion_deferred".into(),
                                payload_schema: 1,
                                payload: json!({
                                    "claim_digest": deferred_claim.to_string(),
                                    "reason": "CAS rebase exhausted (3 attempts)",
                                }),
                                objects: Vec::new(),
                                refs: Vec::new(),
                            },
                            NewEvent {
                                kind: "memory_orphans_expected".into(),
                                payload_schema: 1,
                                payload: json!({
                                    "scope": serde_json::to_value(scope.clone())
                                        .expect("scope serialization cannot fail"),
                                    "digests": installed
                                        .iter()
                                        .map(|d| d.to_string())
                                        .collect::<Vec<_>>(),
                                }),
                                objects: Vec::new(),
                                refs: Vec::new(),
                            },
                        ],
                        None,
                    )?;
                    return Ok(("deferred".into(), None));
                }
            }
        }
        unreachable!("the rebase loop returns on every branch")
    }

    /// memory.propose: install the claim object, commit the canonical
    /// `memory_proposal` fact, then — under a broker approval — commit the
    /// root-selection transition(s) and their backlinks. A supersede target
    /// is retracted by a SECOND transition carrying the supersedes edge:
    /// R-12/M-13 edges point only to already-committed claims, and the actor
    /// derives retraction from the edge's `from` — so the edge departs from
    /// the superseded claim toward the (by then committed) successor. Without
    /// an approval the claim is left `proposed` for the root agent.
    fn dispatch_memory_propose(
        &mut self,
        run_id: RunId,
        intent: &ToolIntent,
        principal: Principal,
    ) -> Result<ToolOutcome, SessionError> {
        // Usage accounting is NotActiveRun-tolerant at dispatch: an
        // approval resolved after its run closed still commits the fact.
        if let Err(e) = self.scheduler.record_usage(
            run_id,
            RunUsage {
                tokens: 0,
                tools: 1,
                children: 0,
                started_at_secs: 0,
            },
        ) {
            match e {
                kanbei_scheduler::SchedulerError::NotActiveRun(_) => {}
                other => return Err(other.into()),
            }
        }
        if self.memory_project.is_none() {
            return Ok(self.memory_outcome_error(intent, "no project bound".into()));
        }
        let Some(claim_val) = intent.args.get("claim") else {
            return Ok(
                self.memory_outcome_error(intent, "memory.propose requires a claim object".into())
            );
        };
        let Some(kind) = claim_val.get("kind").and_then(|k| k.as_str()) else {
            return Ok(self.memory_outcome_error(intent, "claim.kind is required".into()));
        };
        let Some(content) = claim_val.get("content").and_then(|c| c.as_str()) else {
            return Ok(self.memory_outcome_error(intent, "claim.content is required".into()));
        };
        let sensitivity = claim_val
            .get("sensitivity")
            .and_then(|v| v.as_str())
            .unwrap_or("internal")
            .to_string();
        let supersedes: Option<Id128> = claim_val
            .get("supersedes")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok());

        let project_id = match self.memory_project.as_ref().expect("project bound").scope() {
            MemoryScope::Project(id) => *id,
            MemoryScope::Lifetime => unreachable!("project actor owns a project scope"),
        };
        let claim_id = Id128::generate();
        let provenance = ClaimProvenance {
            session: self.session_id,
            event: intent.intent_event.unwrap_or(0),
            source_claims: Vec::new(),
            evidence_excerpt: String::new(),
        };
        let claim = Claim {
            schema: MEMORY_CLAIM_SCHEMA,
            claim_id,
            kind: kind.to_string(),
            content: content.to_string(),
            owner: principal.clone(),
            visibility_scope: MemoryScope::Project(project_id),
            provenance: provenance.clone(),
            observed_at: None,
            valid_from: None,
            sensitivity: sensitivity.clone(),
        };
        let claim_digest = self.memory_install(
            &MemoryScope::Project(project_id),
            &claim.to_canonical_bytes(),
        )?;
        self.fault(crate::FaultPoint::BeforeMemoryProposal);
        self.commit(
            vec![NewEvent {
                kind: "memory_proposal".into(),
                payload_schema: 1,
                payload: json!({
                    "claim_digest": claim_digest.to_string(),
                    "claim_id": claim_id.to_string(),
                    "kind": kind,
                    "content": content,
                    "sensitivity": sensitivity,
                    "owner": principal,
                    "intent_event": intent.intent_event,
                }),
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            None,
        )?;
        self.fault(crate::FaultPoint::AfterMemoryProposal);

        // The supersede target must be an active claim of the project fold.
        let edge = match supersedes {
            Some(target) => {
                let project = self.memory_project.as_ref().expect("project bound");
                let fold = project.fold(project.head()).map_err(SessionError::Memory)?;
                let Some(target_digest) = fold
                    .claims
                    .iter()
                    .find(|(_, c)| c.claim_id == target)
                    .map(|(d, _)| *d)
                else {
                    return Ok(
                        self.memory_outcome_error(intent, "supersedes target not found".into())
                    );
                };
                let edge = ClaimEdge {
                    schema: MEMORY_EDGE_SCHEMA,
                    from: target,
                    to: Some(claim_id),
                    kind: EdgeKind::Supersedes,
                    entity_keys: Vec::new(),
                    provenance: provenance.clone(),
                };
                let digest = self.memory_install(
                    &MemoryScope::Project(project_id),
                    &edge.to_canonical_bytes(),
                )?;
                Some((edge, digest, target_digest))
            }
            None => None,
        };

        let Some(approval_digest) = intent.approval else {
            // Left proposed for the root agent: no transition, no edge.
            return Ok(ToolOutcome {
                call_id: intent.call_id.clone(),
                tool: intent.tool.clone(),
                result: json!({
                    "claim_id": claim_id.to_string(),
                    "claim_digest": claim_digest.to_string(),
                    "status": "proposed",
                }),
                error: None,
                classification: OutcomeClassification::Normal,
                origin_snapshot: intent.origin_snapshot,
                commit_snapshot: self.current_snapshot,
                retained: None,
                hook_denied: None,
            });
        };

        // Phase 1 — the claim's own transition (genesis when the project
        // fold is empty).
        let expected = {
            let project = self.memory_project.as_ref().expect("project bound");
            project.head()
        };
        let (status, transition_id) = self.propose_transition(
            MemoryScope::Project(project_id),
            TransitionKind::RootApproval,
            "memory_root_approved",
            json!({
                "claim_digest": claim_digest.to_string(),
                "decision_digest": approval_digest.to_string(),
                "expected_root": expected.map(|d| d.to_string()),
            }),
            &principal,
            approval_digest,
            &[claim_digest],
            &[],
            &[],
            claim_digest,
            expected,
        )?;

        // Phase 2 — the supersede edge (only after the claim committed).
        let mut status = status;
        let mut transition_id = transition_id;
        if status == "approved"
            && let Some((_edge, edge_digest, target_digest)) = edge
        {
            let expected = {
                let project = self.memory_project.as_ref().expect("project bound");
                project.head()
            };
            let (s, t) = self.propose_transition(
                MemoryScope::Project(project_id),
                TransitionKind::RootApproval,
                "memory_root_approved",
                json!({
                    "claim_digest": claim_digest.to_string(),
                    "edge_digest": edge_digest.to_string(),
                    "decision_digest": approval_digest.to_string(),
                    "expected_root": expected.map(|d| d.to_string()),
                }),
                &principal,
                approval_digest,
                &[],
                &[edge_digest],
                &[target_digest],
                claim_digest,
                expected,
            )?;
            status = s;
            if t.is_some() {
                transition_id = t;
            }
        }

        Ok(ToolOutcome {
            call_id: intent.call_id.clone(),
            tool: intent.tool.clone(),
            result: json!({
                "claim_id": claim_id.to_string(),
                "claim_digest": claim_digest.to_string(),
                "status": status,
                "transition_id": transition_id.map(|t| t.to_string()),
            }),
            error: None,
            classification: OutcomeClassification::Normal,
            origin_snapshot: intent.origin_snapshot,
            commit_snapshot: self.current_snapshot,
            retained: None,
            hook_denied: None,
        })
    }

    /// Resolves a proposed claim's digest from the committed `memory_proposal`
    /// facts: a proposal is committed (and its object installed) but, until
    /// approved, is absent from the scope fold. The most recent matching
    /// proposal wins.
    fn proposed_claim_digest(&self, claim_id: Id128) -> Result<Option<Digest>, SessionError> {
        let log_path = self.log_path.clone();
        let mut found: Option<Digest> = None;
        kanbei_log::for_each_frame(&log_path, |info| {
            for line in &info.events {
                let Ok(env) = Envelope::from_line(line) else {
                    continue;
                };
                if env.kind != "memory_proposal" {
                    continue;
                }
                let payload = self.resolved_payload(&env);
                let matches = payload
                    .get("claim_id")
                    .and_then(|c| c.as_str())
                    .and_then(|s| s.parse::<Id128>().ok())
                    == Some(claim_id);
                if matches
                    && let Some(d) = payload
                        .get("claim_digest")
                        .and_then(|d| d.as_str())
                        .and_then(|s| s.parse::<Digest>().ok())
                {
                    found = Some(d);
                }
            }
        })?;
        Ok(found)
    }

    /// memory.review: the root agent's decision on a proposed project claim
    /// (R-11). `approve` commits the root transition under the broker approval
    /// (the same write path as an approval-gated propose); `reject` and
    /// `request_evidence` commit canonical decision facts only — they never
    /// touch the claim DAG, but they are the recorded review outcome the
    /// design requires ("accepts, rejects, requests evidence, or leaves them
    /// unresolved").
    fn dispatch_memory_review(
        &mut self,
        run_id: RunId,
        intent: &ToolIntent,
        principal: Principal,
    ) -> Result<ToolOutcome, SessionError> {
        if let Err(e) = self.scheduler.record_usage(
            run_id,
            RunUsage {
                tokens: 0,
                tools: 1,
                children: 0,
                started_at_secs: 0,
            },
        ) {
            match e {
                kanbei_scheduler::SchedulerError::NotActiveRun(_) => {}
                other => return Err(other.into()),
            }
        }
        if self.memory_project.is_none() {
            return Ok(self.memory_outcome_error(intent, "no project bound".into()));
        }
        let Some(claim_id) = intent
            .args
            .get("claim_id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<Id128>().ok())
        else {
            return Ok(self.memory_outcome_error(intent, "memory.review requires a claim_id".into()));
        };
        let Some(decision) = intent.args.get("decision").and_then(|v| v.as_str()) else {
            return Ok(
                self.memory_outcome_error(intent, "memory.review requires a decision".into())
            );
        };
        // An active fold claim is already approved; a proposal resolves
        // through its committed `memory_proposal` fact.
        let active = {
            let project = self.memory_project.as_ref().expect("project bound");
            let fold = project.fold(project.head()).map_err(SessionError::Memory)?;
            fold.claims
                .iter()
                .find(|(_, c)| c.claim_id == claim_id)
                .map(|(d, _)| *d)
        };
        match decision {
            "approve" => {
                if active.is_some() {
                    return Ok(
                        self.memory_outcome_error(intent, "claim is already active".into())
                    );
                }
                let Some(claim_digest) = self.proposed_claim_digest(claim_id)? else {
                    return Ok(
                        self.memory_outcome_error(intent, "no matching memory proposal".into())
                    );
                };
                let Some(approval_digest) = intent.approval else {
                    return Ok(
                        self.memory_outcome_error(intent, "approve requires user approval".into())
                    );
                };
                let scope = self.project_scope();
                let expected = self.memory_project.as_ref().expect("project bound").head();
                let (status, transition_id) = self.propose_transition(
                    scope,
                    TransitionKind::RootApproval,
                    "memory_root_approved",
                    json!({
                        "claim_digest": claim_digest.to_string(),
                        "decision_digest": approval_digest.to_string(),
                        "expected_root": expected.map(|d| d.to_string()),
                    }),
                    &principal,
                    approval_digest,
                    &[claim_digest],
                    &[],
                    &[],
                    claim_digest,
                    expected,
                )?;
                Ok(ToolOutcome {
                    call_id: intent.call_id.clone(),
                    tool: intent.tool.clone(),
                    result: json!({
                        "claim_id": claim_id.to_string(),
                        "claim_digest": claim_digest.to_string(),
                        "decision": decision,
                        "status": status,
                        "transition_id": transition_id.map(|t| t.to_string()),
                    }),
                    error: None,
                    classification: OutcomeClassification::Normal,
                    origin_snapshot: intent.origin_snapshot,
                    commit_snapshot: self.current_snapshot,
                    retained: None,
                    hook_denied: None,
                })
            }
            "reject" | "request_evidence" => {
                let Some(claim_digest) = active.or(self.proposed_claim_digest(claim_id)?) else {
                    return Ok(
                        self.memory_outcome_error(intent, "no matching memory proposal".into())
                    );
                };
                let reason = intent
                    .args
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let kind = if decision == "reject" {
                    "memory_proposal_rejected"
                } else {
                    "memory_evidence_requested"
                };
                self.commit(
                    vec![NewEvent {
                        kind: kind.into(),
                        payload_schema: 1,
                        payload: json!({
                            "claim_id": claim_id.to_string(),
                            "claim_digest": claim_digest.to_string(),
                            "reason": reason,
                            "decision_principal": principal,
                        }),
                        objects: Vec::new(),
                        refs: Vec::new(),
                    }],
                    None,
                )?;
                Ok(ToolOutcome {
                    call_id: intent.call_id.clone(),
                    tool: intent.tool.clone(),
                    result: json!({
                        "claim_id": claim_id.to_string(),
                        "claim_digest": claim_digest.to_string(),
                        "decision": decision,
                        "status": decision,
                    }),
                    error: None,
                    classification: OutcomeClassification::Normal,
                    origin_snapshot: intent.origin_snapshot,
                    commit_snapshot: self.current_snapshot,
                    retained: None,
                    hook_denied: None,
                })
            }
            other => Ok(self.memory_outcome_error(
                intent,
                format!("unknown memory.review decision {other:?}"),
            )),
        }
    }

    /// memory.promote: promote an ACTIVE project claim into the lifetime
    /// scope (R-11/E-F4). Promotion is user-gated — the origin fact carries
    /// the broker approval digest. The destination claim is a NEW
    /// lifetime-scoped claim whose provenance carries the source claim digest
    /// and a bounded evidence excerpt; a `promoted_from` edge links
    /// destination → source (the source lives in the project DAG, so the edge
    /// target is cross-scope). The lifetime scope transition commits first;
    /// the session then records the accepted TransitionId as a backlink.
    fn dispatch_memory_promote(
        &mut self,
        run_id: RunId,
        intent: &ToolIntent,
        principal: Principal,
    ) -> Result<ToolOutcome, SessionError> {
        if let Err(e) = self.scheduler.record_usage(
            run_id,
            RunUsage {
                tokens: 0,
                tools: 1,
                children: 0,
                started_at_secs: 0,
            },
        ) {
            match e {
                kanbei_scheduler::SchedulerError::NotActiveRun(_) => {}
                other => return Err(other.into()),
            }
        }
        if self.memory_project.is_none() {
            return Ok(self.memory_outcome_error(intent, "no project bound".into()));
        }
        let Some(source_claim_id) = intent
            .args
            .get("claim_id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<Id128>().ok())
        else {
            return Ok(
                self.memory_outcome_error(intent, "memory.promote requires a claim_id".into())
            );
        };
        let evidence = intent
            .args
            .get("evidence")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let source = {
            let project = self.memory_project.as_ref().expect("project bound");
            let fold = project.fold(project.head()).map_err(SessionError::Memory)?;
            fold.claims
                .iter()
                .find(|(_, c)| c.claim_id == source_claim_id)
                .cloned()
        };
        let Some((source_digest, source_claim)) = source else {
            return Ok(self.memory_outcome_error(
                intent,
                "promote source is not an active project claim".into(),
            ));
        };
        let Some(approval_digest) = intent.approval else {
            return Ok(
                self.memory_outcome_error(intent, "promotion requires user approval".into())
            );
        };
        let provenance = ClaimProvenance::new_promotion(
            self.session_id,
            intent.intent_event.unwrap_or(0),
            vec![source_digest],
            evidence,
        )
        .map_err(SessionError::Memory)?;
        let dest = Claim {
            schema: MEMORY_CLAIM_SCHEMA,
            claim_id: Id128::generate(),
            kind: source_claim.kind.clone(),
            content: source_claim.content.clone(),
            owner: principal.clone(),
            visibility_scope: MemoryScope::Lifetime,
            provenance: provenance.clone(),
            observed_at: source_claim.observed_at,
            valid_from: source_claim.valid_from,
            sensitivity: source_claim.sensitivity.clone(),
        };
        let dest_digest = self.memory_install(&MemoryScope::Lifetime, &dest.to_canonical_bytes())?;
        let edge = ClaimEdge::new(
            dest.claim_id,
            Some(source_claim_id),
            EdgeKind::PromotedFrom,
            Vec::new(),
            provenance,
        )
        .map_err(SessionError::Memory)?;
        let edge_digest = self.memory_install(&MemoryScope::Lifetime, &edge.to_canonical_bytes())?;

        let expected = self.memory_lifetime.head();
        let (status, transition_id) = self.propose_transition(
            MemoryScope::Lifetime,
            TransitionKind::Promotion,
            "memory_promotion_approved",
            json!({
                "claim_digest": dest_digest.to_string(),
                "edge_digest": edge_digest.to_string(),
                "source_claim_id": source_claim_id.to_string(),
                "source_digest": source_digest.to_string(),
                "decision_digest": approval_digest.to_string(),
                "expected_root": expected.map(|d| d.to_string()),
            }),
            &principal,
            approval_digest,
            &[dest_digest],
            &[edge_digest],
            &[],
            dest_digest,
            expected,
        )?;
        Ok(ToolOutcome {
            call_id: intent.call_id.clone(),
            tool: intent.tool.clone(),
            result: json!({
                "claim_id": dest.claim_id.to_string(),
                "claim_digest": dest_digest.to_string(),
                "source_claim_id": source_claim_id.to_string(),
                "source_digest": source_digest.to_string(),
                "status": status,
                "transition_id": transition_id.map(|t| t.to_string()),
            }),
            error: None,
            classification: OutcomeClassification::Normal,
            origin_snapshot: intent.origin_snapshot,
            commit_snapshot: self.current_snapshot,
            retained: None,
            hook_denied: None,
        })
    }
    /// child.spawn: spawn a bounded child run under the active parent,
    /// drive it through the cognition loop with a fresh provider from the
    /// configured factory (the child's render closure attenuates via
    /// [`Session::project_context`]'s run-kind check), and record the child
    /// run lifecycle (run_start + run_outcome) canonically. Every started
    /// run reaches a terminal outcome.
    fn dispatch_child_spawn(
        &mut self,
        run_id: RunId,
        intent: &ToolIntent,
        _principal: Principal,
    ) -> Result<ToolOutcome, SessionError> {
        if self.scheduler.run_kind(run_id) == Some(RunKind::Child) {
            return Ok(self.memory_outcome_error(intent, "children cannot spawn children".into()));
        }
        let Some(prompt) = intent.args.get("prompt").and_then(|p| p.as_str()) else {
            return Ok(self.memory_outcome_error(intent, "child.spawn requires a prompt".into()));
        };
        if prompt.len() > 8192 {
            return Ok(
                self.memory_outcome_error(intent, "child.spawn prompt exceeds 8192 chars".into())
            );
        }
        // Clamped child budgets (the kernel clamp is the session's job; the
        // scheduler records them as given).
        let spec = intent.args.get("budgets");
        let deadline = spec
            .and_then(|b| b.get("deadline_secs"))
            .and_then(|v| v.as_u64())
            .unwrap_or(60)
            .min(60);
        let tokens = spec
            .and_then(|b| b.get("tokens"))
            .and_then(|v| v.as_u64())
            .unwrap_or(20_000)
            .min(20_000);
        let tools = spec
            .and_then(|b| b.get("tools"))
            .and_then(|v| v.as_u64())
            .unwrap_or(16)
            .min(16);
        let child_budgets = Budgets {
            deadline_secs: Some(deadline),
            tokens: Some(tokens),
            tools: Some(tools),
            children: Some(0),
        };
        // The parent's children budget bounds concurrent children.
        let parent_children = self.scheduler.current_usage(run_id).children;
        let parent_cap = self.scheduler.budgets().children.unwrap_or(u64::MAX);
        if parent_children >= parent_cap {
            return Ok(self.memory_outcome_error(intent, "child budget exhausted".into()));
        }
        let child_id = self
            .scheduler
            .spawn_child(run_id, child_budgets)
            .map_err(SessionError::Scheduler)?;
        self.run_start(child_id)?;
        let trigger = Trigger {
            kind: kanbei_scheduler::TriggerKind::ChildDone,
            referent: Some(Digest::new(child_id.to_string().as_bytes())),
        };
        let outcome = {
            let factory = self
                .child_provider
                .as_mut()
                .ok_or_else(|| SessionError::InvalidInput("no child provider configured".into()))?;
            let mut provider = factory();
            self.cognition_loop(
                child_id,
                trigger.clone(),
                provider.as_mut(),
                |sess: &mut Session| sess.project_context(child_id, &trigger),
            )
        };
        let outcome = match outcome {
            Ok(o) => o,
            Err(e) => {
                // Every started run reaches a terminal outcome (canonical):
                // the loop errored without recording — record Failed here
                // and close the scheduler entry.
                let usage = self.scheduler.current_usage(child_id);
                let record = RunOutcome {
                    run_id: child_id,
                    outcome: TerminalOutcome::Failed(FailureKind::Internal),
                    reason: Some(format!("child run failed: {e}")),
                };
                self.commit(
                    vec![NewEvent {
                        kind: "run_outcome".into(),
                        payload_schema: 1,
                        payload: serde_json::to_value(&record).map_err(|e| {
                            SessionError::InvalidInput(format!("run outcome payload: {e}"))
                        })?,
                        objects: Vec::new(),
                        refs: Vec::new(),
                    }],
                    None,
                )?;
                let _ = self.scheduler.record_outcome(
                    child_id,
                    TerminalOutcome::Failed(FailureKind::Internal),
                    usage,
                    &[],
                )?;
                return Ok(self.memory_outcome_error(intent, format!("child run failed: {e}")));
            }
        };
        // The child's canonical outcome was recorded inside cognition_loop;
        // only close the scheduler entry when the loop left it live (the
        // Blocked path commits its record directly and returns without
        // touching the scheduler).
        let usage = self.scheduler.current_usage(child_id);
        if self.scheduler.child(child_id).is_some() {
            let (record, _) = self
                .scheduler
                .record_outcome(child_id, outcome, usage, &[])?;
            let _ = record;
        }
        self.scheduler.observe(trigger);
        self.scheduler.record_usage(
            run_id,
            RunUsage {
                tokens: 0,
                tools: 0,
                children: 1,
                started_at_secs: 0,
            },
        )?;
        Ok(ToolOutcome {
            call_id: intent.call_id.clone(),
            tool: intent.tool.clone(),
            result: json!({
                "run_id": child_id.to_string(),
                "outcome": format!("{outcome:?}"),
                "usage": json!({ "tokens": usage.tokens, "tools": usage.tools }),
            }),
            error: None,
            classification: OutcomeClassification::Normal,
            origin_snapshot: intent.origin_snapshot,
            commit_snapshot: self.current_snapshot,
            retained: None,
            hook_denied: None,
        })
    }

    // ---------- bounded cognition step loop (R-18/E-01) ----------

    /// The built-in cognition step loop: one bounded orchestration body over
    /// the closed host-command set. The kernel checks the wake deadline and
    /// budgets at every host-command boundary; `Finish` commits the run
    /// outcome; `Blocked` records explicit budget exhaustion.
    pub fn cognition_loop(
        &mut self,
        run_id: RunId,
        trigger: Trigger,
        provider: &mut dyn kanbei_scheduler::CognitionProvider,
        render: impl Fn(&mut Session) -> Result<StepContext, SessionError>,
    ) -> Result<TerminalOutcome, SessionError> {
        // T9: `on_turn_start` runs before the first model call/render of a
        // run. A deny is terminal (canonical `turn_denied`, Blocked) with NO
        // model call; annotations are committed before the projection renders
        // so this turn sees them.
        let hook_context = json!({
            "hook": HookKind::OnTurnStart.as_str(),
            "run": run_id.to_string(),
        });
        let hooks = self.evaluate_hooks(HookKind::OnTurnStart, &hook_context.to_string());
        if let Decision::Deny { .. } = hooks.decision {
            self.commit(
                vec![NewEvent {
                    kind: "turn_denied".into(),
                    payload_schema: 1,
                    payload: json!({
                        "run": run_id.to_string(),
                        "hook": HookKind::OnTurnStart.as_str(),
                        "module_id": hooks.denier.as_ref().map(|d| d.module_id.to_string()),
                        "package_digest": hooks
                            .denier
                            .as_ref()
                            .map(|d| d.package_digest.to_string()),
                        "decision_digest": hooks.decision.digest().to_string(),
                    }),
                    objects: Vec::new(),
                    refs: Vec::new(),
                }],
                None,
            )?;
            let usage = self.scheduler.current_usage(run_id);
            self.run_outcome_with_reason(
                run_id,
                TerminalOutcome::Blocked,
                usage,
                &[],
                Some(crate::hooks::DENIED_REASON.to_string()),
            )?;
            return Ok(TerminalOutcome::Blocked);
        }
        for annotation in &hooks.annotations {
            self.commit(
                vec![NewEvent {
                    kind: "hook_annotation".into(),
                    payload_schema: 1,
                    payload: json!({
                        "run": run_id.to_string(),
                        "hook": HookKind::OnTurnStart.as_str(),
                        "key": annotation.key,
                        "value": annotation.value,
                    }),
                    objects: Vec::new(),
                    refs: Vec::new(),
                }],
                None,
            )?;
        }
        let mut context = render(self)?;
        context.budget = self.scheduler.budgets();
        let mut last: Option<StepResult> = None;
        let outcome = loop {
            // External cancel (UI seam, decision 13): a user-requested cancel
            // lands at the next model-call stream boundary or step boundary,
            // taking the canonical `Failed(UserCancelled)` path. The flag is
            // one-shot — clear it once consumed so it does not cancel the
            // following turn.
            let cancel_tripped = self
                .cancel_flag
                .as_ref()
                .is_some_and(|f| f.load(std::sync::atomic::Ordering::SeqCst));
            if cancel_tripped {
                let usage = self.scheduler.current_usage(run_id);
                self.run_outcome_with_reason(
                    run_id,
                    TerminalOutcome::Failed(FailureKind::UserCancelled),
                    usage,
                    &[],
                    Some("cancelled by user".into()),
                )?;
                if let Some(flag) = &self.cancel_flag {
                    flag.store(false, std::sync::atomic::Ordering::SeqCst);
                }
                return Ok(TerminalOutcome::Failed(FailureKind::UserCancelled));
            }
            // Wake deadline/budget at each host-command boundary.
            if let Err(e) = self.scheduler.check_boundary(run_id) {
                // Route through the run FSM like any terminal outcome: the
                // canonical record alone left `active` occupied and every
                // later wake denied ConcurrencyLimit (Wake=Run pairing,
                // architecture.md:118 broken otherwise).
                let usage = self.scheduler.current_usage(run_id);
                self.run_outcome_with_reason(
                    run_id,
                    TerminalOutcome::Blocked,
                    usage,
                    &[],
                    Some(e.to_string()),
                )?;
                return Ok(TerminalOutcome::Blocked);
            }
            // Host-command boundary: repaint before the next model/tool step
            // so the user row, thought/tool rows and the live working
            // indicator appear as they happen (not only after the turn).
            self.fire_present_hook();
            let command = provider
                .step(&context, &trigger, last.as_ref())
                .map_err(|e| SessionError::InvalidInput(format!("cognition step: {e}")))?;            match command {
                StepCommand::ModelCall(_) => {
                    // The staged projection's lowered messages are the
                    // request; the M3 fallback renders the raw context. The
                    // spec's rendered_hash is intentionally not re-validated
                    // against context.rendered_hash here (M3 behavior — the
                    // request is built from the same rendered context).
                    let messages = self
                        .projection_state
                        .as_ref()
                        .map(|p| p.lowered.clone())
                        .unwrap_or_else(|| {
                            vec![Message {
                                role: Role::User,
                                content: context.rendered.clone(),
                                tool_call_id: None,
                            }]
                        });
                    let selected = context.selected_events.clone();
                    let result = match self.model_call(run_id, messages, selected, &context.rendered) {
                        Ok(result) => result,
                        // A user cancel observed at a model-call stream
                        // boundary takes the same canonical path as the
                        // step-boundary check above (decision 13).
                        Err(SessionError::Cancelled) => {
                            let usage = self.scheduler.current_usage(run_id);
                            self.run_outcome_with_reason(
                                run_id,
                                TerminalOutcome::Failed(FailureKind::UserCancelled),
                                usage,
                                &[],
                                Some("cancelled by user".into()),
                            )?;
                            if let Some(flag) = &self.cancel_flag {
                                flag.store(false, std::sync::atomic::Ordering::SeqCst);
                            }
                            return Ok(TerminalOutcome::Failed(FailureKind::UserCancelled));
                        }
                        Err(e) => return Err(e),
                    };
                    last = Some(StepResult::Model(result));
                }
                StepCommand::ToolIntent { tool, arguments } => {
                    let principal = Principal {
                        session: self.session_id,
                        generation: 0,
                        run: Some(0),
                    };
                    let tool_outcome = self.tool_call(run_id, principal, &tool, arguments)?;
                    // A parked approval is pending resolution, not final:
                    // its outcome event arrives at resolution (or recovery
                    // classifies the intent) — committing the park report
                    // would terminate the intent story early.
                    if tool_outcome.awaiting_approval() {
                        // the driver seam resolves parked approvals (an
                        // unattended battery plays the user); the park
                        // report never commits as an outcome — resolution
                        // dispatches + commits, or recovery classifies
                        if let Some(resolved) = self.resolve_parked_via_driver()? {
                            last = Some(StepResult::Tool(
                                serde_json::to_value(&resolved).unwrap_or(Value::Null),
                            ));
                        } else {
                            last = Some(StepResult::Tool(
                                serde_json::to_value(&tool_outcome).unwrap_or(Value::Null),
                            ));
                        }
                    } else if tool_outcome.denied() {
                        // T9: the deny outcome was already committed by tool_call.
                        last = Some(StepResult::Tool(
                            serde_json::to_value(&tool_outcome).unwrap_or(Value::Null),
                        ));
                    } else {
                        self.commit_tool_outcome(&tool_outcome)?;
                        last = Some(StepResult::Tool(
                            serde_json::to_value(&tool_outcome).unwrap_or(Value::Null),
                        ));
                    }
                }
                StepCommand::MemoryQuery { query } => {
                    let principal = Principal {
                        session: self.session_id,
                        generation: 0,
                        run: Some(0),
                    };
                    let tool_outcome = self.tool_call(
                        run_id,
                        principal,
                        "memory.query",
                        json!({ "query": query }),
                    )?;
                    if tool_outcome.awaiting_approval() {
                        if let Some(resolved) = self.resolve_parked_via_driver()? {
                            last = Some(StepResult::Memory(
                                serde_json::to_value(&resolved).unwrap_or(Value::Null),
                            ));
                        } else {
                            last = Some(StepResult::Memory(
                                serde_json::to_value(&tool_outcome).unwrap_or(Value::Null),
                            ));
                        }
                    } else if tool_outcome.denied() {
                        // T9: the deny outcome was already committed by tool_call.
                        last = Some(StepResult::Memory(
                            serde_json::to_value(&tool_outcome).unwrap_or(Value::Null),
                        ));
                    } else {
                        self.commit_tool_outcome(&tool_outcome)?;
                        last = Some(StepResult::Memory(
                            serde_json::to_value(&tool_outcome).unwrap_or(Value::Null),
                        ));
                    }
                }
                StepCommand::MemoryPropose { claim } => {
                    let principal = Principal {
                        session: self.session_id,
                        generation: 0,
                        run: Some(0),
                    };
                    let tool_outcome = self.tool_call(
                        run_id,
                        principal,
                        "memory.propose",
                        json!({ "claim": claim }),
                    )?;
                    if tool_outcome.awaiting_approval() {
                        if let Some(resolved) = self.resolve_parked_via_driver()? {
                            last = Some(StepResult::Memory(
                                serde_json::to_value(&resolved).unwrap_or(Value::Null),
                            ));
                        } else {
                            last = Some(StepResult::Memory(
                                serde_json::to_value(&tool_outcome).unwrap_or(Value::Null),
                            ));
                        }
                    } else if tool_outcome.denied() {
                        // T9: the deny outcome was already committed by tool_call.
                        last = Some(StepResult::Memory(
                            serde_json::to_value(&tool_outcome).unwrap_or(Value::Null),
                        ));
                    } else {
                        self.commit_tool_outcome(&tool_outcome)?;
                        last = Some(StepResult::Memory(
                            serde_json::to_value(&tool_outcome).unwrap_or(Value::Null),
                        ));
                    }
                }
                StepCommand::ChildSpawn { spec } => {
                    let principal = Principal {
                        session: self.session_id,
                        generation: 0,
                        run: Some(0),
                    };
                    let tool_outcome = self.tool_call(run_id, principal, "child.spawn", spec)?;
                    if tool_outcome.awaiting_approval() {
                        if let Some(resolved) = self.resolve_parked_via_driver()? {
                            last = Some(StepResult::Child(
                                serde_json::to_value(&resolved).unwrap_or(Value::Null),
                            ));
                        } else {
                            last = Some(StepResult::Child(
                                serde_json::to_value(&tool_outcome).unwrap_or(Value::Null),
                            ));
                        }
                    } else if tool_outcome.denied() {
                        // T9: the deny outcome was already committed by tool_call.
                        last = Some(StepResult::Child(
                            serde_json::to_value(&tool_outcome).unwrap_or(Value::Null),
                        ));
                    } else {
                        self.commit_tool_outcome(&tool_outcome)?;
                        last = Some(StepResult::Child(
                            serde_json::to_value(&tool_outcome).unwrap_or(Value::Null),
                        ));
                    }
                }
                StepCommand::ScheduleWake {
                    kind,
                    after_secs: _,
                } => {
                    self.scheduler.observe(Trigger {
                        kind,
                        referent: None,
                    });
                    last = Some(StepResult::Scheduled);
                }
                StepCommand::Finish(finish) => break finish,
            }
        };
        let usage = self.scheduler.current_usage(run_id);
        self.run_outcome(run_id, outcome, usage, &[])?;
        Ok(outcome)
    }

    // ---------- interrupted/ambiguous recovery (B-05) ----------

    /// Scan the committed log for tool intents without outcomes and commit
    /// explicit `intent_classified` facts (B-05: committed-intent-without-
    /// outcome is the sufficient condition for interrupted/ambiguous). Run at
    /// open after recovery; outcomes of already-dispatched host work are
    /// always committed as facts, classified interrupted/ambiguous when the
    /// origin world is stale (R-02/C-03).
    pub fn classify_pending_intents(&mut self) -> Result<u64, SessionError> {
        let mut classified = 0u64;
        for intent in self.scan_pending_intents()? {
            if intent.kind != "tool_intent" {
                continue;
            }
            let Some(call_id) = intent.call_id else {
                continue;
            };
            let kind = match intent.origin_snapshot {
                Some(_) => "ambiguous",
                None => "interrupted",
            };
            let payload = json!({
                "call_id": call_id,
                "tool": intent.tool.unwrap_or_default(),
                "classification": kind,
                "reason": "committed intent without outcome (B-05)",
                "origin_snapshot": intent.origin_snapshot.map(|d| d.to_string()),
            });
            self.commit(
                vec![NewEvent {
                    kind: "intent_classified".into(),
                    payload_schema: 1,
                    payload,
                    objects: Vec::new(),
                    refs: Vec::new(),
                }],
                None,
            )?;
            classified += 1;
        }
        Ok(classified)
    }
}
// ---------- M4 projection helpers ----------

/// An empty fold (no claims, no edges, no history) for child runs without a
/// project binding.
fn empty_fold() -> RootFold {
    RootFold {
        root: None,
        claims: Vec::new(),
        edges: Vec::new(),
        retracted: Vec::new(),
        history: Vec::new(),
    }
}

/// Deterministic fold render for a scope-stable memory fragment: claims
/// sorted by ClaimId text, `"kind | content\n"` lines, capped at 16 KiB with
/// a truncation marker. Sensitivity = max claim sensitivity via
/// [`sensitivity_rank`].
fn render_memory_source(root: Digest, fold: &RootFold) -> MemoryFragmentSource {
    let mut claims: Vec<&(Digest, Claim)> = fold.claims.iter().collect();
    claims.sort_by_key(|(_, c)| c.claim_id.to_string());
    let mut text = String::new();
    for (_, claim) in &claims {
        text.push_str(&format!("{} | {}\n", claim.kind, claim.content));
    }
    if text.len() > 16 * 1024 {
        truncate_utf8(&mut text, 16 * 1024);
        text.push_str("…[truncated]");
    }
    let sensitivity = fold
        .claims
        .iter()
        .map(|(_, c)| c.sensitivity.as_str())
        .max_by_key(|s| sensitivity_rank(s))
        .unwrap_or("internal")
        .to_string();
    MemoryFragmentSource {
        root,
        text,
        sensitivity,
        claim_digests: fold.claims.iter().map(|(d, _)| *d).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// NEW-5: a memory fragment longer than the 16 KiB cap with a multibyte
    /// claim must truncate on a char boundary (plain `String::truncate` would
    /// panic here).
    #[test]
    fn memory_source_render_truncates_on_a_char_boundary() {
        let claim = Claim {
            schema: MEMORY_CLAIM_SCHEMA,
            claim_id: Id128::generate(),
            kind: "decision".into(),
            content: "é".repeat(9000),
            owner: Principal {
                session: Id128::generate(),
                generation: 1,
                run: None,
            },
            visibility_scope: MemoryScope::Lifetime,
            provenance: ClaimProvenance::new_ordinary(Id128::generate(), 1),
            observed_at: None,
            valid_from: None,
            sensitivity: "public".into(),
        };
        let mut fold = empty_fold();
        fold.claims.push((claim.digest(), claim));

        let source = render_memory_source(Digest::new(b"root"), &fold);
        assert!(source.text.ends_with("…[truncated]"));
        assert!(source.text.len() <= 16 * 1024 + "…[truncated]".len());
    }
}
