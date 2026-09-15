//! Kernel-owned rendering: `SemanticTree + Theme -> ratatui::Buffer`
//! (architecture.md UI model). The composed tree owns the whole surface: the
//! module authors layout, z-order and the status/header/input rows, and the
//! kernel lays the tree out deterministically and overlays only its own
//! chrome — the staleness banner, the focused input's caret and the
//! safe-mode/render-fault fallbacks (R-27). Luau/Wasm never draws cells
//! (consistency 13).

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color as RColor, Style as RStyle};
use ratatui::text::{Line, Span};

use crate::focus::FocusModel;
use crate::theme::{DEFAULT_STYLE, Theme};
use crate::tree::{Node, NodeKind, SemanticTree};
use crate::tui::resolve_style;

/// Minimum terminal rows for a usable frame: the staleness banner plus at
/// least one tree row.
pub const MIN_ROWS: usize = 2;

/// Everything the renderer needs. Staleness is the kernel's only overlay; the
/// tree (layout, status, input) and focus come from the module-facing side.
pub struct RenderContext<'a> {
    pub tree: &'a SemanticTree,
    pub theme: &'a Theme,
    pub focus: &'a FocusModel,
    /// Terminal size in (rows, cols).
    pub size: (u16, u16),
    /// The native selection pointer (a body node id): the last non-input node
    /// focus landed on. Rendered with the `selected` style so the selection
    /// survives focus moving onto the composer input.
    pub selection: Option<&'a str>,
    /// Kernel-owned staleness banner (R-27 composition fault), overlaid on the
    /// last-valid tree.
    pub staleness: Option<&'a str>,
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
///
/// `input` names the input node laid out within the line (directly or nested
/// in a `row`) with its char column offset, so the kernel can place the caret
/// from the tree rather than a kernel-built string (decision 22).
pub struct BodyLine<'a> {
    pub node: &'a Node,
    pub spans: Vec<(String, String)>,
    pub input: Option<(&'a Node, usize)>,
}

impl<'a> BodyLine<'a> {
    fn plain(node: &'a Node, spans: Vec<(String, String)>) -> Self {
        BodyLine {
            node,
            spans,
            input: None,
        }
    }

    /// Whether the line carries the focused node (directly or as its input).
    fn has_focus(&self, focused: Option<&str>) -> bool {
        let id = self.node.id.as_str();
        id == focused.unwrap_or_default()
            || self
                .input
                .is_some_and(|(n, _)| Some(n.id.as_str()) == focused)
    }
}

