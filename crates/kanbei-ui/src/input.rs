//! Input decoding and sanitization (kernel-owned, consistency 13: protocol
//! parsing stays off the Luau/Wasm side). Raw terminal bytes decode into a
//! closed set of `InputEvent`s; C0 controls that are not recognized are
//! dropped, and invalid UTF-8 is dropped byte-wise. `UiEvent` carries kernel-
//! assigned provenance (`User` or `Module(gen)`, R-27).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputEvent {
    Char(char),
    Backspace,
    Enter,
    Tab,
    ShiftTab,
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    Home,
    End,
    PageUp,
    PageDown,
    Delete,
    CtrlC,
    CtrlL,
    CtrlX,
    /// Unrecognized input, dropped by sanitization.
    Drop,
}

/// Stateful decoder: escape sequences and multi-byte UTF-8 may split across
/// reads, so partial input is buffered until it completes.
#[derive(Debug, Clone, Default)]
pub struct InputDecoder {
    pending: Vec<u8>,
    in_paste: bool,
}

impl InputDecoder {
    pub fn new() -> Self {
        InputDecoder {
            pending: Vec::new(),
            in_paste: false,
        }
    }

    /// Feed raw bytes; returns the decoded events (order preserved).
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<InputEvent> {
        self.pending.extend_from_slice(bytes);
        let mut out = Vec::new();
        loop {
            match self.next_event() {
                Some(e) => out.push(e),
                None => break,
            }
        }
        out
    }

    /// Flush buffered partial input (e.g. at terminal close): incomplete
    /// sequences are dropped.
    pub fn finish(&mut self) -> Vec<InputEvent> {
        let leftover = std::mem::take(&mut self.pending);
        self.in_paste = false;
        if leftover.is_empty() {
            return Vec::new();
        }
        vec![InputEvent::Drop]
    }

    fn next_event(&mut self) -> Option<InputEvent> {
        let b = *self.pending.first()?;
        if self.in_paste {
            if b == 0x1b {
                // Possible paste terminator (ESC [ 201~) — hand back to the
                // escape parser.
                self.in_paste = false;
                return None;
            }
            return Some(match b {
                0x08 | 0x7f => {
                    self.pending.remove(0);
                    InputEvent::Backspace
                }
                0x0d | 0x0a => {
                    self.pending.remove(0);
                    InputEvent::Enter
                }
                c if c < 0x20 => {
                    self.pending.remove(0);
                    InputEvent::Drop
                }
                c => match self.take_char(c) {
                    CharRead::Complete(Some(ch)) => InputEvent::Char(ch),
                    CharRead::Complete(None) | CharRead::Incomplete => InputEvent::Drop,
                },
            });
        }
        match b {
            0x1b => self.escape(),
            0x08 | 0x7f => {
                self.pending.remove(0);
                Some(InputEvent::Backspace)
            }
            0x09 => {
                self.pending.remove(0);
                Some(InputEvent::Tab)
            }
            0x0d | 0x0a => {
                // CR and LF both submit (raw terminals deliver CR; tests and
                // pastes may deliver LF or CRLF — normalize).
                self.pending.remove(0);
                Some(InputEvent::Enter)
            }
            0x03 => {
                self.pending.remove(0);
                Some(InputEvent::CtrlC)
            }
            0x0c => {
                self.pending.remove(0);
                Some(InputEvent::CtrlL)
            }
            0x18 => {
                self.pending.remove(0);
                Some(InputEvent::CtrlX)
            }
            c if c < 0x20 => {
                self.pending.remove(0);
                Some(InputEvent::Drop)
            }
            _ => {
                let ch = self.take_char(b);
                match ch {
                    CharRead::Complete(Some(ch)) => Some(InputEvent::Char(ch)),
                    CharRead::Complete(None) => Some(InputEvent::Drop),
                    CharRead::Incomplete => None,
                }
            }
        }
    }

