//! kanbei-transcript — the transcript projection service (decision 30).
//!
//! A tier-2, replaceable projection of committed envelopes into a typed
//! conversation view: turn segmentation, thought-vs-response classification,
//! tool intent/outcome pairing, collapse defaults and streaming state. The
//! machine is a total function of the envelope stream it is fed — applying the
//! same envelopes (a fresh session replays the full log on open) yields the
//! same view — so a rebuilt projection after resume is byte-identical.
//!
//! The session owns and drives the projection: it applies committed envelopes
//! on the commit path, feeds provider-stream deltas (a home for partials, which
//! stay non-canonical and are never committed), finalizes turns, and exposes
//! the resulting [`TranscriptView`]. The projection contains no UI types;
//! `kanbei-ui` renders the typed view to `SemanticTree` primitives.
//!
//! Typing (structural, doc-faithful): the response is the turn-terminal
//! `model_outcome` (content without pending tool calls); thoughts are
//! intermediate `model_outcome` content plus tool steps; the turn end-state
//! comes from `run_outcome`. Opaque artifacts never enter the projection
//! (M6/S9) and are not rendered (R-19: message identity is its committing
//! event; launch is always resume).

use std::collections::HashSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use kanbei_core::envelope::Envelope;

/// The turn's terminal classification (UI vocabulary for the scheduler's
/// `TerminalOutcome`, kept dependency-free).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum OutcomeClass {
    Progress,
    CompletedGoal,
    NoProgress,
    Waiting,
    Blocked,
    Failed,
}

/// Parse the `run_outcome` payload's terminal outcome. The scheduler
/// serializes unit variants as strings and `Failed(FailureKind)` as an
/// object with the kind string.
pub fn parse_outcome(payload: &Value) -> Option<(OutcomeClass, Option<String>)> {
    let outcome = payload.get("outcome")?;
    let class = match outcome {
        Value::String(s) => match s.as_str() {
            "Progress" => OutcomeClass::Progress,
            "CompletedGoal" => OutcomeClass::CompletedGoal,
            "NoProgress" => OutcomeClass::NoProgress,
            "Waiting" => OutcomeClass::Waiting,
            "Blocked" => OutcomeClass::Blocked,
            _ => return None,
        },
        Value::Object(o) => {
            let reason = o.get("Failed")?.as_str()?;
            match reason {
                "Deadline" | "UserCancelled" | "Provider" | "Tool" | "Internal" | "Quiesced" => {
                    OutcomeClass::Failed
                }
                _ => return None,
            }
        }
        _ => return None,
    };
    let reason = payload
        .get("reason")
        .and_then(Value::as_str)
        .map(str::to_string);
    Some((class, reason))
}

/// Tool step status, from the `tool_outcome` classification (R-02/C-03).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum StepStatus {
    /// The `tool_intent` committed; no outcome yet.
    InFlight,
    Ok,
    /// `Interrupted(reason)` — denied, stale, or approval-denied.
    Interrupted,
    /// `Ambiguous(reason)` — outcome of possibly-dispatched work.
    Ambiguous,
}

/// One row of a turn's working segment (thought bubble).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BubbleRow {
    /// Intermediate model content (a thought).
    Text(String),
    /// A tool step (call + outcome, paired by call_id).
    Step(ToolStep),
    /// A kernel notice (wake denied, breaker trip, resume) — dimmed.
    Notice(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolStep {
    pub call_id: String,
    pub tool: String,
    /// Canonical argument JSON (display; truncated at render).
    pub args: String,
    pub status: StepStatus,
    /// Classification reason (denial / interruption / ambiguity text), raw;
    /// the renderer joins and truncates.
    pub detail: String,
    /// Outcome error text, raw; the renderer joins and truncates.
    pub error: String,
    /// Serialized result payload (`null` → empty), raw and untruncated; the
    /// renderer joins and truncates.
    pub result: String,
}

/// The turn's rendered end-state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TurnState {
    Running,
    /// Terminal `Progress`/`CompletedGoal` (a clean continuation stop).
    Completed,
    Failed,
    /// `Blocked`/`NoProgress`/`Waiting` (a responsible constraint stopped
    /// the turn; the denial/breaker notice names it).
    Blocked,
    /// Replayed from the log with no terminal record (the session died
    /// mid-turn) — B-05: the intent story is canonically classified.
    Interrupted,
}

