//! Render diffing: the kernel writes only changed cells (hot path, R-27).
//! `diff` compares two immutable frame snapshots; `apply` emits minimal ANSI
//! SGR sequences for the changed cells.

use std::io;

use crate::terminal::Terminal;
use crate::theme::Theme;
use crate::TerminalFrame;

/// One changed cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CellEdit {
    pub row: u16,
    pub col: u16,
    pub ch: char,
    pub style: String,
}

/// The set of cells that differ between two frames.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FrameDiff {
    pub edits: Vec<CellEdit>,
}

impl FrameDiff {
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }

    pub fn len(&self) -> usize {
        self.edits.len()
    }
}

/// Cells whose (char, style) differ from `prev` to `next`. Frames must share
/// dimensions; mismatched sizes produce an empty diff (the caller repaints).
pub fn diff(prev: &TerminalFrame, next: &TerminalFrame) -> FrameDiff {
    if prev.rows != next.rows || prev.cols != next.cols {
        return FrameDiff::default();
    }
    let mut edits = Vec::new();
    for (i, (a, b)) in prev.cells.iter().zip(next.cells.iter()).enumerate() {
        if a.ch != b.ch || a.style != b.style {
            edits.push(CellEdit {
                row: (i / next.cols as usize) as u16,
                col: (i % next.cols as usize) as u16,
                ch: b.ch,
                style: b.style.clone(),
            });
        }
    }
    FrameDiff { edits }
}

/// ANSI SGR prefix for a style key (empty = default).
fn sgr(theme: &Theme, style: &str) -> String {
    let s = theme.style(Some(style));
    let mut codes: Vec<&str> = Vec::new();
    codes.push(s.fg.ansi_fg());
    if s.bg != crate::Color::Default {
        codes.push(s.bg.ansi_bg());
    }
    if s.bold {
        codes.push("1");
    }
    if s.underline {
        codes.push("4");
    }
    if s.reverse {
        codes.push("7");
    }
    format!("\x1b[0m\x1b[{}m", codes.join(";"))
}

/// Paint a full frame (used on repaint and first paint).
pub fn paint_full(terminal: &mut dyn Terminal, frame: &TerminalFrame, theme: &Theme) -> io::Result<()> {
    terminal.write(b"\x1b[2J\x1b[H")?;
    let mut last_style = String::new();
    for (i, cell) in frame.cells.iter().enumerate() {
        if cell.is_blank() && cell.style == last_style {
            continue;
        }
        let row = (i / frame.cols as usize) as u16;
        let col = (i % frame.cols as usize) as u16;
        let mut out = format!("\x1b[{};{}H", row + 1, col + 1);
        if cell.style != last_style {
            out.push_str(&sgr(theme, &cell.style));
            last_style = cell.style.clone();
        }
        out.push(cell.ch);
        terminal.write(out.as_bytes())?;
    }
    terminal.write(b"\x1b[0m")?;
    terminal.flush()
}

/// Write only the changed cells of a diff.
pub fn apply(terminal: &mut dyn Terminal, diff: &FrameDiff, theme: &Theme) -> io::Result<()> {
    let mut last_style = String::new();
    for edit in &diff.edits {
        let mut out = format!("\x1b[{};{}H", edit.row + 1, edit.col + 1);
        if edit.style != last_style {
            out.push_str(&sgr(theme, &edit.style));
            last_style = edit.style.clone();
        }
        out.push(edit.ch);
        terminal.write(out.as_bytes())?;
    }
    if !diff.edits.is_empty() {
        terminal.write(b"\x1b[0m")?;
        terminal.flush()?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal::TestTerminal;

    fn frame(text: &str, rows: u16, cols: u16) -> TerminalFrame {
        let mut f = TerminalFrame::blank(rows, cols);
        f.write_line(0, text, "default", false);
        f
    }

    #[test]
    fn diff_finds_changed_cells() {
        let a = frame("hello", 3, 10);
        let mut b = frame("hello", 3, 10);
        b.set(0, 3, 'X', "default");
        let d = diff(&a, &b);
        assert_eq!(d.len(), 1);
        assert_eq!(d.edits[0], CellEdit { row: 0, col: 3, ch: 'X', style: "default".into() });
    }

    #[test]
    fn diff_ignores_style_vs_content_identity() {
        let a = frame("hi", 3, 10);
        let mut b = frame("hi", 3, 10);
        b.set(0, 0, 'h', "selected");
        let d = diff(&a, &b);
        assert_eq!(d.len(), 1);
        assert_eq!(d.edits[0].style, "selected");
    }

    #[test]
    fn diff_size_mismatch_is_empty() {
        let a = frame("hi", 3, 10);
        let b = frame("hi", 4, 10);
        assert!(diff(&a, &b).is_empty());
    }

    #[test]
    fn apply_writes_edits() {
        let a = frame("hello", 3, 10);
        let mut b = frame("hello", 3, 10);
        b.set(0, 0, 'H', "selected");
        b.set(0, 1, 'i', "default");
        let mut t = TestTerminal::new();
        let d = diff(&a, &b);
        apply(&mut t, &d, &Theme::default_theme()).unwrap();
        let text = String::from_utf8(t.bytes).unwrap();
        assert!(text.contains("\x1b[1;1H"));
        assert!(text.contains("\x1b[39;7m")); // reverse video for selected
        assert!(text.contains('H'));
        assert!(text.contains("\x1b[0m"));
    }

    #[test]
    fn paint_full_clears_first() {
        let f = frame("hello", 3, 10);
        let mut t = TestTerminal::new();
        paint_full(&mut t, &f, &Theme::default_theme()).unwrap();
        let text = String::from_utf8(t.bytes).unwrap();
        assert!(text.starts_with("\x1b[2J\x1b[H"));
        assert!(text.contains('h'));
    }
}
