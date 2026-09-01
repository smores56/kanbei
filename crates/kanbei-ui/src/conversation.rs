//! The conversation transcript: a pure projection of committed envelopes
//! into the `SemanticTree` contract (R-19: message identity is its
//! committing event; launch is always resume). The machine is a total
//! function of the envelope stream it is fed: applying the same envelopes
//! (a fresh session replays the full log on open) yields the same
//! transcript. The turn's END is a driver-level fact (the driver stopped
//! driving) — the worker's `finalize_turn` records it, mirroring the
//! terminal `run_outcome` the session committed.
//!
//! Typing (structural, doc-faithful): the response is the turn-terminal
//! `model_outcome` (content without pending tool calls); thoughts are
//! intermediate `model_outcome` content plus tool steps; the turn end-state
//! comes from `run_outcome`. Opaque artifacts never enter the projection
//! (M6/S9) and are not rendered.

use std::collections::HashSet;
use std::time::{Duration, Instant};

use serde_json::Value;

use kanbei_core::envelope::Envelope;

use crate::tree::{Node, NodeKind, SemanticTree};

/// The turn's terminal classification (UI vocabulary for the scheduler's
/// `TerminalOutcome`, kept dependency-free).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutcomeClass {
    Progress,
    CompletedGoal,
    NoProgress,
    Waiting,
    Blocked,
    Failed,
}

impl OutcomeClass {
    pub fn as_str(self) -> &'static str {
        match self {
            OutcomeClass::Progress => "progress",
            OutcomeClass::CompletedGoal => "completed-goal",
            OutcomeClass::NoProgress => "no-progress",
            OutcomeClass::Waiting => "waiting",
            OutcomeClass::Blocked => "blocked",
            OutcomeClass::Failed => "failed",
        }
    }
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
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepStatus {
    /// The `tool_intent` committed; no outcome yet.
    InFlight,
    Ok,
    /// `Interrupted(reason)` — denied, stale, or approval-denied.
    Interrupted,
    /// `Ambiguous(reason)` — outcome of possibly-dispatched work.
    Ambiguous,
}

impl StepStatus {
    pub fn label(self) -> &'static str {
        match self {
            StepStatus::InFlight => "…",
            StepStatus::Ok => "✓",
            StepStatus::Interrupted => "✗",
            StepStatus::Ambiguous => "?",
        }
    }
}

/// One row of a turn's working segment (thought bubble).
#[derive(Debug, Clone)]
pub enum BubbleRow {
    /// Intermediate model content (a thought).
    Text(String),
    /// A tool step (call + outcome, paired by call_id).
    Step(ToolStep),
    /// A kernel notice (wake denied, breaker trip, resume) — dimmed.
    Notice(String),
}

#[derive(Debug, Clone)]
pub struct ToolStep {
    pub call_id: String,
    pub tool: String,
    /// Canonical argument JSON (display; truncated at render).
    pub args: String,
    pub status: StepStatus,
    /// Outcome detail: error text, denial reason, or truncated output.
    pub detail: String,
}

/// The turn's rendered end-state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    pub fn symbol(self) -> &'static str {
        match self {
            TurnState::Running => "…",
            TurnState::Completed => "✓",
            TurnState::Failed => "✗",
            TurnState::Blocked => "!",
            TurnState::Interrupted => "?",
        }
    }

    pub fn from_outcome(last: Option<(OutcomeClass, Option<String>)>) -> Self {
        match last {
            Some((OutcomeClass::Progress | OutcomeClass::CompletedGoal, _)) => {
                TurnState::Completed
            }
            Some((OutcomeClass::Failed, _)) => TurnState::Failed,
            _ => TurnState::Blocked,
        }
    }
}

