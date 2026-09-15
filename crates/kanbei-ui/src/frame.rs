//! Kernel-owned rendering: `SemanticTree + Theme -> ratatui::Buffer`
//! (architecture.md UI model). The layout is deterministic and module-free:
//! banner/header rows on top, body in the middle, kernel status bar and the
//! focused input line at the bottom. Luau/Wasm never draws cells (R-27,
//! consistency 13).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color as RColor, Style as RStyle};
use ratatui::text::{Line, Span};

use crate::focus::FocusModel;
use crate::theme::{DEFAULT_STYLE, Theme};
use crate::tree::{Node, NodeKind, SemanticTree};
use crate::tui::resolve_style;

/// Minimum terminal rows for a usable frame: banner/header + body + status +
/// input.
pub const MIN_ROWS: usize = 4;

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
    /// The native selection pointer (a body node id): the last non-input node
    /// focus landed on. Rendered with the `selected` style so the selection
    /// survives focus moving onto the composer input.
    pub selection: Option<&'a str>,
    pub staleness: Option<&'a str>,
    pub degraded: bool,
}

/// The kernel's rendered surface: the ratatui [`Buffer`] it composed plus the
/// viewport top the body scrolled to (the caller stores it back into the focus
/// model). Cells carry final, theme-resolved styles — no style keys survive
/// the render — so the present path paints the buffer as-is.
#[derive(Debug, Clone)]
pub struct RenderOutput {
    pub buffer: Buffer,
    pub viewport_top: usize,
}

impl RenderOutput {
    pub fn rows(&self) -> u16 {
        self.buffer.area.height
    }

    pub fn cols(&self) -> u16 {
        self.buffer.area.width
    }

    /// The visible text of one row (inspection/test helper).
    pub fn row_text(&self, row: u16) -> String {
        (0..self.buffer.area.width)
            .map(|col| {
                self.buffer[(col, row)]
                    .symbol()
                    .chars()
                    .next()
                    .unwrap_or(' ')
            })
            .collect::<String>()
            .trim_end()
            .to_string()
    }

    /// The resolved style of one cell (inspection/test helper). Reset
    /// channels stay unset, matching [`resolve_style`]'s output.
    pub fn cell_style(&self, row: u16, col: u16) -> RStyle {
        let cell = &self.buffer[(col, row)];
        let mut style = RStyle::default().add_modifier(cell.modifier);
        if cell.fg != RColor::Reset {
            style = style.fg(cell.fg);
        }
        if cell.bg != RColor::Reset {
            style = style.bg(cell.bg);
        }
        style
    }

    /// Paint this frame through the kernel terminal boundary (ratatui diffing).
    pub fn present(&self, terminal: &mut dyn crate::terminal::Terminal) -> std::io::Result<()> {
        crate::terminal::present(terminal, self)
    }
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

/// Render the tree into a ratatui [`Buffer`]. Layout (top to bottom):
/// 1. staleness banner (when present), then the first header node;
/// 2. body: depth-first lines (list items, text wrapped to the width, status
///    and button nodes); scrolled so the focused node stays visible;
/// 3. kernel status bar;
/// 4. the input line: `> ` + focused input content with the caret drawn in
///    reverse video.
///
/// Styles are resolved through the theme here, so every cell carries its final
/// ratatui style and the present path paints the buffer as-is.
pub fn render(ctx: &RenderContext) -> Result<RenderOutput, RenderError> {
    let (rows, cols) = ctx.size;
    let rows = rows as usize;
    let cols = cols as usize;
    if rows < MIN_ROWS {
        return Err(RenderError::TooSmall { rows: rows as u16 });
    }
    let mut buf = Buffer::empty(Rect::new(0, 0, cols as u16, rows as u16));
    let theme = ctx.theme;

    // 1. banner row. There is no header kind: a module composes its title as
    // the first `text` row of the body, so titled workbenches occupy the same
    // top row they did when the kernel special-cased headers.
    if let Some(reason) = ctx.staleness {
        let line = plain_line(theme, &crate::fallback::staleness_text(reason), "banner", cols);
        paint_line(&mut buf, 0, &line, cols as u16);
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
        let emphasized = Some(line.node.id.as_str()) == ctx.focus.focused.as_deref()
            || Some(line.node.id.as_str()) == ctx.selection;
        let segs = wrap_spans(&line.spans, cols);
        for (seg_row, chars) in segs.iter().enumerate() {
            let r = row + seg_row;
            if r >= body_start + body_rows {
                break;
            }
            let line = body_line(theme, chars, emphasized);
            paint_line(&mut buf, r as u16, &line, cols as u16);
        }
        row += segs.len();
        if row >= body_start + body_rows {
            break;
        }
    }

    // Status bar.
    let status_row = rows - 2;
    let line = plain_line(theme, &status, "status", cols);
    paint_line(&mut buf, status_row as u16, &line, cols as u16);

    // Input line with caret.
    let input_row = rows - 1;
    let mut input_text = "> ".to_string();
    if let Some(node) = &input_node {
        input_text.push_str(&node.content());
    }
    let input_text: String = input_text.chars().take(cols).collect();
    let line = plain_line(theme, &input_text, "input", cols);
    paint_line(&mut buf, input_row as u16, &line, cols as u16);
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
        let cell = &mut buf[(caret as u16, input_row as u16)];
        // The old cell writer placed the raw char (even a control) and merely
        // restyled the cell, so mirror that rather than the sanitized run.
        cell.set_char(ch);
        cell.set_style(resolve_style(theme, Some("selected")));
    }