    /// Consume one UTF-8 char from the front. `Incomplete` waits for more
    /// bytes; `Complete(None)` means an invalid sequence was dropped.
    fn take_char(&mut self, first: u8) -> CharRead {
        if first < 0x80 {
            self.pending.remove(0);
            return CharRead::Complete(Some(first as char));
        }
        let len = match first {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => {
                self.pending.remove(0);
                return CharRead::Complete(None);
            }
        };
        if self.pending.len() < len {
            return CharRead::Incomplete;
        }
        let bytes = self.pending[..len].to_vec();
        self.pending.drain(..len);
        match std::str::from_utf8(&bytes) {
            Ok(s) => CharRead::Complete(s.chars().next()),
            Err(_) => CharRead::Complete(None),
        }
    }

    /// Escape sequence handling. Returns None while the sequence is
    /// incomplete.
    fn escape(&mut self) -> Option<InputEvent> {
        debug_assert_eq!(self.pending[0], 0x1b);
        if self.pending.len() == 1 {
            return None; // lone ESC: could be an Alt prefix we do not decode
        }
        match self.pending[1] {
            b'[' => {
                if self.pending.len() == 2 {
                    return None;
                }
                match self.pending[2] {
                    b'A' => {
                        self.pending.drain(..3);
                        Some(InputEvent::ArrowUp)
                    }
                    b'B' => {
                        self.pending.drain(..3);
                        Some(InputEvent::ArrowDown)
                    }
                    b'C' => {
                        self.pending.drain(..3);
                        Some(InputEvent::ArrowRight)
                    }
                    b'D' => {
                        self.pending.drain(..3);
                        Some(InputEvent::ArrowLeft)
                    }
                    b'H' => {
                        self.pending.drain(..3);
                        Some(InputEvent::Home)
                    }
                    b'F' => {
                        self.pending.drain(..3);
                        Some(InputEvent::End)
                    }
                    b'Z' => {
                        self.pending.drain(..3);
                        Some(InputEvent::ShiftTab)
                    }
                    b'0'..=b'9' => {
                        // CSI n~ : find the tilde terminator
                        if let Some(tilde) = self.pending.iter().position(|&c| c == b'~') {
                            let param: String = self.pending[2..tilde]
                                .iter()
                                .map(|&c| c as char)
                                .collect();
                            self.pending.drain(..=tilde);
                            let n: u32 = param.parse().unwrap_or(0);
                            match n {
                                3 => Some(InputEvent::Delete),
                                5 => Some(InputEvent::PageUp),
                                6 => Some(InputEvent::PageDown),
                                200 => {
                                    self.in_paste = true;
                                    Some(InputEvent::Drop) // bracketed paste start marker
                                }
                                201 => Some(InputEvent::Drop),
                                _ => Some(InputEvent::Drop),
                            }
                        } else {
                            None // digits without terminator yet
                        }
                    }
                    _ => {
                        // Unknown CSI: drop the first two bytes; the rest may
                        // be more input.
                        self.pending.drain(..2);
                        Some(InputEvent::Drop)
                    }
                }
            }
            _ => {
                // ESC followed by a non-[ byte: Alt-key or stray ESC. Not
                // decoded in MVP; drop both.
                self.pending.drain(..2);
                Some(InputEvent::Drop)
            }
        }
    }
}