/// One user turn: the message, its working segment, the final answer, and
/// the recorded terminal state.
#[derive(Debug, Clone)]
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
    /// The last run's terminal outcome as recorded (replay safety: a
    /// replayed turn whose driver result is unknown resolves its state from
    /// this).
    pub last_outcome: Option<(OutcomeClass, Option<String>)>,
    /// Rendering metadata (wall clock), set by the UI for live turns;
    /// `None` for replayed history. Not canonical.
    pub started_at: Option<Instant>,
    pub ended_at: Option<Instant>,
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
            started_at: None,
            ended_at: None,
        }
    }

    pub fn elapsed(&self) -> Option<Duration> {
        match (self.started_at, self.ended_at) {
            (Some(s), Some(e)) => e.checked_duration_since(s),
            _ => None,
        }
    }

    /// The collapsed summary line (Q5): state · steps · runs · tokens; a
    /// non-clean end appends the responsible reason.
    pub fn summary(&self) -> String {
        let mut out = format!(
            "[{}] {} step(s), {} run(s), {}+{} tok",
            self.state.symbol(),
            self.tools,
            self.runs,
            self.input_tokens,
            self.output_tokens
        );
        if let Some(elapsed) = self.elapsed() {
            out.push_str(&format!(" in {:.1}s", elapsed.as_secs_f64()));
        }
        if !matches!(self.state, TurnState::Completed)
            && let Some(reason) = &self.reason
        {
            out.push_str(&format!(" — {reason}"));
        }
        out
    }
}

/// The whole transcript: turns in commit order. A pure function of the
/// envelope stream applied so far (plus finalize events, which mirror
/// committed terminal records).
#[derive(Debug, Clone, Default)]
pub struct ConversationState {
    pub turns: Vec<TurnView>,
}

impl ConversationState {
    pub fn new() -> Self {
        Self::default()
    }

    fn active(&self) -> Option<usize> {
        self.turns
            .iter()
            .rposition(|t| t.state == TurnState::Running)
    }

    /// Apply one committed envelope (the only mutation entry point besides
    /// [`Self::finalize_turn`]/[`Self::finish_replay`]). Unknown kinds are
    /// kernel records the transcript does not surface (M6/S9: opaque
    /// artifacts never enter the projection).
    pub fn apply(&mut self, env: &Envelope) {
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
                    self.turns[i].runs += 1;
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
                let reason = match o.get("Interrupted").or_else(|| o.get("Ambiguous")) {
                    Some(Value::String(s)) => s.clone(),
                    _ => String::new(),
                };
                (
                    if o.contains_key("Interrupted") {
                        StepStatus::Interrupted
                    } else {
                        StepStatus::Ambiguous
                    },
                    reason,
                )
            }
            _ => (StepStatus::Ok, String::new()),
        };
        let error = payload.get("error").and_then(Value::as_str).unwrap_or_default();
        let result_text = truncate(
            &payload
                .get("result")
                .filter(|v| !v.is_null())
                .map(Value::to_string)
                .unwrap_or_default(),
            200,
        );
        let detail_parts: Vec<&str> = [&detail, error, &result_text]
            .into_iter()
            .filter(|s| !s.is_empty())
            .collect();
        step.status = status;
        step.detail = detail_parts.join(" · ");
    }

    /// Close the active turn with the driver's observed result (the worker
    /// stopped driving; the terminal `run_outcome` is already in the log,
    /// so this mirrors a committed fact). `None` = no run reached a
    /// terminal outcome (wake denied, error before any run).
    pub fn finalize_turn(&mut self, last: Option<(OutcomeClass, Option<String>)>) {
        let Some(i) = self.active() else {
            return;
        };
        let turn = &mut self.turns[i];
        // The driver's result wins; a `None` result (no run reached a
        // terminal outcome) falls back to the recorded `run_outcome`.
        let effective = last.or(turn.last_outcome.clone());
        turn.state = TurnState::from_outcome(effective);
        turn.ended_at = Some(Instant::now());
    }

    /// End of a replay (session open / resume): a turn still active has no
    /// driver result — resolve it from its recorded terminal outcome, else
    /// mark it interrupted (B-05: the log is the authority).
    pub fn finish_replay(&mut self) {
        if let Some(i) = self.active() {
            let turn = &mut self.turns[i];
            turn.state = TurnState::from_outcome(turn.last_outcome.clone());
        }
    }

    /// The transcript as a semantic tree (the module-facing contract).
    /// `expanded` names the collapsed turns the user re-opened (Q6/Q5:
    /// bubbles collapse on completion, expand on demand); a running turn is
    /// always expanded (Q5: live steps + spinner).
    pub fn tree(&self, expanded: &HashSet<String>) -> SemanticTree {
        let mut root = Node::new("root", NodeKind::Root)
            .child(Node::new("conv", NodeKind::List));
        let list = &mut root.children[0];
        for (n, turn) in self.turns.iter().enumerate() {
            let id = |s: &str| format!("{s}{n}");
            list.children.push(
                Node::new(id("u"), NodeKind::User)
                    .with_content(format!("❯ {}", turn.user)),
            );
            let open = turn.state == TurnState::Running
                || expanded.contains(&format!("t{n}"));
            if open && !turn.thoughts.is_empty() {
                for (k, row) in turn.thoughts.iter().enumerate() {
                    match row {
                        BubbleRow::Text(text) => {
                            list.children.push(
                                Node::new(id(&format!("b{k}")), NodeKind::Thought)
                                    .with_content(indent(text, 2)),
                            );
                        }
                        BubbleRow::Step(step) => {
                            list.children.push(
                                Node::new(id(&format!("b{k}")), NodeKind::Code)
                                    .with_content(step_line(step)),
                            );
                        }
                        BubbleRow::Notice(text) => {
                            list.children.push(
                                Node::new(id(&format!("b{k}")), NodeKind::Thought)
                                    .with_content(indent(text, 2))
                                    .with_style("status"),
                            );
                        }
                    }
                }
                if turn.state == TurnState::Running {
                    list.children.push(
                        Node::new(id("p"), NodeKind::Progress)
                            .with_content("  … working"),
                    );
                }
            }
            if turn.state != TurnState::Running {
                let marker = if open { "▾" } else { "▸" };
                list.children.push(
                    Node::new(id("t"), NodeKind::Group)
                        .with_content(format!("{marker} {}", turn.summary()))
                        .focusable(),
                );
            }
            if let Some(answer) = &turn.response {
                list.children.push(
                    Node::new(id("r"), NodeKind::Response)
                        .with_content(indent(answer, 1)),
                );
            }
            list.children.push(
                Node::new(id("d"), NodeKind::Divider)
                    .with_content("─".repeat(2)),
            );
        }
        SemanticTree::new(root)
    }
}

