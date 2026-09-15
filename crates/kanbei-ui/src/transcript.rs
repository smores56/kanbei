//! Presentation over the typed transcript view (decision 30): `kanbei-ui`
//! owns rendering, `kanbei-transcript` owns projection. [`render_transcript`]
//! composes the T10 `SemanticTree` primitives; [`transcript_rows`] flattens
//! the same rows for the hand-rolled TUI (document order, per-row turn
//! attribution).
//!
//! Collapse is recomputation, not state: the view carries default openness
//! (running turns open, settled turns collapsed) and the session-local
//! [`CollapseOverrides`] re-open settled turns per render.

use crate::tree::{Node, SemanticTree};
use kanbei_transcript::{
    BubbleRow, CollapseOverrides, StepStatus, ToolStep, TranscriptView, TurnState, TurnView,
};

/// One flat transcript row for the TUI (document order): text, theme style
/// name, and the turn index it renders (the toggle identity for click/
/// keyboard selection). Mirrors [`render_transcript`] so the module renderer
/// and the TUI stay in lockstep.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscriptRow {
    pub text: String,
    pub style: String,
    pub turn: usize,
}

/// Whether a turn's thought bubble is open. `view()` already folds the
/// running default into `turn.open`, but callers may render a view built with
/// different overrides (the CLI's listener view uses none), so the live
/// overrides are still OR'd on top.
fn is_open(turn: &TurnView, n: usize, overrides: &CollapseOverrides) -> bool {
    turn.open || overrides.contains(n)
}

/// The transcript as a semantic tree (the module-facing contract). A running
/// turn is always expanded (Q5: live steps + spinner); a settled turn
/// collapses unless re-opened (Q6/Q5).
pub fn render_transcript(view: &TranscriptView, overrides: &CollapseOverrides) -> SemanticTree {
    // The former semantic rows are primitive compositions: a user message
    // or answer is a styled `text`, a working step is a `code` line, the
    // live spinner is a styled `text`, and the turn toggle is a `button`.
    let mut root = Node::stack("root").child(Node::col("conv"));
    let rows = &mut root.children[0];
    for (n, turn) in view.turns.iter().enumerate() {
        let id = |s: &str| format!("{s}{n}");
        rows.children.push(Node::styled_text(
            id("u"),
            format!("❯ {}", turn.user),
            "user",
        ));
        let open = is_open(turn, n, overrides);
        if open && !turn.thoughts.is_empty() {
            for (k, row) in turn.thoughts.iter().enumerate() {
                match row {
                    BubbleRow::Text(text) => {
                        rows.children.push(Node::styled_text(
                            id(&format!("b{k}")),
                            indent(text, 2),
                            "thought",
                        ));
                    }
                    BubbleRow::Step(step) => {
                        rows.children
                            .push(Node::code(id(&format!("b{k}")), step_line(step)));
                    }
                    BubbleRow::Notice(text) => {
                        rows.children.push(Node::styled_text(
                            id(&format!("b{k}")),
                            indent(text, 2),
                            "status",
                        ));
                    }
                }
            }
        }
        if open {
            if let Some(text) = &turn.streaming {
                rows.children
                    .push(Node::styled_text(id("s"), indent(text, 2), "thought"));
            }
            if turn.state == TurnState::Running {
                rows.children
                    .push(Node::styled_text(id("p"), "  … working", "progress"));
            }
        }
        if turn.state != TurnState::Running {
            let marker = if open { "▾" } else { "▸" };
            rows.children.push(Node::button(
                id("t"),
                format!("{marker} {}", turn_summary(turn)),
            ));
        }
        if let Some(answer) = &turn.response {
            rows.children
                .push(Node::styled_text(id("r"), indent(answer, 1), "response"));
        }
        rows.children
            .push(Node::styled_text(id("d"), "─".repeat(2), "divider"));
    }
    SemanticTree::new(root)
}

