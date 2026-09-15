//! Kernel-owned rendering: `SemanticTree + Theme -> TerminalFrame`
//! (architecture.md UI model). The layout is deterministic and module-free:
//! banner/header rows on top, body in the middle, kernel status bar and the
//! focused input line at the bottom. Luau/Wasm never draws cells (R-27,
//! consistency 13).

use crate::focus::FocusModel;
use crate::theme::{DEFAULT_STYLE, Theme};
use crate::tree::{Node, NodeKind, SemanticTree};

/// Minimum terminal rows for a usable frame: banner/header + body + status +
/// input.
pub const MIN_ROWS: usize = 4;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cell {
    pub ch: char,
    pub style: String,
}

impl Cell {
    pub fn blank() -> Self {
        Cell {
            ch: ' ',
            style: DEFAULT_STYLE.to_string(),
        }
    }

    pub fn is_blank(&self) -> bool {
        self.ch == ' ' && self.style == DEFAULT_STYLE
    }
}

/// A full snapshot of the terminal surface (immutable; hot paths consume
/// these, consistency 13).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalFrame {
    pub rows: u16,
    pub cols: u16,
    pub cells: Vec<Cell>,
}

impl TerminalFrame {
    pub fn blank(rows: u16, cols: u16) -> Self {
        TerminalFrame {
            rows,
            cols,
            cells: vec![Cell::blank(); rows as usize * cols as usize],
        }
    }

    pub fn cell(&self, row: u16, col: u16) -> &Cell {
        &self.cells[row as usize * self.cols as usize + col as usize]
    }

    pub fn set(&mut self, row: u16, col: u16, ch: char, style: &str) {
        let idx = row as usize * self.cols as usize + col as usize;
        self.cells[idx] = Cell {
            ch,
            style: style.to_string(),
        };
    }

    /// The visible text of one row (test helper).
    pub fn row_text(&self, row: u16) -> String {
        (0..self.cols)
            .map(|c| self.cell(row, c).ch)
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    pub fn write_line(&mut self, row: u16, text: &str, style: &str, focused: bool) {
        let style = if focused { "selected" } else { style };
        let cols = self.cols as usize;
        for (i, ch) in text.chars().take(cols).enumerate() {
            let ch = if ch.is_control() { ' ' } else { ch };
            self.set(row, i as u16, ch, style);
        }
    }

    /// Write one pre-wrapped row of per-character styles; a focused row is
    /// drawn uniformly in reverse video (the kernel's focus highlight).
    pub fn write_chars(&mut self, row: u16, chars: &[(char, String)], focused: bool) {
        let cols = self.cols as usize;
        for (i, (ch, style)) in chars.iter().take(cols).enumerate() {
            let ch = if ch.is_control() { ' ' } else { *ch };
            let style = if focused { "selected" } else { style.as_str() };
            self.set(row, i as u16, ch, style);
        }
    }
}

/// Everything the renderer needs. Status/staleness/degraded are kernel-owned
/// overlays; the tree and focus come from the module-facing side.
pub struct RenderContext<'a> {
    pub tree: &'a SemanticTree,
    pub theme: &'a Theme,
    pub focus: &'a FocusModel,
    /// Terminal size in (rows, cols).
    pub size: (u16, u16),
    /// Kernel status text (e.g. run state).
    pub status: &'a str,
    pub staleness: Option<&'a str>,
    pub degraded: bool,
}

/// The rendered frame plus the viewport top the renderer actually used
/// (focus-follow may move it; the caller stores it back into the focus
/// model).
pub struct RenderOutput {
    pub frame: TerminalFrame,
    pub viewport_top: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum RenderError {
    #[error("terminal too small for the workbench layout: {rows} rows (need >= {MIN_ROWS})")]
    TooSmall { rows: u16 },
}

/// One body line: a node plus its pre-styled segments. The segments carry
/// per-span theme styles, so a `text` node's spans survive into cells.
pub struct BodyLine<'a> {
    pub node: &'a Node,
    pub spans: Vec<(String, String)>,
}