impl TurnState {
    /// The end-state a recorded terminal outcome maps to: `Progress`/
    /// `CompletedGoal` → Completed, `Failed` → Failed, the responsible-stop
    /// classes → Blocked. No recorded outcome means nothing closed the turn
    /// while it was active (the session died mid-turn) → Interrupted.
    pub fn from_outcome(last: Option<(OutcomeClass, Option<String>)>) -> Self {
        match last {
            Some((OutcomeClass::Progress | OutcomeClass::CompletedGoal, _)) => {
                TurnState::Completed
            }
            Some((OutcomeClass::Failed, _)) => TurnState::Failed,
            Some(_) => TurnState::Blocked,
            None => TurnState::Interrupted,
        }
    }
}

/// One user turn: the message, its working segment, the final answer, and
/// the recorded terminal state. Wall-clock metadata is deliberately absent:
/// the view is a pure function of the envelope stream, so resume rebuilds are
/// identical.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnView {
    pub user: String,
    /// The turn's working segment, in commit order (thoughts and tool
    /// steps interleaved).
    pub thoughts: Vec<BubbleRow>,
    /// The turn's final answer (the terminal model_outcome content).
    pub response: Option<String>,
    pub state: TurnState,
    /// Run-failure reason (the `run_outcome` reason for a failed turn).
    pub reason: Option<String>,
    pub runs: u32,
    pub tools: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    /// The current run's terminal outcome as recorded, cleared by a
    /// superseding `run_start` (replay safety: a replayed turn whose driver
    /// result is unknown resolves its state from this; `None` means nothing
    /// closed the turn while it was active).
    pub last_outcome: Option<(OutcomeClass, Option<String>)>,
    /// Derived, per-view only: whether the turn's thought bubble is open
    /// (running, or the user re-opened it). Never stored in the projection
    /// state; the renderer may OR the live overrides on top.
    pub open: bool,
    /// Non-canonical in-flight stream text (provider deltas); cleared on
    /// [`TranscriptProjection::end_stream`]. Never committed.
    pub streaming: Option<String>,
}

impl TurnView {
    fn new(user: String) -> Self {
        TurnView {
            user,
            thoughts: Vec::new(),
            response: None,
            state: TurnState::Running,
            reason: None,
            runs: 0,
            tools: 0,
            input_tokens: 0,
            output_tokens: 0,
            last_outcome: None,
            open: false,
            streaming: None,
        }
    }
}

/// The whole transcript as a typed view: turns in commit order. A pure
/// function of the envelope stream applied so far (plus finalize events, which
/// mirror committed terminal records).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptView {
    pub turns: Vec<TurnView>,
}

impl TranscriptView {
    /// Total model egress across all turns (input, output) — status bar.
    pub fn tokens(&self) -> (u64, u64) {
        let mut tin = 0u64;
        let mut tout = 0u64;
        for t in &self.turns {
            tin += t.input_tokens;
            tout += t.output_tokens;
        }
        (tin, tout)
    }
}

/// Session-local manual collapse overrides: the turns the user re-opened
/// (Q6/Q5: bubbles collapse on completion, expand on demand). Passed per-view;
/// never stored in the projection, so the projection's defaults stay a pure
/// function of the committed envelopes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CollapseOverrides {
    expanded: HashSet<String>,
}

impl CollapseOverrides {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn contains(&self, turn: usize) -> bool {
        self.expanded.contains(&format!("t{turn}"))
    }

    /// Flip a turn's manual override (running turns are always open; the
    /// override takes effect once the turn settles).
    pub fn toggle(&mut self, turn: usize) {
        let key = format!("t{turn}");
        if !self.expanded.remove(&key) {
            self.expanded.insert(key);
        }
    }

    /// Merge another override set into this one (union). The kernel uses it to
    /// layer a mount's own overrides over the session-local set: a module's
    /// collapse toggle reaches only that mount's render context.
    pub fn union_with(&mut self, other: &CollapseOverrides) {
        self.expanded.extend(other.expanded.iter().cloned());
    }
}

