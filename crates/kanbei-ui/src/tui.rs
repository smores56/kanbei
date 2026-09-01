//! The ratatui adapter (R-27 substrate amendment): renders the kernel
//! contract — `SemanticTree` + `Theme` + reserved keys — with ratatui
//! widgets instead of the custom cell grid. The contract and invariants
//! survive the swap: the module produces tree data, the kernel renders; the
//! reserved-key table is unchanged; input sanitization maps raw terminal
//! events into the closed `InputEvent` set and drops the rest; the theme
//! resolves style names, never trusts unknowns.

use ratatui::style::{Color as RColor, Modifier, Style as RStyle};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::frame::{wrap, BodyLine};
use crate::input::InputEvent;
use crate::theme::{Color, Style, Theme};

/// Map a theme color to its ratatui counterpart.
pub fn to_rcolor(c: Color) -> RColor {
    match c {
        Color::Default => RColor::Reset,
        Color::Black => RColor::Black,
        Color::Red => RColor::Red,
        Color::Green => RColor::Green,
        Color::Yellow => RColor::Yellow,
        Color::Blue => RColor::Blue,
        Color::Magenta => RColor::Magenta,
        Color::Cyan => RColor::Cyan,
        Color::White => RColor::White,
        Color::BrightBlack => RColor::Gray,
        Color::BrightRed => RColor::LightRed,
        Color::BrightGreen => RColor::LightGreen,
        Color::BrightYellow => RColor::LightYellow,
        Color::BrightBlue => RColor::LightBlue,
        Color::BrightMagenta => RColor::LightMagenta,
        Color::BrightCyan => RColor::LightCyan,
        Color::BrightWhite => RColor::White,
    }
}

/// Resolve a named style key (theme vocabulary) to a ratatui style. Unknown
/// or empty keys resolve to the default style (theme contract).
pub fn resolve_style(theme: &Theme, name: Option<&str>) -> RStyle {
    let s: &Style = theme.style(name);
    let mut out = RStyle::default();
    if s.fg != Color::Default {
        out = out.fg(to_rcolor(s.fg));
    }
    if s.bg != Color::Default {
        out = out.bg(to_rcolor(s.bg));
    }
    if s.bold {
        out = out.add_modifier(Modifier::BOLD);
    }
    if s.underline {
        out = out.add_modifier(Modifier::UNDERLINED);
    }
    if s.reverse {
        out = out.add_modifier(Modifier::REVERSED);
    }
    out
}

/// One visible transcript row: one wrapped line of one body item, with its
/// theme-resolved style. Rows are pre-wrapped to the viewport width, so one
/// row always occupies exactly one terminal row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StyledRow {
    /// Index of the body line (transcript item) this row renders — the hit
    /// identity for click-to-toggle.
    pub item: usize,
    pub text: String,
    pub style: Option<String>,
}