    Ok(RenderOutput {
        buffer: buf,
        viewport_top: top,
    })
}

/// The frame contract is one visible cell per char and controls are never
/// emitted, so sanitize control characters up front.
fn sanitize(ch: char) -> char {
    if ch.is_control() { ' ' } else { ch }
}

/// Paint a line one char per cell, bounded by `cols`. ratatui's own line
/// painting uses grapheme widths (a wide char would consume two cells and a
/// zero-width mark one), which would shift text against the frame contract's
/// one cell per char; the contract wins, so cells are set directly.
fn paint_line(buf: &mut Buffer, row: u16, line: &Line<'_>, cols: u16) {
    let mut x = 0u16;
    'spans: for span in &line.spans {
        for ch in span.content.chars() {
            if x >= cols {
                break 'spans;
            }
            let cell = &mut buf[(x, row)];
            cell.set_char(ch);
            cell.set_style(span.style);
            x += 1;
        }
    }
}

/// A single-style line, truncated to `cols`, with the named theme style.
fn plain_line(theme: &Theme, text: &str, name: &str, cols: usize) -> Line<'static> {
    let text: String = text.chars().take(cols).map(sanitize).collect();
    Line::from(Span::styled(text, resolve_style(theme, Some(name))))
}

/// A pre-wrapped body row: one span per run of equal style; a focused row is
/// uniformly reverse video (the kernel's focus highlight).
fn body_line(theme: &Theme, chars: &[(char, String)], focused: bool) -> Line<'static> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut text = String::new();
    let mut run: Option<&str> = None;
    for (ch, name) in chars {
        let name = if focused { "selected" } else { name.as_str() };
        if run != Some(name) {
            push_run(&mut spans, theme, &mut text, run);
            run = Some(name);
        }
        text.push(sanitize(*ch));
    }
    push_run(&mut spans, theme, &mut text, run);
    Line::from(spans)
}