/// The replaceable typed contract (decision 30): a transcript projection is
/// driven by the session and yields a deterministic typed view. Inject a
/// replacement through `SessionConfig::transcript`; the session and the UI
/// need no changes. Mirrors the `ProviderEngine` seam shape.
pub trait TranscriptProjection: Send {
    /// Implementation identity (diagnostics / egress pins).
    fn name(&self) -> &str;
    /// Apply one committed envelope (the only canonical mutation entry point
    /// besides [`Self::finalize_turn`]/[`Self::finish_replay`]). Unknown kinds
    /// are kernel records the transcript does not surface.
    fn apply(&mut self, env: &Envelope);
    /// Close the active turn with the driver's observed result (the worker
    /// stopped driving; the terminal `run_outcome` is already in the log).
    /// `None` = resolve from the recorded terminal outcome.
    fn finalize_turn(&mut self, last: Option<OutcomeClass>);
    /// End of a replay (session open / resume): every turn still active has no
    /// driver result — resolve it from its own recorded terminal outcome when
    /// one closed its last run, else mark it interrupted (B-05: the log is the
    /// authority).
    fn finish_replay(&mut self);
    /// Append a non-canonical provider-stream fragment to the in-flight
    /// partial (never committed).
    fn apply_delta(&mut self, fragment: &str);
    /// The provider stream ended: clear the in-flight partial.
    fn end_stream(&mut self);
    /// The typed view, with the given session-local overrides applied.
    fn view(&self, overrides: &CollapseOverrides) -> TranscriptView;
    fn as_any(&self) -> &dyn std::any::Any;
}

/// The built-in projection: the extracted conversation state machine.
#[derive(Debug, Clone, Default)]
pub struct ConversationProjection {
    turns: Vec<TurnView>,
}

impl ConversationProjection {
    pub fn new() -> Self {
        Self::default()
    }

    fn active(&self) -> Option<usize> {
        self.turns
            .iter()
            .rposition(|t| t.state == TurnState::Running)
    }