/// Render the tree into cells. Layout (top to bottom):
/// 1. staleness banner (when present), then the first header node;
/// 2. body: depth-first lines (list items, text wrapped to the width, status
///    and button nodes); scrolled so the focused node stays visible;
/// 3. kernel status bar;
/// 4. the input line: `> ` + focused input content with the caret drawn in
///    reverse video.
pub fn render(ctx: &RenderContext) -> Result<RenderOutput, RenderError> {
    let (rows, cols) = ctx.size;
    let rows = rows as usize;
    let cols = cols as usize;
    if rows < MIN_ROWS {
        return Err(RenderError::TooSmall { rows: rows as u16 });
    }
    let mut frame = TerminalFrame::blank(ctx.size.0, ctx.size.1);

    // 1. banner row. There is no header kind: a module composes its title as
    // the first `text` row of the body, so titled workbenches occupy the same
    // top row they did when the kernel special-cased headers.
    if let Some(reason) = ctx.staleness {
        frame.write_line(0, &crate::fallback::staleness_text(reason), "banner", false);
    }
    let body_start = if ctx.staleness.is_some() { 1 } else { 0 };

    // 2. body lines (input nodes are kernel-rendered on the bottom row).
    let mut lines: Vec<BodyLine> = Vec::new();
    collect_lines(&ctx.tree.root, &mut lines);

    // 3. input node selection for the bottom row.
    let input_node = ctx
        .tree
        .input_node(ctx.focus.focused.as_deref())
        .cloned();

    // 4. status bar text.
    let mut status = ctx.status.to_string();
    if ctx.degraded {
        status.push_str(" [degraded]");
    }
    if ctx.staleness.is_some() {
        status.push_str(" [stale]");
    }

    // Viewport: keep the focused line visible; tail when unfocused.
    let body_rows = rows.saturating_sub(body_start + 2); // status + input rows
    let focused_idx = ctx
        .focus
        .focused
        .as_deref()
        .and_then(|id| lines.iter().position(|l| l.node.id == id));
    let max_top = lines.len().saturating_sub(body_rows);
    let top = match focused_idx {
        Some(f) => f.min(max_top),
        None => max_top,
    };
    let mut row = body_start;
    for line in lines.iter().skip(top) {
        let focused = Some(line.node.id.as_str()) == ctx.focus.focused.as_deref();
        let segs = wrap_spans(&line.spans, cols);
        for (seg_row, chars) in segs.iter().enumerate() {
            let r = row + seg_row;
            if r >= body_start + body_rows {
                break;
            }
            frame.write_chars(r as u16, chars, focused);
        }
        row += segs.len();
        if row >= body_start + body_rows {
            break;
        }
    }

    // Status bar.
    let status_row = rows - 2;
    frame.write_line(status_row as u16, &status.chars().take(cols).collect::<String>(), "status", false);

    // Input line with caret.
    let input_row = rows - 1;
    let mut input_text = "> ".to_string();
    if let Some(node) = &input_node {
        input_text.push_str(&node.content());
    }
    let input_text: String = input_text.chars().take(cols).collect();
    frame.write_line(input_row as u16, &input_text, "input", false);
    // Caret: reverse-video at the caret offset into the prompt+content
    // (prompt is the 2-char "> " prefix), clamped to the visible text.
    let caret = match &input_node {
        Some(node) => ctx.focus.caret_for(node),
        None => 0,
    };
    let caret = (caret + 2).min(input_text.chars().count().saturating_sub(1));
    if let Some(ch) = input_text.chars().nth(caret)
        && ch != ' '
    {
        frame.set(input_row as u16, caret as u16, ch, "selected");
    }

    Ok(RenderOutput {
        frame,
        viewport_top: top,
    })
}