/// The flat TUI transcript (document order) with per-row turn attribution.
/// Thought segments render only while the turn is running or its bubble is
/// expanded (R-02/C-03). Mirrors [`render_transcript`].
pub fn transcript_rows(view: &TranscriptView, overrides: &CollapseOverrides) -> Vec<TranscriptRow> {
    let mut rows = Vec::new();
    for (n, turn) in view.turns.iter().enumerate() {
        rows.push(TranscriptRow {
            text: format!("❯ {}", turn.user),
            style: "user".into(),
            turn: n,
        });
        let open = is_open(turn, n, overrides);
        if open && !turn.thoughts.is_empty() {
            for row in turn.thoughts.iter() {
                let (text, style) = match row {
                    BubbleRow::Text(t) => (indent(t, 2), "thought"),
                    BubbleRow::Step(s) => (step_line(s), "tool"),
                    BubbleRow::Notice(t) => (indent(t, 2), "status"),
                };
                rows.push(TranscriptRow {
                    text,
                    style: style.into(),
                    turn: n,
                });
            }
        }
        if open {
            if let Some(text) = &turn.streaming {
                rows.push(TranscriptRow {
                    text: indent(text, 2),
                    style: "thought".into(),
                    turn: n,
                });
            }
            if turn.state == TurnState::Running {
                rows.push(TranscriptRow {
                    text: "  … working".into(),
                    style: "progress".into(),
                    turn: n,
                });
            }
        }
        if turn.state != TurnState::Running {
            let marker = if open { "▾" } else { "▸" };
            rows.push(TranscriptRow {
                text: format!("{marker} {}", turn_summary(turn)),
                style: "thought".into(),
                turn: n,
            });
        }
        if let Some(answer) = &turn.response {
            rows.push(TranscriptRow {
                text: indent(answer, 1),
                style: "response".into(),
                turn: n,
            });
        }
        rows.push(TranscriptRow {
            text: "──".into(),
            style: "divider".into(),
            turn: n,
        });
    }
    rows
}

fn step_line(step: &ToolStep) -> String {
    let mut out = format!(
        "  {} {}({})",
        step_status_label(step.status),
        step.tool,
        truncate(&step.args, 120)
    );
    let detail = step_detail(step);
    if !detail.is_empty() {
        out.push_str(&format!(" — {}", truncate(&detail, 160)));
    }
    out
}

/// The step's outcome detail: classification reason, error text, and
/// serialized result, joined for display. The projection carries each raw and
/// untruncated; the renderer owns the ` · ` join and the truncation.
fn step_detail(step: &ToolStep) -> String {
    let result = truncate(&step.result, 200);
    [step.detail.as_str(), step.error.as_str(), result.as_str()]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" · ")
}

/// The collapsed summary line (Q5): state · steps · runs · tokens; a
/// non-clean end appends the responsible reason.
fn turn_summary(turn: &TurnView) -> String {
    let mut out = format!(
        "[{}] {} step(s), {} run(s), {}+{} tok",
        state_symbol(turn.state),
        turn.tools,
        turn.runs,
        turn.input_tokens,
        turn.output_tokens
    );
    if turn.state != TurnState::Completed
        && let Some(reason) = &turn.reason
    {
        out.push_str(&format!(" — {reason}"));
    }
    out
}

fn state_symbol(state: TurnState) -> &'static str {
    match state {
        TurnState::Running => "…",
        TurnState::Completed => "✓",
        TurnState::Failed => "✗",
        TurnState::Blocked => "!",
        TurnState::Interrupted => "?",
    }
}