fn step_line(step: &ToolStep) -> String {
    let mut out = format!("  {} {}({})", step.status.label(), step.tool, truncate(&step.args, 120));
    if !step.detail.is_empty() {
        out.push_str(&format!(" — {}", truncate(&step.detail, 160)));
    }
    out
}

fn truncate(s: &str, max: usize) -> String {
    let chars: Vec<char> = s.chars().collect();
    if chars.len() <= max {
        return s.to_string();
    }
    let mut out: String = chars[..max].iter().collect();
    out.push('…');
    out
}

fn indent(text: &str, spaces: usize) -> String {
    let pad = " ".repeat(spaces);
    text.replace('\n', &format!("\n{pad}"))
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

    #[test]
    fn user_message_opens_a_turn() {
        let mut s = ConversationState::new();
        s.apply(&env(1, "user_message", json!({ "text": "hello" })));
        assert_eq!(s.turns.len(), 1);
        assert_eq!(s.turns[0].user, "hello");
        assert_eq!(s.turns[0].state, TurnState::Running);
    }

    #[test]
    fn terminal_outcome_without_calls_is_the_response() {
        let mut s = ConversationState::new();
        s.apply(&env(1, "user_message", json!({ "text": "hi" })));
        s.apply(&env(2, "model_outcome", model_outcome(Some("the answer"), &[], 10, 5)));
        assert_eq!(s.turns[0].response.as_deref(), Some("the answer"));
        assert!(s.turns[0].thoughts.is_empty());
        assert_eq!(s.turns[0].input_tokens, 10);
        assert_eq!(s.turns[0].output_tokens, 5);
    }

    #[test]
    fn intermediate_outcome_with_calls_is_thought() {
        let mut s = ConversationState::new();
        s.apply(&env(1, "user_message", json!({ "text": "hi" })));
        s.apply(&env(2, "model_outcome", model_outcome(Some("let me check"), &["fs.read"], 1, 2)));
        assert_eq!(s.turns[0].response, None);
        assert!(matches!(s.turns[0].thoughts[0], BubbleRow::Text(ref t) if t == "let me check"));
    }

    #[test]
    fn tool_only_outcome_has_no_phantom_text() {
        let mut s = ConversationState::new();
        s.apply(&env(1, "user_message", json!({ "text": "hi" })));
        s.apply(&env(2, "model_outcome", model_outcome(None, &["fs.read"], 1, 2)));
        assert!(s.turns[0].thoughts.is_empty());
        assert_eq!(s.turns[0].response, None);
    }

    #[test]
    fn tool_intent_and_outcome_pair_by_call_id() {
        let mut s = ConversationState::new();
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
        let mut s = ConversationState::new();
        s.apply(&env(1, "user_message", json!({ "text": "hi" })));
        s.apply(&env(2, "run_start", json!({})));
        assert_eq!(s.turns[0].runs, 1);
        s.apply(&env(3, "run_outcome", json!({
            "run_id": "r", "outcome": "Progress", "reason": null
        })));
        s.finalize_turn(Some((OutcomeClass::Progress, None)));
        assert_eq!(s.turns[0].state, TurnState::Completed);
        assert!(s.active().is_none());
    }

    #[test]
    fn failed_run_carries_the_reason() {
        let mut s = ConversationState::new();
        s.apply(&env(1, "user_message", json!({ "text": "hi" })));
        s.apply(&env(2, "run_outcome", json!({
            "run_id": "r",
            "outcome": { "Failed": "Provider" },
            "reason": "provider 500"
        })));
        s.finalize_turn(None);
        assert_eq!(s.turns[0].state, TurnState::Failed);
        assert_eq!(s.turns[0].reason.as_deref(), Some("provider 500"));
        assert!(s.turns[0].summary().contains("provider 500"));
    }

    #[test]
    fn replay_finish_resolves_a_leftover_active_turn() {
        let mut s = ConversationState::new();
        s.apply(&env(1, "user_message", json!({ "text": "hi" })));
        s.apply(&env(2, "run_outcome", json!({
            "run_id": "r", "outcome": "Blocked", "reason": null
        })));
        s.finish_replay();
        assert_eq!(s.turns[0].state, TurnState::Blocked);
    }

    #[test]
    fn wake_denial_and_breaker_become_notices() {
        let mut s = ConversationState::new();
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
    fn tree_renders_user_bubble_summary_and_response() {
        let mut s = ConversationState::new();
        s.apply(&env(1, "user_message", json!({ "text": "hello" })));
        s.apply(&env(2, "run_start", json!({})));
        s.apply(&env(
            3,
            "tool_intent",
            json!({ "call_id": "c1", "tool": "fs.read", "args": { "path": "a" } }),
        ));
        s.apply(&env(
            4,
            "tool_outcome",
            json!({ "call_id": "c1", "tool": "fs.read", "result": "x", "error": null,
                    "classification": "Normal" }),
        ));
        // running: the bubble is open with the step row + spinner
        let t = s.tree(&HashSet::new());
        let kinds: Vec<NodeKind> = t.nodes().iter().map(|n| n.kind).collect();
        assert!(kinds.contains(&NodeKind::Code));
        assert!(kinds.contains(&NodeKind::Progress));
        s.apply(&env(5, "model_outcome", model_outcome(Some("done"), &[], 3, 4)));
        s.finalize_turn(Some((OutcomeClass::Progress, None)));
        // collapsed: no step row, summary + response present
        let t = s.tree(&HashSet::new());
        let content: Vec<String> = t.nodes().iter().map(|n| n.content.clone()).collect();
        assert!(!content.iter().any(|c| c.contains("fs.read")));
        assert!(content.iter().any(|c| c.starts_with("▸")));
        assert!(content.iter().any(|c| c.contains("done")));
        // expanded: the step row comes back
        let t = s.tree(&HashSet::from([format!("t{}", 0)]));
        let content: Vec<String> = t.nodes().iter().map(|n| n.content.clone()).collect();
        assert!(content.iter().any(|c| c.contains("fs.read")));
        assert!(content.iter().any(|c| c.starts_with("▾")));
    }
}