fn push_run(
    spans: &mut Vec<Span<'static>>,
    theme: &Theme,
    text: &mut String,
    name: Option<&str>,
) {
    if text.is_empty() {
        return;
    }
    let style = resolve_style(theme, Some(name.unwrap_or(DEFAULT_STYLE)));
    spans.push(Span::styled(std::mem::take(text), style));
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
            // Each column occupies its widest line's width in EVERY band, so a
            // column with no line in a band (unequal column heights) pads to
            // the same offset and later columns do not shift left.
            let widths: Vec<usize> = columns
                .iter()
                .map(|lines| lines.iter().map(|l| spans_width(&l.spans)).max().unwrap_or(0))
                .collect();
            let bands = columns.iter().map(Vec::len).max().unwrap_or(0);
            for band in 0..bands {
                let mut spans: Vec<(String, String)> = Vec::new();
                let mut anchor: Option<&'a Node> = None;
                for (col, lines) in columns.iter().enumerate() {
                    let width = widths[col];
                    if !spans.is_empty() {
                        spans.push((String::from(" "), DEFAULT_STYLE.to_string()));
                    }
                    match lines.get(band) {
                        Some(line) => {
                            if anchor.is_none() {
                                anchor = Some(line.node);
                            }
                            spans.extend(line.spans.iter().cloned());
                            let pad = width.saturating_sub(spans_width(&line.spans));
                            if pad > 0 {
                                spans.push((" ".repeat(pad), DEFAULT_STYLE.to_string()));
                            }
                        }
                        // Absent band: keep the column's position with padding.
                        None if width > 0 => {
                            spans.push((" ".repeat(width), DEFAULT_STYLE.to_string()));
                        }
                        None => {}
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

/// The display width (chars) of a run of styled spans.
fn spans_width(spans: &[(String, String)]) -> usize {
    spans.iter().map(|(text, _)| text.chars().count()).sum()
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
            selection: None,
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
        let theme = Theme::default_theme();
        let out = render(&ctx(&t, &f, "idle", &theme)).unwrap();
        assert_eq!(out.row_text(0), "kanbei");
        assert_eq!(out.row_text(1), "first");
        assert_eq!(out.row_text(2), "second");
        assert_eq!(out.row_text(8), "idle");
        assert_eq!(out.row_text(9), "> hi");
        // caret at offset 1 is reverse-video; the prompt cell is the input style
        assert_eq!(out.cell_style(9, 3), resolve_style(&theme, Some("selected")));
        assert_eq!(out.cell_style(9, 2), resolve_style(&theme, Some("input")));
    }

    /// Decision 32 amendment: `selection` is applied natively — the selected
    /// body node renders with the `selected` style even when focus is
    /// elsewhere (e.g. on the composer input).
    #[test]
    fn selection_highlights_natively() {
        let t = tree();
        let f = FocusModel::new();
        let theme = Theme::default_theme();
        let mut c = ctx(&t, &f, "idle", &theme);
        c.selection = Some("h");
        let out = render(&c).unwrap();
        assert_eq!(
            out.cell_style(0, 0),
            resolve_style(&theme, Some("selected")),
            "the selected node is highlighted"
        );
        assert_ne!(
            out.cell_style(1, 0),
            resolve_style(&theme, Some("selected")),
            "an unselected row keeps its own style"
        );
    }

    #[test]
    fn banner_and_degraded_overlays() {        let t = tree();
        let f = FocusModel::new();
        let theme = Theme::default_theme();
        let mut c = ctx(&t, &f, "idle", &theme);
        c.size = (10, 40);
        c.staleness = Some("publish failed");
        c.degraded = true;
        let out = render(&c).unwrap();
        assert!(out.row_text(0).starts_with("composition stale"));
        assert_eq!(out.row_text(1), "kanbei");
        assert!(out.row_text(8).contains("idle"));
        assert!(out.row_text(8).contains("[degraded]"));
        assert!(out.row_text(8).contains("[stale]"));
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
        assert_eq!(out.row_text(0), "line one, far above");
    }

    #[test]
    fn scrolls_to_tail_without_focus() {
        let t = tree();
        let f = FocusModel::new();
        let out = render(&ctx(&t, &f, "idle", &Theme::default_theme())).unwrap();
        // 10 rows: body(8) + status + input; all 3 body lines fit
        assert_eq!(out.row_text(0), "kanbei");
        assert_eq!(out.row_text(1), "first");
        assert_eq!(out.row_text(2), "second");
    }

    #[test]
    fn wraps_long_lines() {
        let t = SemanticTree::new(Node::stack("root").child(Node::text(
            "t",
            "0123456789 0123456789, wrapped tail",
        )));
        let f = FocusModel::new();
        let out = render(&ctx(&t, &f, "idle", &Theme::default_theme())).unwrap();
        assert_eq!(out.row_text(0), "0123456789 012345678");
        assert_eq!(out.row_text(1), "9, wrapped tail");
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
        assert_eq!(out.row_text(0), "low");
        assert_eq!(out.row_text(1), "high");
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
        let theme = Theme::default_theme();
        let out = render(&ctx(&t, &f, "idle", &theme)).unwrap();
        assert_eq!(out.row_text(0), "abcd");
        assert_eq!(out.cell_style(0, 0), resolve_style(&theme, Some("user")));
        assert_eq!(out.cell_style(0, 1), resolve_style(&theme, Some("user")));
        assert_eq!(
            out.cell_style(0, 2),
            resolve_style(&theme, Some(DEFAULT_STYLE))
        );
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
        assert_eq!(out.row_text(0), "left right");
        assert_eq!(out.row_text(1), "", "row consumes a single band");

        // `col` stays vertical (the old row==col aliasing is gone).
        let t = SemanticTree::new(
            Node::stack("root").child(
                Node::col("c")
                    .child(Node::text("a", "left"))
                    .child(Node::text("b", "right")),
            ),
        );
        let out = render(&ctx(&t, &f, "idle", &Theme::default_theme())).unwrap();
        assert_eq!(out.row_text(0), "left");
        assert_eq!(out.row_text(1), "right");
    }

    #[test]
    fn row_columns_stay_aligned_across_unequal_height_bands() {
        // A one-line left column and a two-line right column: the right column
        // must keep its offset in the band where the left column has no line,
        // instead of shifting left.
        let t = SemanticTree::new(
            Node::stack("root").child(
                Node::row("r")
                    .child(Node::text("a", "A"))
                    .child(
                        Node::col("c")
                            .child(Node::text("b1", "B1"))
                            .child(Node::text("b2", "B2")),
                    ),
            ),
        );
        let f = FocusModel::new();
        let out = render(&ctx(&t, &f, "idle", &Theme::default_theme())).unwrap();
        assert_eq!(out.row_text(0), "A B1");
        assert_eq!(out.row_text(1), "  B2", "the column offset is kept");
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
        assert_eq!(out.row_text(0), "a b");
    }

    #[test]
    fn placeholder_and_fallback_renders() {
        let p = fallback::placeholder_tree("workbench", "reduce failed");
        let f = FocusModel::new();
        let out = render(&ctx(&p, &f, "idle", &Theme::default_theme())).unwrap();
        let body: String = (0..8).map(|r| out.row_text(r)).collect::<Vec<_>>().join("|");
        assert!(body.contains("UI component faulted"), "body: {body}");
        assert!(body.contains("reduce failed"), "body: {body}");

        let fb = fallback::FallbackUi::new("kernel render fault");
        let tree = fb.tree();
        let out = render(&ctx(&tree, &f, "safe mode", &Theme::default_theme())).unwrap();
        assert_eq!(out.row_text(0), "kanbei safe mode");
        assert_eq!(out.row_text(9), ">");
    }

    /// Pin the whole visible frame the ratatui engine emits for a
    /// representative tree, so the engine swap cannot drift.
    #[test]
    fn ratatui_engine_pins_visible_frame() {
        let t = tree();
        let mut f = FocusModel::new();
        f.revalidate(&t);
        f.caret = 1;
        let theme = Theme::default_theme();
        let out = render(&ctx(&t, &f, "idle", &theme)).unwrap();
        let rows: Vec<String> = (0..10).map(|r| out.row_text(r)).collect();
        assert_eq!(rows.join("|"), "kanbei|first|second||||||idle|> hi");
        assert_eq!(out.viewport_top, 0);
        assert_eq!(out.cell_style(9, 0), resolve_style(&theme, Some("input")));
        assert_eq!(
            out.cell_style(9, 3),
            resolve_style(&theme, Some("selected"))
        );
    }

    /// The render resolves theme keys at paint time: cells carry the theme's
    /// final style (no style-key round-trip), including a re-themed caret.
    #[test]
    fn cells_carry_resolved_theme_styles() {
        let t = tree();
        let mut f = FocusModel::new();
        f.revalidate(&t);
        f.caret = 1;
        let mut theme = Theme::default_theme();
        theme.styles.insert(
            "selected".into(),
            crate::Style {
                fg: crate::Color::Magenta,
                bg: crate::Color::Default,
                bold: false,
                underline: false,
                reverse: true,
            },
        );
        let out = render(&ctx(&t, &f, "idle", &theme)).unwrap();
        // The caret (offset 1 + two-char prompt) takes the re-themed style...
        assert_eq!(
            out.cell_style(9, 3),
            resolve_style(&theme, Some("selected"))
        );
        // ...and the prompt keeps the input style.
        assert_eq!(out.cell_style(9, 2), resolve_style(&theme, Some("input")));
        assert_ne!(
            out.cell_style(9, 3),
            resolve_style(&Theme::default_theme(), Some("selected"))
        );
    }
}
