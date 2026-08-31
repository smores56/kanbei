//! kanbei-driver: the product-layer cognition driver over
//! [`kanbei_session::Session`].
//!
//! The session actor owns every FSM (wake admission, runs, tools, memory,
//! approvals); this crate owns the *policy* of driving: which wake to
//! accept next, which host command to issue from a model response, and when
//! a turn is complete. Every decision it makes is canonically recorded by
//! the session; the driver adds no state of its own beyond the run it is
//! in.
//!
//! Conversation continuity across tool round-trips: a run's projection is
//! frozen at render time (`project_context`), so a model call inside a run
//! cannot see a fact committed in that same run. The driver therefore ends
//! the run after each committed fact and wakes again — the next run
//! re-projects from canonical facts (the M4 staged pipeline renders the
//! trajectory, tool intents, and outcomes as text), so the model sees the
//! outcome on its next call. One tool round-trip is one additional
//! wake/run: each remains a bounded, budget-checked, canonically recorded
//! unit (R-18/E-01, R-09).

// SessionError embeds ModuleError/ScopeError/ServiceError (large variants);
// mirrors the crate-level allow in kanbei-session and its siblings.
#![allow(clippy::result_large_err)]

use kanbei_provider::ToolCall;
use kanbei_scheduler::{
    CognitionProvider, ModelCallSpec, StepCommand, StepContext, StepError, StepResult,
    TerminalOutcome, Trigger, TriggerKind,
};
use kanbei_session::{NewEvent, Session, SessionError};
use serde_json::{json, Value};

/// The provider-backed cognition seam: maps the model's own responses to
/// host commands.
///
/// - first step (no previous result): a model call against the run's
///   frozen projection;
/// - a model response with tool calls: execute the FIRST call as a tool
///   intent (parallel calls serialize across wakes — the response, with its
///   remaining calls, stays visible to the model in the committed
///   `model_outcome` trajectory text);
/// - a model response without tool calls: the model's content is the turn's
///   final answer; finish the run (a `model_outcome` fact was committed, so
///   the outcome is `Progress`);
/// - any committed fact result (tool/memory/child outcome): finish the run
///   with `Progress` — the model cannot see the fact until the next wake
///   re-projects (see the crate docs).
#[derive(Debug, Default)]
pub struct EngineProvider {
    /// True when the last model response had no outstanding tool call:
    /// the model's content is the turn's final answer.
    pub completed: bool,
    /// The final answer content (`None` when the model returned none —
    /// e.g. a pure tool-call response that was interrupted by a breaker).
    pub final_content: Option<String>,
}

impl CognitionProvider for EngineProvider {
    fn step(
        &mut self,
        context: &StepContext,
        _trigger: &Trigger,
        last: Option<&StepResult>,
    ) -> Result<StepCommand, StepError> {
        match last {
            None => Ok(StepCommand::ModelCall(ModelCallSpec {
                rendered_hash: context.rendered_hash,
                max_tokens: None,
            })),
            Some(StepResult::Model(result)) => {
                let calls: Vec<ToolCall> = result
                    .get("tool_calls")
                    .cloned()
                    .and_then(|v| serde_json::from_value(v).ok())
                    .unwrap_or_default();
                match calls.into_iter().next() {
                    Some(call) => Ok(StepCommand::ToolIntent {
                        tool: call.name,
                        arguments: call.arguments,
                    }),
                    None => {
                        self.completed = true;
                        self.final_content =
                            result.get("content").and_then(Value::as_str).map(str::to_string);
                        Ok(StepCommand::Finish(TerminalOutcome::Progress))
                    }
                }
            }
            Some(StepResult::Tool(_) | StepResult::Memory(_) | StepResult::Child(_)) => {
                Ok(StepCommand::Finish(TerminalOutcome::Progress))
            }
            Some(StepResult::Scheduled) => Ok(StepCommand::Finish(TerminalOutcome::Waiting)),
        }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The observable result of driving wakes after a triggering fact (a user
/// message, an explicit resume).
#[derive(Debug, Default)]
pub struct Turn {
    /// The model's final answer — `None` when no run completed without an
    /// outstanding tool call (wake denied, breaker trip, provider error,
    /// ...). The responsible constraint is canonically recorded.
    pub answer: Option<String>,
    /// Wakes accepted and run for this turn.
    pub runs: u32,
    /// Terminal outcome of the last run, when one ran.
    pub last_outcome: Option<TerminalOutcome>,
}

/// Drives a [`Session`] from canonical facts to quiescence.
pub struct Driver {
    session: Session,
    provider: EngineProvider,
}

impl Driver {
    pub fn new(session: Session) -> Self {
        Self {
            session,
            provider: EngineProvider::default(),
        }
    }

    /// Commits the user message as a canonical `user_message` fact
    /// (payload `{"text": ...}`, schema 1 — the M5 gate contract) and
    /// drives the resulting wakes to quiescence.
    pub fn user_turn(&mut self, text: &str) -> Result<Turn, SessionError> {
        self.session.commit(
            vec![NewEvent {
                kind: "user_message".into(),
                payload_schema: 1,
                payload: json!({ "text": text }),
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            None,
        )?;
        self.session
            .observe_trigger(Trigger { kind: TriggerKind::UserMessage, referent: None });
        self.drive_to_quiescence()
    }

    /// Explicitly resumes cognition after a canonical breaker pause, then
    /// drives pending wakes to quiescence.
    pub fn resume(&mut self) -> Result<Turn, SessionError> {
        self.session.resume_cognition()?;
        self.drive_to_quiescence()
    }

    /// Drives pending wakes: accept → run start → bounded cognition loop.
    /// Continues while the last run committed a fact and the model still
    /// wants to act (the tool round-trip). Stops on: the model's final
    /// answer, a wake denial (the scheduler commits the responsible
    /// constraint as `wake_denied`), or a terminal outcome that is not a
    /// continuation (`Blocked`, `Failed`, `Waiting`, `NoProgress`).
    pub fn drive_to_quiescence(&mut self) -> Result<Turn, SessionError> {
        let mut turn = Turn::default();
        loop {
            let Some(run) = self.session.accept_wake()? else {
                return Ok(turn);
            };
            turn.runs += 1;
            self.session.run_start(run.run_id)?;
            self.provider = EngineProvider::default();
            let trigger = run.trigger.clone();
            let outcome = match self.session.cognition_loop(
                run.run_id,
                trigger.clone(),
                &mut self.provider,
                |sess: &mut Session| sess.project_context(run.run_id, &trigger),
            ) {
                Ok(outcome) => outcome,
                Err(e) => {
                    // A mid-run error (provider failure, projection error)
                    // leaves the run active; without a terminal outcome the
                    // active slot denies every later wake (ConcurrencyLimit).
                    // Cancel it — a canonical Failed record — then surface
                    // the error so the caller can retry or report.
                    self.session.cancel_active_run()?;
                    return Err(e);
                }
            };
            turn.last_outcome = Some(outcome);
            if self.provider.completed {
                turn.answer = self.provider.final_content.take();
                return Ok(turn);
            }
            match outcome {
                TerminalOutcome::Progress | TerminalOutcome::CompletedGoal => {
                    // A new canonical fact the model must see: wake on it.
                    self.session
                        .observe_trigger(Trigger { kind: TriggerKind::NewCausalEvent, referent: None });
                }
                _ => return Ok(turn),
            }
        }
    }

    pub fn session(&self) -> &Session {
        &self.session
    }

    pub fn session_mut(&mut self) -> &mut Session {
        &mut self.session
    }

    pub fn into_session(self) -> Session {
        self.session
    }
}