    /// One `model_outcome` payload (see [`Self::apply`]).
    fn apply_model_outcome(turn: &mut TurnView, payload: &Value) {
        // The response content and pending tool calls live in the
        // CompletionResponse (`result`) the session committed; the egress
        // record carries the token usage.
        let result = payload.get("result");
        let content = result
            .and_then(|r| r.get("content"))
            .and_then(Value::as_str);
        let has_calls = result
            .and_then(|r| r.get("tool_calls"))
            .and_then(Value::as_array)
            .is_some_and(|v| !v.is_empty());
        if let Some(egress) = payload.get("egress") {
            turn.input_tokens += egress
                .get("input_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            turn.output_tokens += egress
                .get("output_tokens")
                .and_then(Value::as_u64)
                .unwrap_or(0);
        }
        // A committed outcome supersedes any in-flight partial.
        turn.streaming = None;
        match (content, has_calls) {
            (Some(text), false) => {
                // Turn-terminal: the model stopped without outstanding tool
                // calls — its content is the answer (Q3 structural rule).
                turn.response = Some(text.to_string());
            }
            (Some(text), true) => {
                // Intermediate: the model is still acting — thought text.
                turn.thoughts.push(BubbleRow::Text(text.to_string()));
            }
            (None, _) => {
                // Tool-only call (null content): no phantom text (Q4).
            }
        }
    }

    /// One `tool_intent` payload (see [`Self::apply`]).
    fn apply_tool_intent(turn: &mut TurnView, payload: &Value) {
        turn.tools += 1;
        turn.thoughts.push(BubbleRow::Step(ToolStep {
            call_id: payload
                .get("call_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            tool: payload
                .get("tool")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            args: payload.get("args").map(Value::to_string).unwrap_or_default(),
            status: StepStatus::InFlight,
            detail: String::new(),
            error: String::new(),
            result: String::new(),
        }));
    }

    /// One `tool_outcome` payload (see [`Self::apply`]).
    fn apply_tool_outcome(turn: &mut TurnView, payload: &Value) {
        let call_id = payload
            .get("call_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(step) = turn
            .thoughts
            .iter_mut()
            .filter_map(|row| match row {
                BubbleRow::Step(s) if s.call_id == call_id => Some(s),
                _ => None,
            })
            .last()
        else {
            return;
        };
        // OutcomeClassification serializes unit variants as plain strings
        // and newtypes as single-key objects.
        let (status, detail) = match payload.get("classification") {
            Some(Value::String(_)) => (StepStatus::Ok, String::new()),
            Some(Value::Object(o)) => {
                let reason = match o
                    .get("Interrupted")
                    .or_else(|| o.get("Denied"))
                    .or_else(|| o.get("Ambiguous"))
                {
                    Some(Value::String(s)) => s.clone(),
                    _ => String::new(),
                };
                (
                    if o.contains_key("Interrupted") || o.contains_key("Denied") {
                        StepStatus::Interrupted
                    } else {
                        StepStatus::Ambiguous
                    },
                    reason,
                )
            }
            _ => (StepStatus::Ok, String::new()),
        };
        let error = payload
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        // Carry the outcome components raw: joining and truncating is a
        // presentation concern owned by the renderer.
        let result = payload
            .get("result")
            .filter(|v| !v.is_null())
            .map(Value::to_string)
            .unwrap_or_default();
        step.status = status;
        step.detail = detail;
        step.error = error;
        step.result = result;
    }
}

impl TranscriptProjection for ConversationProjection {
    fn name(&self) -> &str {
        "conversation"
    }

    fn apply(&mut self, env: &Envelope) {
        match env.kind.as_str() {
            "user_message" => {
                let text = env
                    .payload
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string();
                self.turns.push(TurnView::new(text));
            }
            "run_start" => {
                if let Some(i) = self.active() {
                    let turn = &mut self.turns[i];
                    turn.runs += 1;
                    // A new run supersedes the previous run's terminal record:
                    // a turn still running at replay then has no outcome
                    // applied while it was active, so its state must not be
                    // resolved from an unrelated earlier outcome.
                    turn.last_outcome = None;
                    turn.reason = None;
                }
            }
            "model_outcome" => {
                if let Some(i) = self.active() {
                    Self::apply_model_outcome(&mut self.turns[i], &env.payload);
                }
            }
            "tool_intent" => {
                if let Some(i) = self.active() {
                    Self::apply_tool_intent(&mut self.turns[i], &env.payload);
                }
            }
            "tool_outcome" => {
                if let Some(i) = self.active() {
                    Self::apply_tool_outcome(&mut self.turns[i], &env.payload);
                }
            }
            "run_outcome" => {
                if let Some(i) = self.active()
                    && let Some((class, reason)) = parse_outcome(&env.payload)
                {
                    let turn = &mut self.turns[i];
                    turn.last_outcome = Some((class, reason.clone()));
                    if class == OutcomeClass::Failed {
                        turn.reason = reason;
                    }
                }
            }
            "wake_denied" => {
                if let Some(i) = self.active() {
                    let reason = env
                        .payload
                        .get("reason")
                        .map(|v| match v {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        })
                        .unwrap_or_else(|| "unknown".into());
                    self.turns[i]
                        .thoughts
                        .push(BubbleRow::Notice(format!("wake denied: {reason}")));
                }
            }
            "breaker_tripped" => {
                if let Some(i) = self.active() {
                    let counter = env
                        .payload
                        .get("counter")
                        .and_then(Value::as_str)
                        .unwrap_or("unknown");
                    let value = env
                        .payload
                        .get("value")
                        .and_then(Value::as_u64)
                        .unwrap_or_default();
                    let threshold = env
                        .payload
                        .get("threshold")
                        .and_then(Value::as_u64)
                        .unwrap_or_default();
                    self.turns[i].thoughts.push(BubbleRow::Notice(format!(
                        "breaker tripped: {counter} {value} ≥ {threshold}"
                    )));
                }
            }
            "cognition_resumed" => {
                if let Some(i) = self.active() {
                    self.turns[i]
                        .thoughts
                        .push(BubbleRow::Notice("cognition resumed".into()));
                }
            }
            _ => {}
        }
    }

    fn finalize_turn(&mut self, last: Option<OutcomeClass>) {
        let Some(i) = self.active() else {
            return;
        };
        let turn = &mut self.turns[i];
        // The driver's observed class wins; it carries no reason, so reuse the
        // reason recorded for the same class (the `run_outcome` kept it). A
        // `None` result falls back to the recorded terminal outcome; when
        // neither exists nothing closed the turn while it was active →
        // Interrupted.
        let effective = match last {
            Some(class) => {
                let reason = turn
                    .last_outcome
                    .as_ref()
                    .filter(|(c, _)| *c == class)
                    .and_then(|(_, r)| r.clone());
                Some((class, reason))
            }
            None => turn.last_outcome.clone(),
        };
        // The driver supplies only the class; keep the displayed failure reason
        // in sync with the effective outcome so the recorded reason survives
        // (`last.map(|c| (c, None))` used to drop it).
        if let Some((OutcomeClass::Failed, reason)) = &effective {
            turn.reason = reason.clone();
        }
        turn.state = TurnState::from_outcome(effective);
    }

    fn finish_replay(&mut self) {
        // Every turn still Running at replay's end was closed live only by
        // `finalize_turn` (not a log event): resolve it from its own recorded
        // terminal outcome. A turn with none died mid-turn — its last outcome
        // was cleared by the superseding `run_start` — so it is Interrupted.
        for turn in &mut self.turns {
            if turn.state == TurnState::Running {
                turn.state = TurnState::from_outcome(turn.last_outcome.clone());
            }
        }
    }

    fn apply_delta(&mut self, fragment: &str) {
        if let Some(i) = self.active() {
            self.turns[i]
                .streaming
                .get_or_insert_with(String::new)
                .push_str(fragment);
        }
    }

    fn end_stream(&mut self) {
        // Clear the partial on every Running turn: replay can transiently hold
        // more than one (finalize is not a log event), and a partial must not
        // survive on an earlier one.
        for turn in &mut self.turns {
            if turn.state == TurnState::Running {
                turn.streaming = None;
            }
        }
    }

    fn view(&self, overrides: &CollapseOverrides) -> TranscriptView {
        // Collapse defaults are a pure function of the envelope stream: a
        // running turn is always open (live steps + spinner), a settled turn
        // collapses. Overrides re-open settled turns per-view only.
        let turns = self
            .turns
            .iter()
            .enumerate()
            .map(|(n, turn)| {
                let mut turn = turn.clone();
                turn.open = turn.state == TurnState::Running || overrides.contains(n);
                turn
            })
            .collect();
        TranscriptView { turns }
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kanbei_core::envelope::{Envelope, ENVELOPE_SCHEMA};
    use serde_json::json;

    fn env(seq: u64, kind: &str, payload: Value) -> Envelope {
        Envelope {
            env: ENVELOPE_SCHEMA,
            seq,
            evt: format!("e{seq}"),
            kind: kind.into(),
            payload_schema: 1,
            payload,
            refs: Vec::new(),
            snapshot: None,
        }
    }

    fn model_outcome(content: Option<&str>, calls: &[&str], tin: u64, tout: u64) -> Value {
        let mut result = json!({ "content": content, "tool_calls": [], "finish_reason": "stop" });
        if !calls.is_empty() {
            result["tool_calls"] = json!(
                calls.iter().map(|c| json!({ "id": "call_1", "name": c, "arguments": {} })).collect::<Vec<_>>()
            );
        }
        json!({
            "provider": "p", "model": "m", "rendered_hash": "h",
            "result": result,
            "egress": { "input_tokens": tin, "output_tokens": tout },
        })
    }

    fn default_view(p: &ConversationProjection) -> TranscriptView {
        p.view(&CollapseOverrides::new())
    }

    #[test]
    fn user_message_opens_a_turn() {
        let mut s = ConversationProjection::new();
        s.apply(&env(1, "user_message", json!({ "text": "hello" })));
        assert_eq!(s.turns.len(), 1);
        assert_eq!(s.turns[0].user, "hello");
        assert_eq!(s.turns[0].state, TurnState::Running);
    }

    #[test]
    fn terminal_outcome_without_calls_is_the_response() {
        let mut s = ConversationProjection::new();
        s.apply(&env(1, "user_message", json!({ "text": "hi" })));
        s.apply(&env(2, "model_outcome", model_outcome(Some("the answer"), &[], 10, 5)));
        assert_eq!(s.turns[0].response.as_deref(), Some("the answer"));
        assert!(s.turns[0].thoughts.is_empty());
        assert_eq!(s.turns[0].input_tokens, 10);
        assert_eq!(s.turns[0].output_tokens, 5);
    }

    #[test]
    fn intermediate_outcome_with_calls_is_thought() {
        let mut s = ConversationProjection::new();
        s.apply(&env(1, "user_message", json!({ "text": "hi" })));
        s.apply(&env(2, "model_outcome", model_outcome(Some("let me check"), &["fs.read"], 1, 2)));
        assert_eq!(s.turns[0].response, None);
        assert!(matches!(s.turns[0].thoughts[0], BubbleRow::Text(ref t) if t == "let me check"));
    }

    #[test]
    fn tool_only_outcome_has_no_phantom_text() {
        let mut s = ConversationProjection::new();
        s.apply(&env(1, "user_message", json!({ "text": "hi" })));
        s.apply(&env(2, "model_outcome", model_outcome(None, &["fs.read"], 1, 2)));
        assert!(s.turns[0].thoughts.is_empty());
        assert_eq!(s.turns[0].response, None);
    }

    #[test]
    fn tool_intent_and_outcome_pair_by_call_id() {
        let mut s = ConversationProjection::new();
        s.apply(&env(1, "user_message", json!({ "text": "hi" })));
        s.apply(&env(
            2,
            "tool_intent",
            json!({ "call_id": "c1", "tool": "fs.read", "args": { "path": "a" } }),
        ));
        assert!(matches!(s.turns[0].thoughts[0], BubbleRow::Step(ref st)
            if st.call_id == "c1" && st.tool == "fs.read" && st.status == StepStatus::InFlight));
        s.apply(&env(
            3,
            "tool_outcome",
            json!({ "call_id": "c1", "tool": "fs.read", "result": "data", "error": null,
                    "classification": "Normal" }),
        ));
        match &s.turns[0].thoughts[0] {
            BubbleRow::Step(st) => {
                assert_eq!(st.status, StepStatus::Ok);
            }
            _ => panic!("step row"),
        }
        // denied (Interrupted) carries the reason
        s.apply(&env(4, "tool_intent", json!({ "call_id": "c2", "tool": "fs.write", "args": {} })));
        s.apply(&env(
            5,
            "tool_outcome",
            json!({ "call_id": "c2", "tool": "fs.write", "result": null, "error": null,
                    "classification": { "Interrupted": "approval denied by user" } }),
        ));
        match &s.turns[0].thoughts[1] {
            BubbleRow::Step(st) => {
                assert_eq!(st.status, StepStatus::Interrupted);
                assert_eq!(st.detail, "approval denied by user");
            }
            _ => panic!("step row"),
        }
    }

    #[test]
    fn run_outcome_records_terminal_and_finalize_closes_the_turn() {
        let mut s = ConversationProjection::new();
        s.apply(&env(1, "user_message", json!({ "text": "hi" })));
        s.apply(&env(2, "run_start", json!({})));
        assert_eq!(s.turns[0].runs, 1);
        s.apply(&env(3, "run_outcome", json!({
            "run_id": "r", "outcome": "Progress", "reason": null
        })));
        s.finalize_turn(Some(OutcomeClass::Progress));
        assert_eq!(s.turns[0].state, TurnState::Completed);
        assert!(s.active().is_none());
    }

    #[test]
    fn failed_run_carries_the_reason() {
        let mut s = ConversationProjection::new();
        s.apply(&env(1, "user_message", json!({ "text": "hi" })));
        s.apply(&env(2, "run_outcome", json!({
            "run_id": "r",
            "outcome": { "Failed": "Provider" },
            "reason": "provider 500"
        })));
        s.finalize_turn(None);
        assert_eq!(s.turns[0].state, TurnState::Failed);
        assert_eq!(s.turns[0].reason.as_deref(), Some("provider 500"));
    }

    /// A driver class passed to `finalize_turn` carries no reason, so the
    /// reason recorded for the same class is preserved.
    #[test]
    fn finalize_preserves_the_recorded_reason_for_the_same_class() {
        let mut s = ConversationProjection::new();
        s.apply(&env(1, "user_message", json!({ "text": "hi" })));
        s.apply(&env(2, "run_outcome", json!({
            "run_id": "r",
            "outcome": { "Failed": "Provider" },
            "reason": "provider 500"
        })));
        s.finalize_turn(Some(OutcomeClass::Failed));
        assert_eq!(s.turns[0].state, TurnState::Failed);
        assert_eq!(s.turns[0].reason.as_deref(), Some("provider 500"));
        // A different class is unrelated to the recorded reason.
        let mut s = ConversationProjection::new();
        s.apply(&env(1, "user_message", json!({ "text": "hi" })));
        s.apply(&env(2, "run_outcome", json!({
            "run_id": "r",
            "outcome": { "Failed": "Provider" },
            "reason": "provider 500"
        })));
        s.finalize_turn(Some(OutcomeClass::Progress));
        assert_eq!(s.turns[0].state, TurnState::Completed);
    }

    #[test]
    fn replay_finish_resolves_a_leftover_active_turn() {
        let mut s = ConversationProjection::new();
        s.apply(&env(1, "user_message", json!({ "text": "hi" })));
        s.apply(&env(2, "run_outcome", json!({
            "run_id": "r", "outcome": "Blocked", "reason": null
        })));
        s.finish_replay();
        assert_eq!(s.turns[0].state, TurnState::Blocked);
    }

    /// Resume identity: a turn closed live by `finalize_turn` (not a log
    /// event) resolves from its recorded outcome at replay; a turn whose last
    /// run never reached an outcome is Interrupted, and every leftover Running
    /// turn is resolved, not just the last.
    #[test]
    fn replay_resolves_every_leftover_running_turn() {
        let mut s = ConversationProjection::new();
        s.apply(&env(1, "user_message", json!({ "text": "a" })));
        s.apply(&env(2, "run_start", json!({})));
        s.apply(&env(3, "run_outcome", json!({
            "run_id": "r", "outcome": "Progress", "reason": null
        })));
        s.apply(&env(4, "user_message", json!({ "text": "b" })));
        s.apply(&env(5, "run_start", json!({})));
        s.finish_replay();
        assert_eq!(s.turns[0].state, TurnState::Completed);
        assert_eq!(s.turns[1].state, TurnState::Interrupted);
    }

    /// A mid-turn death (no outcome applied while the turn was active) is
    /// Interrupted, and the live path (`finalize_turn(None)`) and the replayed
    /// path agree.
    #[test]
    fn mid_turn_death_is_interrupted_and_matches_live() {
        let stream = [
            env(1, "user_message", json!({ "text": "hi" })),
            env(2, "run_start", json!({})),
            env(3, "model_outcome", model_outcome(Some("thinking"), &["fs.read"], 1, 2)),
            env(
                4,
                "tool_intent",
                json!({ "call_id": "c1", "tool": "fs.read", "args": { "path": "a" } }),
            ),
        ];
        let mut live = ConversationProjection::new();
        for env in &stream {
            live.apply(env);
        }
        live.finalize_turn(None);
        assert_eq!(live.turns[0].state, TurnState::Interrupted);

        let mut replay = ConversationProjection::new();
        for env in &stream {
            replay.apply(env);
        }
        replay.finish_replay();
        assert_eq!(replay.turns[0].state, TurnState::Interrupted);
        assert_eq!(default_view(&live), default_view(&replay));
    }

    #[test]
    fn wake_denial_and_breaker_become_notices() {
        let mut s = ConversationProjection::new();
        s.apply(&env(1, "user_message", json!({ "text": "hi" })));
        s.apply(&env(
            2,
            "wake_denied",
            json!({ "kind": "Main", "trigger_kind": "UserMessage", "reason": "Paused" }),
        ));
        s.apply(&env(
            3,
            "breaker_tripped",
            json!({ "counter": "IdenticalAction", "value": 3, "threshold": 3 }),
        ));
        let notices: Vec<&BubbleRow> = s.turns[0].thoughts.iter().collect();
        assert!(matches!(notices[0], BubbleRow::Notice(t) if t.contains("Paused")));
        assert!(matches!(notices[1], BubbleRow::Notice(t) if t.contains("IdenticalAction")));
    }

    #[test]
    fn parse_outcome_handles_variants() {
        assert_eq!(
            parse_outcome(&json!({ "outcome": "Progress", "reason": null })).map(|(c, _)| c),
            Some(OutcomeClass::Progress)
        );
        let (c, r) = parse_outcome(&json!({
            "outcome": { "Failed": "UserCancelled" }, "reason": "cancelled by user"
        }))
        .unwrap();
        assert_eq!(c, OutcomeClass::Failed);
        assert_eq!(r.as_deref(), Some("cancelled by user"));
        assert_eq!(parse_outcome(&json!({ "outcome": "Nope" })), None);
    }

    #[test]
    fn tokens_sums_egress_across_turns() {
        let mut s = ConversationProjection::new();
        s.apply(&env(1, "user_message", json!({ "text": "a" })));
        s.apply(&env(2, "model_outcome", model_outcome(Some("x"), &[], 10, 5)));
        s.finalize_turn(Some(OutcomeClass::Progress));
        s.apply(&env(3, "user_message", json!({ "text": "b" })));
        s.apply(&env(4, "model_outcome", model_outcome(Some("y"), &[], 7, 2)));
        s.finalize_turn(Some(OutcomeClass::Progress));
        assert_eq!(default_view(&s).tokens(), (17, 7));
    }

    // ---- decision 30 acceptance ----

    #[test]
    fn two_projections_of_the_same_stream_produce_equal_views() {
        let stream = [
            env(1, "user_message", json!({ "text": "hi" })),
            env(2, "run_start", json!({})),
            env(
                3,
                "tool_intent",
                json!({ "call_id": "c1", "tool": "fs.read", "args": { "path": "a" } }),
            ),
            env(
                4,
                "tool_outcome",
                json!({ "call_id": "c1", "tool": "fs.read", "result": "x", "error": null,
                        "classification": "Normal" }),
            ),
            env(5, "model_outcome", model_outcome(Some("done"), &[], 3, 4)),
        ];
        let mut a = ConversationProjection::new();
        let mut b = ConversationProjection::new();
        for env in &stream {
            a.apply(env);
            b.apply(env);
        }
        a.finalize_turn(None);
        b.finalize_turn(None);
        assert_eq!(default_view(&a), default_view(&b));
    }

    #[test]
    fn replay_is_identical_to_live_application() {
        let stream = [
            env(1, "user_message", json!({ "text": "hi" })),
            env(2, "run_start", json!({})),
            env(3, "model_outcome", model_outcome(Some("thinking"), &["fs.read"], 1, 2)),
            env(
                4,
                "tool_intent",
                json!({ "call_id": "c1", "tool": "fs.read", "args": { "path": "a" } }),
            ),
            env(
                5,
                "tool_outcome",
                json!({ "call_id": "c1", "tool": "fs.read", "result": "x", "error": null,
                        "classification": "Normal" }),
            ),
            env(6, "run_outcome", json!({ "run_id": "r", "outcome": "Blocked", "reason": null })),
        ];
        let mut live = ConversationProjection::new();
        for env in &stream {
            live.apply(env);
        }
        live.finalize_turn(None);

        let mut replay = ConversationProjection::new();
        for env in &stream {
            replay.apply(env);
        }
        replay.finish_replay();

        assert_eq!(default_view(&live), default_view(&replay));
    }

    #[test]
    fn collapse_defaults_are_pure_and_overrides_stay_session_local() {
        let mut s = ConversationProjection::new();
        s.apply(&env(1, "user_message", json!({ "text": "a" })));
        s.apply(&env(2, "run_start", json!({})));
        s.apply(&env(3, "run_outcome", json!({ "run_id": "r", "outcome": "Progress", "reason": null })));
        s.finalize_turn(None);
        s.apply(&env(4, "user_message", json!({ "text": "b" })));

        let baseline = default_view(&s);
        assert!(!baseline.turns[0].open, "a settled turn collapses by default");
        assert!(baseline.turns[1].open, "a running turn is always open");

        let mut overrides = CollapseOverrides::new();
        overrides.toggle(0);
        let expanded = s.view(&overrides);
        assert!(expanded.turns[0].open, "the override re-opens the settled turn");
        assert!(expanded.turns[1].open);

        // The override never mutated the projection: the default view is
        // byte-identical afterwards.
        assert_eq!(s.view(&CollapseOverrides::new()), baseline);
    }

    #[test]
    fn streaming_deltas_are_an_in_flight_partial_and_end_stream_clears_it() {
        let mut s = ConversationProjection::new();
        s.apply(&env(1, "user_message", json!({ "text": "hi" })));
        assert_eq!(default_view(&s).turns[0].streaming, None);
        s.apply_delta("he");
        s.apply_delta("llo");
        let view = default_view(&s);
        assert_eq!(view.turns[0].streaming.as_deref(), Some("hello"));
        assert!(view.turns[0].open, "the in-flight partial renders in an open bubble");
        s.end_stream();
        assert_eq!(default_view(&s).turns[0].streaming, None);
    }

    /// A partial must not survive on an earlier Running turn when a later turn
    /// is the active one at stream end (replay can hold several, since
    /// finalize is not a log event).
    #[test]
    fn end_stream_clears_partials_on_every_running_turn() {
        let mut s = ConversationProjection::new();
        s.apply(&env(1, "user_message", json!({ "text": "a" })));
        s.apply(&env(2, "run_start", json!({})));
        s.apply_delta("partial");
        s.apply(&env(3, "user_message", json!({ "text": "b" })));
        s.end_stream();
        assert_eq!(s.turns[0].streaming, None);
        assert_eq!(s.turns[1].streaming, None);
    }
}