/// Depth-first body lines. Layout kinds recurse (siblings in ascending z
/// order, so a higher z paints later and occludes); a `row`'s children share
/// each band horizontally; `input` nodes are kernel-rendered on the bottom row
/// and skipped here.
pub(crate) fn collect_lines<'a>(node: &'a Node, out: &mut Vec<BodyLine<'a>>) {
    match node.kind() {
        NodeKind::Stack | NodeKind::Col | NodeKind::Layer => {
            for child in sorted_children(node) {
                collect_lines(child, out);
            }
        }
        NodeKind::Row => {
            // Horizontal layout: each child's lines are laid out side by side,
            // band-by-band, joined with a one-cell gutter. Intrinsic-width
            // scheme (not equal split): a child contributes its natural width
            // and `render` wraps the merged line to the viewport, so the result
            // is deterministic and total-width-safe. The band's anchor node is
            // its first contributing child, keeping focus/viewport lookup
            // meaningful for the leading column.
            let columns: Vec<Vec<BodyLine<'a>>> = sorted_children(node)
                .into_iter()
                .map(|child| {
                    let mut lines = Vec::new();
                    collect_lines(child, &mut lines);
                    lines
                })
                .collect();
            let bands = columns.iter().map(Vec::len).max().unwrap_or(0);
            for band in 0..bands {
                let mut spans: Vec<(String, String)> = Vec::new();
                let mut anchor: Option<&'a Node> = None;
                for lines in &columns {
                    if let Some(line) = lines.get(band) {
                        if anchor.is_none() {
                            anchor = Some(line.node);
                        }
                        if !spans.is_empty() {
                            spans.push((String::from(" "), DEFAULT_STYLE.to_string()));
                        }
                        spans.extend(line.spans.iter().cloned());
                    }
                }
                if let Some(anchor) = anchor {
                    out.push(BodyLine { node: anchor, spans });
                }
            }
        }
        NodeKind::List => {
            // A list's items are its rows; children (if any) follow.
            for item in node.items() {
                out.push(BodyLine {
                    node,
                    spans: vec![(item.label.clone(), DEFAULT_STYLE.to_string())],
                });
            }
            for child in sorted_children(node) {
                collect_lines(child, out);
            }
        }
        NodeKind::Text => out.push(BodyLine {
            node,
            spans: styled_spans(node, DEFAULT_STYLE),
        }),
        NodeKind::Code => out.push(BodyLine {
            node,
            spans: styled_spans(node, "tool"),
        }),
        NodeKind::Button => out.push(BodyLine {
            node,
            spans: vec![(node.content(), DEFAULT_STYLE.to_string())],
        }),
        NodeKind::Input => {}
    }
}

/// Siblings in ascending z (stable, so equal-z siblings keep document order).
fn sorted_children(node: &Node) -> Vec<&Node> {
    let mut children: Vec<&Node> = node.children.iter().collect();
    children.sort_by_key(|c| c.z());
    children
}

/// A text/code node's spans with the kind's default style where a span names
/// none.
fn styled_spans(node: &Node, default: &str) -> Vec<(String, String)> {
    node.spans()
        .iter()
        .map(|s| {
            (
                s.text.clone(),
                s.style.clone().unwrap_or_else(|| default.to_string()),
            )
        })
        .collect()
}

/// Split styled segments into pre-wrapped rows of per-character styles. `\n`
/// forces a row break; a row fills to `cols` first.
fn wrap_spans(spans: &[(String, String)], cols: usize) -> Vec<Vec<(char, String)>> {
    let mut rows: Vec<Vec<(char, String)>> = Vec::new();
    let mut cur: Vec<(char, String)> = Vec::new();
    for (text, style) in spans {
        for ch in text.chars() {
            if ch == '\n' {
                rows.push(std::mem::take(&mut cur));
                continue;
            }
            if cur.len() >= cols {
                rows.push(std::mem::take(&mut cur));
            }
            cur.push((ch, style.clone()));
        }
    }
    rows.push(cur);
    rows
}

