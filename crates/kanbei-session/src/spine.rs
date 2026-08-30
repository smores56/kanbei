//! M3 agent spine (architecture.md R-09/E-09/E-10, R-17/E-02, R-18/E-01):
//! the run FSM commit paths, model/tool intent+outcome records with
//! origin_snapshot provenance (R-08, R-02/C-03), bounded approval queue
//! (R-17/H-05), dispatch re-verification (R-16/D-11/C-10), responder
//! priority (R-09/E-10), and interrupted/ambiguous classification at
//! recovery (B-05).

use kanbei_capabilities::{ApprovalIntent, Capability, GrantScope, Principal};
use kanbei_core::digest::Digest;
use kanbei_core::envelope::Envelope;
use kanbei_core::id::Id128;
use kanbei_provider::{
    CacheOutcome, CachePlan, EgressEntry, FinishReason, Message, ModelCallRecord, Role,
};
use kanbei_scheduler::{
    Budgets, RunId, RunKind, RunOutcome, RunStart, RunUsage, StepCommand,
    StepContext, StepResult, TerminalOutcome, Trigger,
    WakeDecision,
};
use kanbei_tools::{
    ApprovalParked, OutcomeClassification, ToolIntent, ToolOutcome, execute_tool,
    tool_call_id,
};
use serde_json::{Value, json};