impl InputEvent {
    /// Events that carry user intent to a module reducer. Everything else is
    /// kernel-handled (navigation, focus, reserved actions). Enter resolves
    /// against the focused node: a focused button activates, otherwise the
    /// input submits.
    pub fn to_ui(&self, focused_node: Option<&str>) -> Option<UiEventKind> {
        match self {
            InputEvent::Char(c) => Some(UiEventKind::Char(*c)),
            InputEvent::Backspace => Some(UiEventKind::Backspace),
            InputEvent::Enter => match focused_node {
                Some(id) => Some(UiEventKind::Activate(id.to_string())),
                None => Some(UiEventKind::Enter),
            },
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiProvenance {
    User,
    Module(u64),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiEventKind {
    Char(char),
    Backspace,
    Enter,
    /// The user activated the focused node (button).
    Activate(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UiEvent {
    pub provenance: UiProvenance,
    pub kind: UiEventKind,
}

/// Result of decoding one UTF-8 sequence from the input buffer.
enum CharRead {
    /// A complete sequence: `None` means invalid bytes were dropped.
    Complete(Option<char>),
    /// The sequence needs more bytes.
    Incomplete,
}

impl UiEvent {
    pub fn user(kind: UiEventKind) -> Self {
        UiEvent {
            provenance: UiProvenance::User,
            kind,
        }
    }

    pub fn module(generation: u64, kind: UiEventKind) -> Self {
        UiEvent {
            provenance: UiProvenance::Module(generation),
            kind,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn decode(bytes: &[u8]) -> Vec<InputEvent> {
        let mut d = InputDecoder::new();
        let mut out = d.feed(bytes);
        out.extend(d.finish());
        out
    }

    #[test]
    fn plain_chars() {
        assert_eq!(decode(b"hi"), vec![InputEvent::Char('h'), InputEvent::Char('i')]);
        assert_eq!(
            decode("héllo".as_bytes()),
            vec![
                InputEvent::Char('h'),
                InputEvent::Char('é'),
                InputEvent::Char('l'),
                InputEvent::Char('l'),
                InputEvent::Char('o'),
            ]
        );
    }

    #[test]
    fn controls() {
        assert_eq!(
            decode(b"\x03\x0c\x18\x0d\x0a\x08\x7f\x01"),
            vec![
                InputEvent::CtrlC,
                InputEvent::CtrlL,
                InputEvent::CtrlX,
                InputEvent::Enter,
                InputEvent::Enter,
                InputEvent::Backspace,
                InputEvent::Backspace,
                InputEvent::Drop,
            ]
        );
    }

    #[test]
    fn escapes() {
        assert_eq!(
            decode(b"\x1b[A\x1b[B\x1b[C\x1b[D\x1b[H\x1b[F\x1b[Z\x1b[3~\x1b[5~\x1b[6~"),
            vec![
                InputEvent::ArrowUp,
                InputEvent::ArrowDown,
                InputEvent::ArrowRight,
                InputEvent::ArrowLeft,
                InputEvent::Home,
                InputEvent::End,
                InputEvent::ShiftTab,
                InputEvent::Delete,
                InputEvent::PageUp,
                InputEvent::PageDown,
            ]
        );
    }

    #[test]
    fn split_across_reads() {
        let mut d = InputDecoder::new();
        assert!(d.feed(b"\x1b[").is_empty());
        assert_eq!(d.feed(b"A"), vec![InputEvent::ArrowUp]);
        assert!(d.feed(&"é".as_bytes()[..1]).is_empty());
        assert_eq!(d.feed(&"é".as_bytes()[1..]), vec![InputEvent::Char('é')]);
    }

    #[test]
    fn unknown_sequences_dropped() {
        assert_eq!(decode(b"\x1b[9~\x1b[999~\x1bX"), vec![InputEvent::Drop, InputEvent::Drop, InputEvent::Drop]);
        assert_eq!(decode(b"\x1b"), vec![InputEvent::Drop]);
        assert_eq!(decode(b"\xff\xfe"), vec![InputEvent::Drop, InputEvent::Drop]);
    }

    #[test]
    fn bracketed_paste() {
        let mut d = InputDecoder::new();
        let mut out = d.feed(b"\x1b[200~hi there\x1b[201~");
        out.extend(d.finish());
        // paste markers dropped; interior text arrives as chars
        assert_eq!(
            out,
            vec![
                InputEvent::Drop,
                InputEvent::Char('h'),
                InputEvent::Char('i'),
                InputEvent::Char(' '),
                InputEvent::Char('t'),
                InputEvent::Char('h'),
                InputEvent::Char('e'),
                InputEvent::Char('r'),
                InputEvent::Char('e'),
                InputEvent::Drop,
            ]
        );
    }

    #[test]
    fn to_ui_mapping() {
        assert_eq!(
            InputEvent::Char('x').to_ui(None),
            Some(UiEventKind::Char('x'))
        );
        assert_eq!(InputEvent::Backspace.to_ui(None), Some(UiEventKind::Backspace));
        assert_eq!(InputEvent::ArrowUp.to_ui(None), None);
        assert_eq!(
            InputEvent::Enter.to_ui(Some("btn")),
            Some(UiEventKind::Activate("btn".into()))
        );
        assert_eq!(InputEvent::Enter.to_ui(None), Some(UiEventKind::Enter));
    }
}