/// Render the tree into a ratatui [`Buffer`]. The composed tree owns the whole
/// surface: layout kinds lay out depth-first (siblings in ascending z order),
/// `input` nodes are ordinary lines, and the frame is sized to the terminal.
/// The kernel overlays only its own chrome — the staleness banner, the focused
/// input's caret — and keeps scrolling/focus/selection native (decision 22).
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

    // Staleness banner (R-27 composition fault): the kernel is the only source
    // of chrome that does not come from the tree.
    if let Some(reason) = ctx.staleness {
        let line = plain_line(theme, &crate::fallback::staleness_text(reason), "banner", cols);
        paint_line(&mut buf, 0, &line, cols as u16);
    }
    let body_start = if ctx.staleness.is_some() { 1 } else { 0 };

    let mut lines: Vec<BodyLine> = Vec::new();
    collect_lines(&ctx.tree.root, &mut lines);

    // Viewport: keep the focused line visible; tail when unfocused. The whole
    // remaining frame is body — no reserved status/input rows.
    let body_rows = rows.saturating_sub(body_start);
    let focused_idx = lines
        .iter()
        .position(|l| l.has_focus(ctx.focus.focused.as_deref()));
    let max_top = lines.len().saturating_sub(body_rows);
    let top = match focused_idx {
        Some(f) => f.min(max_top),
        None => max_top,
    };
    let mut row = body_start;
    // The focused input's frame row and caret column, resolved from the tree.
    let mut caret: Option<(u16, u16)> = None;
    for line in lines.iter().skip(top) {
        let is_input = line.node.kind() == NodeKind::Input;
        let emphasized = !is_input
            && (line.has_focus(ctx.focus.focused.as_deref())
                || Some(line.node.id.as_str()) == ctx.selection);
        let segs = wrap_spans(&line.spans, cols);
        for (seg_row, chars) in segs.iter().enumerate() {
            let r = row + seg_row;
            if r >= body_start + body_rows {
                break;
            }
            if seg_row == 0
                && let Some((node, offset)) = line.input
                && ctx.focus.focused.as_deref() == Some(node.id.as_str())
            {
                // Keep the caret on the last content char when it sits past the
                // end, so a focused-but-empty composer still shows a caret box.
                let len = node.content().chars().count();
                let at = ctx.focus.caret_for(node).min(len.saturating_sub(1));
                caret = Some((r as u16, (offset + at) as u16));
            }
            let painted = body_line(theme, chars, emphasized);
            paint_line(&mut buf, r as u16, &painted, cols as u16);
        }
        row += segs.len();
        if row >= body_start + body_rows {
            break;
        }
    }

    // Caret: reverse-video over the focused input's character at the tree-
    // resolved offset. A caret on a blank cell is invisible, matching the
    // kernel's prior "no caret on space" behavior.
    if let Some((r, c)) = caret
        && c < cols as u16
    {
        let ch = buf[(c, r)].symbol().chars().next().unwrap_or(' ');
        if ch != ' ' {
            let cell = &mut buf[(c, r)];
            cell.set_char(ch);
            cell.set_style(resolve_style(theme, Some("selected")));
        }
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
            // its first INPUT child when it has one (the composer row), else
            // its first contributing child, keeping focus/viewport/caret
            // lookup meaningful.
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
                let mut input: Option<(&'a Node, usize)> = None;
                for (col, lines) in columns.iter().enumerate() {
                    let width = widths[col];
                    if !spans.is_empty() {
                        spans.push((String::from(" "), DEFAULT_STYLE.to_string()));
                    }
                    match lines.get(band) {
                        Some(line) => {
                            // An input child anchors the band (and records its
                            // column) so the composer's focus and caret resolve
                            // to the input, not the prompt.
                            if line.input.is_some() || line.node.kind() == NodeKind::Input {
                                let node = line.input.map(|(n, _)| n).unwrap_or(line.node);
                                input = Some((node, spans_width(&spans)));
                                anchor = Some(node);
                            } else if anchor.is_none() {
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
                    out.push(BodyLine {
                        node: anchor,
                        spans,
                        input,
                    });
                }
            }
        }
        NodeKind::List => {
            // A list's items are its rows; children (if any) follow.
            for item in node.items() {
                out.push(BodyLine::plain(
                    node,
                    vec![(item.label.clone(), DEFAULT_STYLE.to_string())],
                ));
            }
            for child in sorted_children(node) {
                collect_lines(child, out);
            }
        }
        NodeKind::Text => out.push(BodyLine::plain(node, styled_spans(node, DEFAULT_STYLE))),
        NodeKind::Code => out.push(BodyLine::plain(node, styled_spans(node, "tool"))),
        NodeKind::Button => out.push(BodyLine::plain(
            node,
            vec![(node.content(), DEFAULT_STYLE.to_string())],
        )),
        // The input is an ordinary tree line (decision 6): the module authors
        // it and the kernel places the caret.
        NodeKind::Input => out.push(BodyLine {
            node,
            spans: vec![(node.content(), "input".to_string())],
            input: Some((node, 0)),
        }),
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
        theme: &'a Theme,
    ) -> RenderContext<'a> {
        RenderContext {
            tree,
            theme,
            focus,
            size: (10, 20),
            selection: None,
            staleness: None,
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
        let out = render(&ctx(&t, &f, &theme)).unwrap();
        assert_eq!(out.row_text(0), "kanbei");
        assert_eq!(out.row_text(1), "first");
        assert_eq!(out.row_text(2), "second");
        // The input is an ordinary tree line, not a kernel-pinned bottom row.
        assert_eq!(out.row_text(3), "hi");
        assert_eq!(out.row_text(9), "", "no kernel status/input chrome");
        // caret at content offset 1 is reverse-video; the lead cell is input style
        assert_eq!(out.cell_style(3, 1), resolve_style(&theme, Some("selected")));
        assert_eq!(out.cell_style(3, 0), resolve_style(&theme, Some("input")));
    }

    /// Decision 32 amendment: `selection` is applied natively — the selected
    /// body node renders with the `selected` style even when focus is
    /// elsewhere (e.g. on the composer input).
    #[test]
    fn selection_highlights_natively() {
        let t = tree();
        let f = FocusModel::new();
        let theme = Theme::default_theme();
        let mut c = ctx(&t, &f, &theme);
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

    /// The staleness banner is the kernel's only overlay (R-27): it shifts the
    /// last-valid tree down without inventing a status/input row.
    #[test]
    fn staleness_banner_overlays_last_valid_tree() {
        let t = tree();
        let f = FocusModel::new();
        let theme = Theme::default_theme();
        let mut c = ctx(&t, &f, &theme);
        c.size = (10, 40);
        c.staleness = Some("publish failed");
        let out = render(&c).unwrap();
        assert!(out.row_text(0).starts_with("composition stale"));
        assert_eq!(out.row_text(1), "kanbei");
        assert_eq!(out.row_text(2), "first");
        assert_eq!(out.row_text(4), "hi");
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
        let out = render(&ctx(&t, &f, &Theme::default_theme())).unwrap();
        assert_eq!(out.viewport_top, 0);
        assert_eq!(out.row_text(0), "line one, far above");
    }

    #[test]
    fn scrolls_to_tail_without_focus() {
        let t = tree();
        let f = FocusModel::new();
        let out = render(&ctx(&t, &f, &Theme::default_theme())).unwrap();
        // 10 rows of body; the whole tree (incl. input) fits.
        assert_eq!(out.row_text(0), "kanbei");
        assert_eq!(out.row_text(1), "first");
        assert_eq!(out.row_text(2), "second");
        assert_eq!(out.row_text(3), "hi", "input scrolls with the body");
    }

    #[test]
    fn wraps_long_lines() {
        let t = SemanticTree::new(Node::stack("root").child(Node::text(
            "t",
            "0123456789 0123456789, wrapped tail",
        )));
        let f = FocusModel::new();
        let out = render(&ctx(&t, &f, &Theme::default_theme())).unwrap();
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
        let out = render(&ctx(&t, &f, &Theme::default_theme())).unwrap();
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
        let out = render(&ctx(&t, &f, &theme)).unwrap();
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
        let out = render(&ctx(&t, &f, &Theme::default_theme())).unwrap();
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
        let out = render(&ctx(&t, &f, &Theme::default_theme())).unwrap();
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
        let out = render(&ctx(&t, &f, &Theme::default_theme())).unwrap();
        assert_eq!(out.row_text(0), "A B1");
        assert_eq!(out.row_text(1), "  B2", "the column offset is kept");
    }

    #[test]
    fn too_small() {
        let t = tree();
        let f = FocusModel::new();
        let theme = Theme::default_theme();
        let mut c = ctx(&t, &f, &theme);
        c.size = (1, 10);
        assert!(matches!(render(&c), Err(RenderError::TooSmall { rows: 1 })));
    }

    #[test]
    fn controls_blanked() {
        let t = SemanticTree::new(Node::stack("root").child(Node::text("t", "a\tb")));
        let f = FocusModel::new();
        let out = render(&ctx(&t, &f, &Theme::default_theme())).unwrap();
        assert_eq!(out.row_text(0), "a b");
    }

    #[test]
    fn placeholder_and_fallback_renders() {
        let p = fallback::placeholder_tree("workbench", "reduce failed");
        let f = FocusModel::new();
        let out = render(&ctx(&p, &f, &Theme::default_theme())).unwrap();
        let body: String = (0..8).map(|r| out.row_text(r)).collect::<Vec<_>>().join("|");
        assert!(body.contains("UI component faulted"), "body: {body}");
        assert!(body.contains("reduce failed"), "body: {body}");

        let fb = fallback::FallbackUi::new("kernel render fault");
        let tree = fb.tree();
        let out = render(&ctx(&tree, &f, &Theme::default_theme())).unwrap();
        assert_eq!(out.row_text(0), "kanbei safe mode");
        // The fallback tree's input is an ordinary line: the kernel no longer
        // paints a "> " prompt on a reserved bottom row.
        let rows: Vec<String> = (0..out.rows()).map(|r| out.row_text(r)).collect();
        assert!(rows.iter().all(|r| !r.contains("> ")), "no kernel prompt: {rows:?}");
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
        let out = render(&ctx(&t, &f, &theme)).unwrap();
        let rows: Vec<String> = (0..10).map(|r| out.row_text(r)).collect();
        assert_eq!(rows.join("|"), "kanbei|first|second|hi||||||");
        assert_eq!(out.viewport_top, 0);
        assert_eq!(out.cell_style(3, 0), resolve_style(&theme, Some("input")));
        assert_eq!(
            out.cell_style(3, 1),
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
        let out = render(&ctx(&t, &f, &theme)).unwrap();
        // The caret (content offset 1) takes the re-themed style...
        assert_eq!(
            out.cell_style(3, 1),
            resolve_style(&theme, Some("selected"))
        );
        // ...and the lead cell keeps the input style.
        assert_eq!(out.cell_style(3, 0), resolve_style(&theme, Some("input")));
        assert_ne!(
            out.cell_style(3, 1),
            resolve_style(&Theme::default_theme(), Some("selected"))
        );
    }
}