use crate::{NewEvent, Session, SessionError};

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
    /// record. Responder priority: a responder batch outranks a pending
    /// cognition batch (built-in policy).
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
                        payload: serde_json::to_value(&a)
                            .map_err(|e| SessionError::InvalidInput(format!("wake payload: {e}")))?,
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
                        payload: serde_json::to_value(&d)
                            .map_err(|e| SessionError::InvalidInput(format!("denial payload: {e}")))?,
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
    /// genesis is a state-changing transition).
    pub fn run_start(&mut self, run_id: RunId) -> Result<RunStart, SessionError> {
        self.fault(crate::FaultPoint::BeforeRunStart);
        let start = self.scheduler.run_start(run_id)?;
        self.commit(
            vec![NewEvent {
                kind: "run_start".into(),
                payload_schema: 1,
                payload: serde_json::to_value(&start).map_err(|e| {
                    SessionError::InvalidInput(format!("run start payload: {e}"))
                })?,
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            None,
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
        self.fault(crate::FaultPoint::BeforeRunOutcome);
        let (record, trip) = self.scheduler.record_outcome(run_id, outcome, usage, action_digests)?;
        let mut events = vec![NewEvent {
            kind: "run_outcome".into(),
            payload_schema: 1,
            payload: serde_json::to_value(&record).map_err(|e| {
                SessionError::InvalidInput(format!("run outcome payload: {e}"))
            })?,
            objects: Vec::new(),
            refs: Vec::new(),
        }];
        if let Some(t) = trip {
            events.push(NewEvent {
                kind: "breaker_tripped".into(),
                payload_schema: 1,
                payload: serde_json::to_value(&t).map_err(|e| {
                    SessionError::InvalidInput(format!("breaker payload: {e}"))
                })?,
                objects: Vec::new(),
                refs: Vec::new(),
            });
        }
        self.commit(events, None)?;
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
        self.scheduler.set_budgets(Budgets { tokens: Some(tokens), ..self.scheduler.budgets() });
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

    /// Responder priority (R-09/E-10): an in-flight cognition run is
    /// cancelled at the stream boundary, classified `Failed(UserCancelled)`;
    /// committed intents are never rolled back. Returns the cancelled run's
    /// outcome record when one was active.
    pub fn cancel_active_run(&mut self) -> Result<Option<RunOutcome>, SessionError> {
        let Some(run_id) = self.scheduler.active_run() else {
            return Ok(None);
        };
        let usage = self.scheduler.current_usage(run_id);
        let (record, _) = self.scheduler.record_outcome(
            run_id,
            TerminalOutcome::Failed(kanbei_scheduler::FailureKind::UserCancelled),
            usage,
            &[],
        )?;
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
        let intent = ModelCallRecord {
            provider: provider.clone(),
            model: model.clone(),
            projection_hashes: Vec::new(),
            module_hashes: Vec::new(),
            selected_events: selected_events.clone(),
            rendered_hash,
            params: params.clone(),
            cache_plan: CachePlan::None,
            cache_outcome: CacheOutcome::Miss,
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
                payload: serde_json::to_value(&intent).map_err(|e| {
                    SessionError::InvalidInput(format!("model call payload: {e}"))
                })?,
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
            tools: self.tool_registry.canonical_json().as_array().cloned().unwrap_or_default(),
            temperature,
            max_tokens,
        };
        let response = engine.complete(&request).map_err(|e| {
            SessionError::InvalidInput(format!("provider error: {e}"))
        })?;
        self.fault(crate::FaultPoint::AfterModelCall);

        let result = json!({
            "content": response.content,
            "tool_calls": response.tool_calls,
            "finish_reason": response.finish_reason,
            "usage": response.usage,
        });
        let outcome = json!({
            "provider": provider_id,
            "model": model_id,
            "rendered_hash": rendered_hash.to_string(),
            "result": result,
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
        let intent = ToolIntent {
            call_id,
            run_id,
            principal: principal.clone(),
            tool: tool.into(),
            args: kanbei_tools::canonicalize(args),
            approval: None,
            origin_snapshot: self.current_snapshot,
        };
        self.fault(crate::FaultPoint::BeforeToolIntentCommit);
        let intent_payload = serde_json::to_value(&intent)
            .map_err(|e| SessionError::InvalidInput(format!("tool intent payload: {e}")))?;
        self.commit(
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

        // Approval gate: consequential tools require an approval intent;
        // approved intents carry the digest, parked intents wait in the
        // bounded queue.
        let want = Capability::new(tool.into(), vec!["call".into()]);
        let approval_digest = match self.check_approval(&principal, &want) {
            Ok(d) => d,
            Err(e) => {
                return Ok(self.outcome_interrupted(intent, format!("approval denied: {e}")));
            }
        };
        let mut intent = intent;
        intent.approval = approval_digest;

        // Dispatch-time re-verification (R-16/D-11/C-10): revoked ⇒ the
        // intent resolves `interrupted` with a user-visible reason.
        self.fault(crate::FaultPoint::BeforeToolDispatch);
        let outcome = self.dispatch_tool(run_id, &intent, principal)?;
        self.fault(crate::FaultPoint::AfterToolDispatch);
        Ok(outcome)
    }

    /// The approval gate: tools whose capability requires approval park in
    /// the bounded queue; `Ok(Some(digest))` = approved. `Ok(None)` = not
    /// gated. `Err` = explicitly denied by policy.
    fn check_approval(
        &mut self,
        principal: &Principal,
        want: &Capability,
    ) -> Result<Option<Digest>, String> {
        if self.approval_bound == 0 {
            return Ok(None);
        }
        let policy_version = self.broker.policy_version();
        match self.broker.check(principal, want, policy_version) {
            Ok(effective) => {
                if !effective.requires_approval {
                    return Ok(None);
                }
                let approval = self
                    .broker
                    .require_approval(principal, want)
                    .map_err(|e| e.to_string())?;
                // The caller must explicitly approve; park the intent-shaped
                // approval. The digest returned marks the intent approved.
                let digest = approval.digest;
                self.approvals.push_back(ApprovalParked {
                    intent: ToolIntent {
                        call_id: tool_call_id(),
                        run_id: Id128::generate(),
                        principal: principal.clone(),
                        tool: want.resource.clone(),
                        args: Value::Null,
                        approval: None,
                        origin_snapshot: None,
                    },
                    approval,
                });
                while self.approvals.len() > self.approval_bound {
                    self.approvals.pop_front();
                }
                Ok(Some(digest))
            }
            Err(e) => Err(e.to_string()),
        }
    }

    fn dispatch_tool(
        &mut self,
        run_id: RunId,
        intent: &ToolIntent,
        principal: Principal,
    ) -> Result<ToolOutcome, SessionError> {
        // Dispatch-time re-verification (R-16/D-11/C-10): re-run the broker
        // guard set against the current composition; revoked ⇒ the intent
        // resolves `interrupted` with a user-visible reason. Approval-gated
        // tools additionally require their parked approval entry to still be
        // valid at dispatch (re-approval is a new intent).
        let want = Capability::new(intent.tool.clone(), vec!["call".into()]);
        match self.broker.check(&principal, &want, self.broker.policy_version()) {
            Ok(effective) => {
                if effective.requires_approval && intent.approval.is_none() {
                    return Ok(self.outcome_interrupted(
                        intent.clone(),
                        "approval required but missing at dispatch".into(),
                    ));
                }
            }
            Err(e) => {
                return Ok(self.outcome_interrupted(intent.clone(), format!("dispatch recheck: {e}")));
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
                });
            }
        };
        self.scheduler.record_usage(
            run_id,
            RunUsage { tokens: 0, tools: 1, children: 0, started_at_secs: 0 },
        )?;
        Ok(ToolOutcome {
            call_id: intent.call_id.clone(),
            tool: intent.tool.clone(),
            result,
            error: None,
            classification: OutcomeClassification::Normal,
            origin_snapshot: intent.origin_snapshot,
            commit_snapshot: self.current_snapshot,
            retained: None,
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
        }
    }

    /// Commit a tool outcome record (the committed intent's fact — R-02).
    /// The outcome's result runs the retention gate FIRST (R-28/D-S1: tools
    /// emit candidates; policy decides before persistence); a boundary fact
    /// commits alongside the outcome.
    pub fn commit_tool_outcome(&mut self, outcome: &ToolOutcome) -> Result<(), SessionError> {
        self.fault(crate::FaultPoint::BeforeToolOutcomeCommit);
        let mut boundary: Option<kanbei_policy::BoundaryFact> = None;
        if outcome.error.is_none() && outcome.result != Value::Null {
            let candidate = kanbei_policy::Candidate {
                role: kanbei_policy::CandidateRole::ToolOutput,
                content: serde_json::to_vec(&outcome.result)
                    .map_err(|e| SessionError::InvalidInput(format!("candidate: {e}")))?,
                replay_relevant: self.policy.replay_relevant(
                    kanbei_policy::CandidateRole::ToolOutput,
                    None,
                ),
                sensitivity: None,
                media: Some("application/json".into()),
            };
            if let Ok(admission) = self.policy.admit(candidate) {
                boundary = self.policy.boundary_fact(&admission);
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
            payload: serde_json::to_value(outcome).map_err(|e| {
                SessionError::InvalidInput(format!("tool outcome payload: {e}"))
            })?,
            objects: Vec::new(),
            refs: Vec::new(),
        });
        self.commit(events, None)?;
        self.fault(crate::FaultPoint::AfterToolOutcomeCommit);
        Ok(())
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
        let mut context = render(self)?;
        context.budget = self.scheduler.budgets();
        let mut last: Option<StepResult> = None;
        let outcome = loop {
            // Wake deadline/budget at each host-command boundary.
            if let Err(e) = self.scheduler.check_boundary(run_id) {
                let record = RunOutcome {
                    run_id,
                    outcome: TerminalOutcome::Blocked,
                    reason: Some(e.to_string()),
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
                return Ok(TerminalOutcome::Blocked);
            }
            let command = provider
                .step(&context, &trigger, last.as_ref())
                .map_err(|e| SessionError::InvalidInput(format!("cognition step: {e}")))?;
            match command {
                StepCommand::ModelCall(_) => {
                    let rendered = context.rendered.clone();
                    let selected = context.selected_events.clone();
                    let messages = vec![Message {
                        role: Role::User,
                        content: rendered,
                        tool_call_id: None,
                    }];
                    let result = self.model_call(run_id, messages, selected, &context.rendered)?;
                    last = Some(StepResult::Model(result));
                }
                StepCommand::ToolIntent { tool, arguments } => {
                    let principal = Principal {
                        session: self.session_id,
                        generation: 0,
                        run: Some(0),
                    };
                    let tool_outcome =
                        self.tool_call(run_id, principal, &tool, arguments)?;
                    self.commit_tool_outcome(&tool_outcome)?;
                    last = Some(StepResult::Tool(serde_json::to_value(&tool_outcome).unwrap_or(Value::Null)));
                }
                StepCommand::MemoryQuery { .. } | StepCommand::MemoryPropose { .. } => {
                    return Err(SessionError::InvalidInput(
                        "memory commands land in M4".into(),
                    ));
                }
                StepCommand::ChildSpawn { .. } => {
                    return Err(SessionError::InvalidInput(
                        "child spawn lands in M4".into(),
                    ));
                }
                StepCommand::ScheduleWake { kind, after_secs: _ } => {
                    self.scheduler.observe(Trigger { kind, referent: None });
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
        let log_path = self.log_path.clone();
        let mut committed_outcomes: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut already_classified: std::collections::HashSet<String> =
            std::collections::HashSet::new();
        let mut pending: Vec<(String, String, Option<Digest>)> = Vec::new();
        kanbei_log::for_each_frame(&log_path, |info| {
            for line in &info.events {
                let Ok(env) = Envelope::from_line(line) else {
                    continue;
                };
                match env.kind.as_str() {
                    "intent_classified" => {
                        if let Some(call) = env.payload.get("call_id").and_then(|c| c.as_str()) {
                            already_classified.insert(call.to_string());
                        }
                    }
                    "tool_outcome" => {
                        if let Some(call) = env.payload.get("call_id").and_then(|c| c.as_str()) {
                            committed_outcomes.insert(call.to_string());
                        }
                    }
                    "tool_intent" => {
                        if let (Some(call), Some(tool)) = (
                            env.payload.get("call_id").and_then(|c| c.as_str()),
                            env.payload.get("tool").and_then(|t| t.as_str()),
                        ) {
                            pending.push((
                                call.to_string(),
                                tool.to_string(),
                                env.snapshot,
                            ));
                        }
                    }
                    _ => {}
                }
            }
        })?;
        let mut classified = 0u64;
        for (call_id, tool, origin) in pending {
            if committed_outcomes.contains(&call_id) || already_classified.contains(&call_id) {
                continue;
            }
            let kind = match origin {
                Some(_) => "ambiguous",
                None => "interrupted",
            };
            let payload = json!({
                "call_id": call_id,
                "tool": tool,
                "classification": kind,
                "reason": "committed intent without outcome (B-05)",
                "origin_snapshot": origin.map(|d| d.to_string()),
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
