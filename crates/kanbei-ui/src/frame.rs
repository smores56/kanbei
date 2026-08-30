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

struct BodyLine<'a> {
    node: &'a Node,
    text: String,
    style: String,
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

    // 1. banner + header rows.
    let banner_row: Option<usize> = ctx.staleness.map(|_| 0);
    let header_row = if banner_row.is_some() { 1 } else { 0 };
    if let Some(reason) = ctx.staleness {
        frame.write_line(0, &crate::fallback::staleness_text(reason), "banner", false);
    }
    let header = ctx
        .tree
        .nodes()
        .into_iter()
        .find(|n| n.kind == NodeKind::Header);
    if let Some(h) = header {
        let text: String = h.content.chars().take(cols).collect();
        frame.write_line(header_row as u16, &text, "header", false);
    }

    // 2. body lines (header and input nodes are kernel-rendered elsewhere).
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
    let body_start = header_row + 1;
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
        let segs = wrap(&line.text, cols);
        for (seg_row, text) in segs.iter().enumerate() {
            let r = row + seg_row;
            if r >= body_start + body_rows {
                break;
            }
            frame.write_line(r as u16, text, &line.style, focused);
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
        input_text.push_str(&node.content);
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

/// Depth-first body lines. `Header` and `Input` nodes are kernel-rendered
/// (top/bottom rows) and skipped here.
fn collect_lines<'a>(node: &'a Node, out: &mut Vec<BodyLine<'a>>) {
    match node.kind {
        NodeKind::Root | NodeKind::List => {
            for child in &node.children {
                collect_lines(child, out);
            }
        }
        NodeKind::Header | NodeKind::Input => {}
        NodeKind::ListItem | NodeKind::Text | NodeKind::Status | NodeKind::Button => {
            out.push(BodyLine {
                node,
                text: node.content.clone(),
                style: node
                    .style
                    .clone()
                    .unwrap_or_else(|| match node.kind {
                        NodeKind::Status => "status".to_string(),
                        NodeKind::Placeholder => "error".to_string(),
                        _ => DEFAULT_STYLE.to_string(),
                    }),
            });
        }
        NodeKind::Placeholder => {
            out.push(BodyLine {
                node,
                text: node.content.clone(),
                style: "error".to_string(),
            });
        }
    }
}

/// Split into lines (on '\n') then char-wrap each to `cols`.
fn wrap(text: &str, cols: usize) -> Vec<String> {
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
    use crate::{Node, SemanticTree};

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
            Node::new("root", NodeKind::Root)
                .child(Node::new("h", NodeKind::Header).with_content("kanbei"))
                .child(
                    Node::new("list", NodeKind::List)
                        .child(Node::new("a", NodeKind::ListItem).with_content("first"))
                        .child(Node::new("b", NodeKind::ListItem).with_content("second")),
                )
                .child(Node::new("input", NodeKind::Input).with_content("hi").focusable()),
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
        let mut f = FocusModel::new();
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
            Node::new("root", NodeKind::Root).child(
                Node::new("list", NodeKind::List).child(
                    Node::new("top", NodeKind::ListItem)
                        .with_content("line one, far above")
                        .focusable(),
                ),
            ),
        );
        let mut f = FocusModel::new();
        f.focused = Some("top".into());
        let out = render(&ctx(&t, &f, "idle", &Theme::default_theme())).unwrap();
        assert_eq!(out.viewport_top, 0);
        assert_eq!(out.frame.row_text(1), "line one, far above");
    }

    #[test]
    fn scrolls_to_tail_without_focus() {
        let t = tree();
        let f = FocusModel::new();
        let out = render(&ctx(&t, &f, "idle", &Theme::default_theme())).unwrap();
        // 10 rows: banner/header(1) + body(7) + status + input; 2 lines fit
        assert_eq!(out.frame.row_text(1), "first");
        assert_eq!(out.frame.row_text(2), "second");
    }

    #[test]
    fn wraps_long_lines() {
        let t = SemanticTree::new(
            Node::new("root", NodeKind::Root).child(
                Node::new("t", NodeKind::Text).with_content("0123456789 0123456789, wrapped tail"),
            ),
        );
        let f = FocusModel::new();
        let out = render(&ctx(&t, &f, "idle", &Theme::default_theme())).unwrap();
        assert_eq!(out.frame.row_text(1), "0123456789 012345678");
        assert_eq!(out.frame.row_text(2), "9, wrapped tail");
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
        let t = SemanticTree::new(
            Node::new("root", NodeKind::Root)
                .child(Node::new("t", NodeKind::Text).with_content("a\tb")),
        );
        let f = FocusModel::new();
        let out = render(&ctx(&t, &f, "idle", &Theme::default_theme())).unwrap();
        assert_eq!(out.frame.row_text(1), "a b");
    }

    #[test]
    fn placeholder_and_fallback_renders() {
        let p = fallback::placeholder_tree("workbench", "reduce failed");
        let f = FocusModel::new();
        let out = render(&ctx(&p, &f, "idle", &Theme::default_theme())).unwrap();
        let body: String = (1..8).map(|r| out.frame.row_text(r)).collect::<Vec<_>>().join("|");
        assert!(body.contains("UI component faulted"), "body: {body}");
        assert!(body.contains("reduce failed"), "body: {body}");

        let fb = fallback::FallbackUi::new("kernel render fault");
        let tree = fb.tree();
        let out = render(&ctx(&tree, &f, "safe mode", &Theme::default_theme())).unwrap();
        assert_eq!(out.frame.row_text(0), "kanbei safe mode");
        assert_eq!(out.frame.row_text(9), ">");
    }
}