/// Build the transcript viewport over the body lines: each line is
/// char-wrapped to `width` (the `frame::wrap` contract — plain wrapped
/// text, no markdown in v1), starting at item `top` (the scroll offset, in
/// item space) for at most `height` visible rows. Returns the visible rows
/// plus a row→item hit map (same length as the rows) for mouse
/// click-to-toggle.
pub fn build_viewport(
    lines: &[BodyLine<'_>],
    top: usize,
    height: usize,
    width: usize,
) -> (Vec<StyledRow>, Vec<usize>) {
    let mut rows: Vec<StyledRow> = Vec::new();
    for (i, line) in lines.iter().enumerate() {
        if i < top {
            continue;
        }
        if rows.len() >= height {
            break;
        }
        for text in wrap(&line.text, width) {
            if rows.len() >= height {
                break;
            }
            rows.push(StyledRow {
                item: i,
                text,
                style: Some(line.style.clone()),
            });
        }
    }
    let map = rows.iter().map(|r| r.item).collect();
    (rows, map)
}

/// The total rendered row count of the body lines at `width` (wraps each
/// line) — the transcript's height in rows, used for bottom-pinned scroll.
pub fn total_rows(lines: &[BodyLine<'_>], width: usize) -> usize {
    lines.iter().map(|l| wrap(&l.text, width).len()).sum()
}

/// One line of a rendered transcript row, styled through the theme.
pub fn row_to_line(row: &StyledRow, theme: &Theme) -> Line<'static> {
    Line::from(Span::styled(row.text.clone(), resolve_style(theme, row.style.as_deref())))
}

/// Render the visible transcript rows as a stateless paragraph. The caller
/// positions it (layout); the hit map from [`build_viewport`] covers the
/// same rows, so a click at row `y` maps to `hit[y]`.
pub fn transcript_paragraph<'a>(rows: &'a [StyledRow], theme: &'a Theme) -> Paragraph<'a> {
    let lines: Vec<Line> = rows
        .iter()
        .map(|r| {
            Line::from(vec![Span::styled(
                r.text.clone(),
                resolve_style(theme, r.style.as_deref()),
            )])
        })
        .collect();
    Paragraph::new(ratatui::text::Text::from(lines))
}

/// Map a decoded terminal key event to the closed `InputEvent` set (input
/// sanitization contract: only mapped events survive; everything else is
/// dropped). Control combinations follow the reserved table (Ctrl-C/L/X);
/// `Ctrl-Q` quits (UI level) and lone `Escape` returns to input focus (UI
/// level) — both are UI events, not kernel-reserved.
pub fn key_to_input(k: &crossterm::event::KeyEvent) -> Option<InputEvent> {
    use crossterm::event::{KeyCode, KeyEventKind, KeyModifiers};
    if k.kind != KeyEventKind::Press {
        return None;
    }
    let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
    match k.code {
        KeyCode::Char(c) if ctrl => Some(match c {
            'c' => InputEvent::CtrlC,
            'l' => InputEvent::CtrlL,
            'x' => InputEvent::CtrlX,
            'q' => InputEvent::CtrlQ,
            _ => InputEvent::Drop,
        }),
        KeyCode::Char(c) if c.is_control() => Some(InputEvent::Drop),
        KeyCode::Char(c) => Some(InputEvent::Char(c)),
        KeyCode::Backspace => Some(InputEvent::Backspace),
        KeyCode::Enter => Some(InputEvent::Enter),
        KeyCode::Tab => Some(InputEvent::Tab),
        KeyCode::BackTab => Some(InputEvent::ShiftTab),
        KeyCode::Up => Some(InputEvent::ArrowUp),
        KeyCode::Down => Some(InputEvent::ArrowDown),
        KeyCode::Left => Some(InputEvent::ArrowLeft),
        KeyCode::Right => Some(InputEvent::ArrowRight),
        KeyCode::Home => Some(InputEvent::Home),
        KeyCode::End => Some(InputEvent::End),
        KeyCode::PageUp => Some(InputEvent::PageUp),
        KeyCode::PageDown => Some(InputEvent::PageDown),
        KeyCode::Delete => Some(InputEvent::Delete),
        KeyCode::Esc => Some(InputEvent::Escape),
        _ => None, // unmapped keys (F-keys, alt-chords, ...) are dropped
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::collect_lines;
    use crate::tree::{Node, NodeKind, SemanticTree};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, KeyEventKind};

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::CONTROL)
    }

    #[test]
    fn style_mapping_and_unknown_key_fallback() {
        let theme = Theme::default_theme();
        let st = resolve_style(&theme, Some("user"));
        assert_eq!(st.fg, Some(RColor::LightCyan));
        assert!(st.add_modifier.contains(Modifier::BOLD));
        // unknown keys resolve to the default style (no color forced)
        let st = resolve_style(&theme, Some("nope"));
        assert_eq!(st.fg, None);
        let st = resolve_style(&theme, None);
        assert_eq!(st.fg, None);
    }

    #[test]
    fn viewport_windows_and_hits() {
        let tree = SemanticTree::new(
            Node::new("root", NodeKind::Root)
                .child(Node::new("a", NodeKind::User).with_content("aa"))
                .child(Node::new("b", NodeKind::Response).with_content("bbbb"))
                .child(Node::new("c", NodeKind::User).with_content("cc")),
        );
        let mut lines = Vec::new();
        collect_lines(&tree.root, &mut lines);
        let (rows, hits) = build_viewport(&lines, 1, 2, 80);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].text, "bbbb");
        assert_eq!(rows[1].text, "cc");
        assert_eq!(hits, vec![1, 2]);
    }

    #[test]
    fn viewport_wraps_to_width() {
        let tree = SemanticTree::new(
            Node::new("root", NodeKind::Root)
                .child(Node::new("a", NodeKind::Response).with_content("abcdefghij")),
        );
        let mut lines = Vec::new();
        collect_lines(&tree.root, &mut lines);
        let (rows, _) = build_viewport(&lines, 0, 10, 4);
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[0].text, "abcd");
        assert_eq!(rows[1].text, "efgh");
        assert_eq!(rows[2].text, "ij");
    }

    #[test]
    fn key_mapping_reserved_and_dropped() {
        assert_eq!(key_to_input(&ctrl(KeyCode::Char('c'))), Some(InputEvent::CtrlC));
        assert_eq!(key_to_input(&ctrl(KeyCode::Char('l'))), Some(InputEvent::CtrlL));
        assert_eq!(key_to_input(&ctrl(KeyCode::Char('x'))), Some(InputEvent::CtrlX));
        assert_eq!(key_to_input(&ctrl(KeyCode::Char('q'))), Some(InputEvent::CtrlQ));
        assert_eq!(key_to_input(&ctrl(KeyCode::Char('z'))), Some(InputEvent::Drop));
        assert_eq!(key_to_input(&key(KeyCode::Char('h'))), Some(InputEvent::Char('h')));
        assert_eq!(key_to_input(&key(KeyCode::Enter)), Some(InputEvent::Enter));
        assert_eq!(key_to_input(&key(KeyCode::Esc)), Some(InputEvent::Escape));
        assert_eq!(key_to_input(&key(KeyCode::Backspace)), Some(InputEvent::Backspace));
        assert_eq!(key_to_input(&key(KeyCode::Up)), Some(InputEvent::ArrowUp));
        // release events and unmapped keys are dropped
        let mut release = key(KeyCode::Char('x'));
        release.kind = KeyEventKind::Release;
        assert_eq!(key_to_input(&release), None);
        assert_eq!(key_to_input(&key(KeyCode::F(1))), None);
    }
}