fn step_status_label(status: StepStatus) -> &'static str {
    match status {
        StepStatus::InFlight => "…",
        StepStatus::Ok => "✓",
        StepStatus::Interrupted => "✗",
        StepStatus::Ambiguous => "?",
    }
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
    use kanbei_transcript::{ConversationProjection, TranscriptProjection};
    use serde_json::{Value, json};

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
    fn tree_renders_user_bubble_summary_and_response() {
        let mut s = ConversationProjection::new();
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
        let t = render_transcript(&s.view(&CollapseOverrides::new()), &CollapseOverrides::new());
        let kinds: Vec<crate::NodeKind> = t.nodes().iter().map(|n| n.kind()).collect();
        assert!(kinds.contains(&crate::NodeKind::Code), "a tool step is code");
        let text: Vec<String> = t.nodes().iter().map(|n| n.content()).collect();
        assert!(text.iter().any(|c| c.contains("… working")), "spinner is a text row");
        assert!(t.focusable().is_empty(), "no turn toggle while running");
        s.apply(&env(5, "model_outcome", model_outcome(Some("done"), &[], 3, 4)));
        s.finalize_turn(None);
        // collapsed: no step row, summary + response present
        let t = render_transcript(&s.view(&CollapseOverrides::new()), &CollapseOverrides::new());
        let text: Vec<String> = t.nodes().iter().map(|n| n.content()).collect();
        assert!(!text.iter().any(|c| c.contains("fs.read")));
        assert!(text.iter().any(|c| c.starts_with("▸")));
        assert!(text.iter().any(|c| c.contains("done")));
        // the collapsed summary is a focusable button
        assert_eq!(t.focusable().len(), 1);
        assert_eq!(t.focusable()[0].kind(), crate::NodeKind::Button);
        // expanded: the step row comes back
        let mut overrides = CollapseOverrides::new();
        overrides.toggle(0);
        let t = render_transcript(&s.view(&overrides), &overrides);
        let text: Vec<String> = t.nodes().iter().map(|n| n.content()).collect();
        assert!(text.iter().any(|c| c.contains("fs.read")));
        assert!(text.iter().any(|c| c.starts_with("▾")));
    }

    #[test]
    fn transcript_projects_document_order_and_toggle_state() {
        let mut s = ConversationProjection::new();
        s.apply(&env(1, "user_message", json!({ "text": "hi" })));
        s.apply(&env(2, "run_start", json!({})));
        s.apply(&env(
            3,
            "tool_intent",
            json!({ "call_id": "c1", "tool": "fs.read", "args": { "path": "a" } }),
        ));
        // running: bubble open (step + spinner), no summary marker yet
        let rows = transcript_rows(&s.view(&CollapseOverrides::new()), &CollapseOverrides::new());
        assert_eq!(rows[0].text, "❯ hi");
        assert_eq!(rows[0].style, "user");
        assert!(rows.iter().any(|r| r.text.contains("fs.read") && r.style == "tool"));
        assert!(rows
            .iter()
            .any(|r| r.text == "  … working" && r.style == "progress"));
        assert!(!rows.iter().any(|r| r.text.starts_with("▸")));

        s.apply(&env(
            4,
            "tool_outcome",
            json!({ "call_id": "c1", "tool": "fs.read", "result": "x", "error": null,
                    "classification": "Normal" }),
        ));
        s.apply(&env(5, "model_outcome", model_outcome(Some("done"), &[], 3, 4)));
        s.finalize_turn(Some(kanbei_transcript::OutcomeClass::Progress));

        // collapsed: summary marker, no step rows, indented response, divider
        let rows = transcript_rows(&s.view(&CollapseOverrides::new()), &CollapseOverrides::new());
        assert!(rows.iter().any(|r| r.text.starts_with("▸ [✓]")));
        assert!(!rows.iter().any(|r| r.text.contains("fs.read")));
        // `indent` prefixes only continuation lines; a single-line answer
        // renders bare.
        assert!(rows
            .iter()
            .any(|r| r.text == "done" && r.style == "response"));
        assert_eq!(rows.last().unwrap().text, "──");
        assert_eq!(
            rows.iter().map(|r| r.turn).collect::<Vec<_>>(),
            vec![0; rows.len()]
        );

        // expanded: steps come back, marker flips, spinner only while running
        let mut overrides = CollapseOverrides::new();
        overrides.toggle(0);
        let rows = transcript_rows(&s.view(&overrides), &overrides);
        assert!(rows.iter().any(|r| r.text.starts_with("▾ [✓]")));
        assert!(rows.iter().any(|r| r.text.contains("fs.read") && r.style == "tool"));
        assert!(!rows.iter().any(|r| r.text.contains("… working")));
    }
}