/// Split into lines (on '\n') then char-wrap each to `cols`.
pub(crate) fn wrap(text: &str, cols: usize) -> Vec<String> {
    let mut out = Vec::new();
    for raw in text.split('\n') {
        let mut line = String::new();
        for ch in raw.chars() {
            if line.chars().count() >= cols {
                out.push(std::mem::take(&mut line));
            }
            line.push(ch);
        }
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fallback;
    use crate::{ListItem, Node, SemanticTree};

    fn ctx<'a>(
        tree: &'a SemanticTree,
        focus: &'a FocusModel,
        status: &'a str,
        theme: &'a Theme,
    ) -> RenderContext<'a> {
        RenderContext {
            tree,
            theme,
            focus,
            size: (10, 20),
            status,
            staleness: None,
            degraded: false,
        }
    }

    fn tree() -> SemanticTree {
        SemanticTree::new(
            Node::stack("root")
                .child(Node::styled_text("h", "kanbei", "header"))
                .child(Node::list(
                    "list",
                    vec![ListItem::new("a", "first"), ListItem::new("b", "second")],
                ))
                .child(Node::input("input", "hi")),
        )
    }

    #[test]
    fn layout() {
        let t = tree();
        let mut f = FocusModel::new();
        f.revalidate(&t);
        f.caret = 1;
        let out = render(&ctx(&t, &f, "idle", &Theme::default_theme())).unwrap();
        assert_eq!(out.frame.row_text(0), "kanbei");
        assert_eq!(out.frame.row_text(1), "first");
        assert_eq!(out.frame.row_text(2), "second");
        assert_eq!(out.frame.row_text(8), "idle");
        assert_eq!(out.frame.row_text(9), "> hi");
        // caret at offset 1 is reverse-video
        assert_eq!(out.frame.cell(9, 3).style, "selected");
        assert_eq!(out.frame.cell(9, 2).style, "input");
    }

    #[test]
    fn banner_and_degraded_overlays() {
        let t = tree();
        let f = FocusModel::new();
        let theme = Theme::default_theme();
        let mut c = ctx(&t, &f, "idle", &theme);
        c.size = (10, 40);
        c.staleness = Some("publish failed");
        c.degraded = true;
        let out = render(&c).unwrap();
        assert!(out.frame.row_text(0).starts_with("composition stale"));
        assert_eq!(out.frame.row_text(1), "kanbei");
        assert!(out.frame.row_text(8).contains("idle"));
        assert!(out.frame.row_text(8).contains("[degraded]"));
        assert!(out.frame.row_text(8).contains("[stale]"));
    }

    #[test]
    fn viewport_keeps_focus_visible() {
        let t = SemanticTree::new(
            Node::stack("root").child(Node::styled_text(
                "top",
                "line one, far above",
                "response",
            )),
        );
        let mut f = FocusModel::new();
        f.focused = Some("top".into());
        let out = render(&ctx(&t, &f, "idle", &Theme::default_theme())).unwrap();
        assert_eq!(out.viewport_top, 0);
        assert_eq!(out.frame.row_text(0), "line one, far above");
    }

    #[test]
    fn scrolls_to_tail_without_focus() {
        let t = tree();
        let f = FocusModel::new();
        let out = render(&ctx(&t, &f, "idle", &Theme::default_theme())).unwrap();
        // 10 rows: body(8) + status + input; all 3 body lines fit
        assert_eq!(out.frame.row_text(0), "kanbei");
        assert_eq!(out.frame.row_text(1), "first");
        assert_eq!(out.frame.row_text(2), "second");
    }

    #[test]
    fn wraps_long_lines() {
        let t = SemanticTree::new(Node::stack("root").child(Node::text(
            "t",
            "0123456789 0123456789, wrapped tail",
        )));
        let f = FocusModel::new();
        let out = render(&ctx(&t, &f, "idle", &Theme::default_theme())).unwrap();
        assert_eq!(out.frame.row_text(0), "0123456789 012345678");
        assert_eq!(out.frame.row_text(1), "9, wrapped tail");
    }

    #[test]
    fn renders_sibling_z_in_ascending_paint_order() {
        let t = SemanticTree::new(
            Node::stack("root")
                .child(Node::stack_z("high", 3).child(Node::text("a", "high")))
                .child(Node::stack_z("low", -1).child(Node::text("b", "low"))),
        );
        let f = FocusModel::new();
        let out = render(&ctx(&t, &f, "idle", &Theme::default_theme())).unwrap();
        assert_eq!(out.frame.row_text(0), "low");
        assert_eq!(out.frame.row_text(1), "high");
    }

    #[test]
    fn spans_keep_their_styles() {
        let t = SemanticTree::new(
            Node::stack("root").child(Node::text_spans(
                "t",
                vec![
                    crate::Span::styled("ab", "user"),
                    crate::Span::plain("cd"),
                ],
            )),
        );
        let f = FocusModel::new();
        let out = render(&ctx(&t, &f, "idle", &Theme::default_theme())).unwrap();
        assert_eq!(out.frame.row_text(0), "abcd");
        assert_eq!(out.frame.cell(0, 0).style, "user");
        assert_eq!(out.frame.cell(0, 1).style, "user");
        assert_eq!(out.frame.cell(0, 2).style, DEFAULT_STYLE);
    }

    #[test]
    fn row_places_children_on_the_same_line() {
        let t = SemanticTree::new(
            Node::stack("root").child(
                Node::row("r")
                    .child(Node::text("a", "left"))
                    .child(Node::text("b", "right")),
            ),
        );
        let f = FocusModel::new();
        let out = render(&ctx(&t, &f, "idle", &Theme::default_theme())).unwrap();
        assert_eq!(out.frame.row_text(0), "left right");
        assert_eq!(out.frame.row_text(1), "", "row consumes a single band");

        // `col` stays vertical (the old row==col aliasing is gone).
        let t = SemanticTree::new(
            Node::stack("root").child(
                Node::col("c")
                    .child(Node::text("a", "left"))
                    .child(Node::text("b", "right")),
            ),
        );
        let out = render(&ctx(&t, &f, "idle", &Theme::default_theme())).unwrap();
        assert_eq!(out.frame.row_text(0), "left");
        assert_eq!(out.frame.row_text(1), "right");
    }

    #[test]
    fn too_small() {
        let t = tree();
        let f = FocusModel::new();
        let theme = Theme::default_theme();
        let mut c = ctx(&t, &f, "idle", &theme);
        c.size = (2, 10);
        assert!(matches!(render(&c), Err(RenderError::TooSmall { rows: 2 })));
    }

    #[test]
    fn controls_blanked() {
        let t = SemanticTree::new(Node::stack("root").child(Node::text("t", "a\tb")));
        let f = FocusModel::new();
        let out = render(&ctx(&t, &f, "idle", &Theme::default_theme())).unwrap();
        assert_eq!(out.frame.row_text(0), "a b");
    }

    #[test]
    fn placeholder_and_fallback_renders() {
        let p = fallback::placeholder_tree("workbench", "reduce failed");
        let f = FocusModel::new();
        let out = render(&ctx(&p, &f, "idle", &Theme::default_theme())).unwrap();
        let body: String = (0..8).map(|r| out.frame.row_text(r)).collect::<Vec<_>>().join("|");
        assert!(body.contains("UI component faulted"), "body: {body}");
        assert!(body.contains("reduce failed"), "body: {body}");

        let fb = fallback::FallbackUi::new("kernel render fault");
        let tree = fb.tree();
        let out = render(&ctx(&tree, &f, "safe mode", &Theme::default_theme())).unwrap();
        assert_eq!(out.frame.row_text(0), "kanbei safe mode");
        assert_eq!(out.frame.row_text(9), ">");
    }
}
