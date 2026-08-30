//! The kernel terminal boundary (R-27): initialization, raw mode,
//! restoration, size, and output. Rust owns terminal init/restore; the
//! [`TerminalGuard`] makes restoration reliable even on panic/crash paths.
//!
//! The boundary is an owned fd (never the process stdin/stdout by default —
//! callers pass the fd explicitly, which keeps tests hermetic via
//! [`openpty`]).

use std::io;
use std::os::fd::{AsFd, BorrowedFd, OwnedFd};

use rustix::fs::{Mode, OFlags};
use rustix::io as rio;
use rustix::pty::{OpenptFlags, openpt, ptsname, unlockpt};
use rustix::termios::{OptionalActions, Termios, isatty, tcgetattr, tcgetwinsize, tcsetattr};

/// The kernel's terminal abstraction. Implementations are hot-path pieces:
/// cell writes are buffered by the caller's diff/paint paths and flushed by
/// [`Terminal::flush`].
pub trait Terminal {
    fn size(&mut self) -> io::Result<(u16, u16)>;
    fn write(&mut self, bytes: &[u8]) -> io::Result<()>;
    fn flush(&mut self) -> io::Result<()>;
}

/// Real termios-backed terminal over an owned fd. When the fd is not a tty
/// (e.g. a pipe), raw-mode operations become no-ops and size falls back to
/// 24x80 — the boundary stays usable in tests and non-interactive runs.
pub struct TermiosTerminal {
    fd: OwnedFd,
    saved: Option<Termios>,
    raw: bool,
}

impl TermiosTerminal {
    /// Take ownership of a terminal fd, saving its current termios state
    /// (when it is a tty) for later restoration.
    pub fn open(fd: OwnedFd) -> io::Result<Self> {
        let saved = if isatty(&fd) {
            Some(tcgetattr(&fd)?)
        } else {
            None
        };
        Ok(TermiosTerminal {
            fd,
            saved,
            raw: false,
        })
    }

    pub fn is_raw(&self) -> bool {
        self.raw
    }

    /// Enter raw mode (cfmakeraw): input byte-at-a-time, no echo, no
    /// signal-generating keys (except those the kernel decodes itself).
    pub fn enter_raw(&mut self) -> io::Result<()> {
        if self.raw {
            return Ok(());
        }
        if let Some(saved) = &self.saved {
            let mut raw = saved.clone();
            raw.make_raw();
            tcsetattr(&self.fd, OptionalActions::Now, &raw)?;
        }
        self.raw = true;
        Ok(())
    }

    /// Restore the saved termios state. Safe to call repeatedly; no-op when
    /// never entered raw mode.
    pub fn restore(&mut self) -> io::Result<()> {
        if !self.raw {
            return Ok(());
        }
        if let Some(saved) = &self.saved {
            tcsetattr(&self.fd, OptionalActions::Now, saved)?;
        }
        self.raw = false;
        Ok(())
    }

    pub fn fd(&self) -> BorrowedFd<'_> {
        self.fd.as_fd()
    }
}

impl Terminal for TermiosTerminal {
    fn size(&mut self) -> io::Result<(u16, u16)> {
        if isatty(&self.fd) {
            let ws = tcgetwinsize(&self.fd)?;
            Ok((ws.ws_row, ws.ws_col))
        } else {
            Ok((24, 80))
        }
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut rest = bytes;
        while !rest.is_empty() {
            let n = rio::write(&self.fd, rest)?;
            rest = &rest[n..];
        }
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        // Raw mode is unbuffered at the fd level; nothing to flush.
        Ok(())
    }
}

impl Drop for TermiosTerminal {
    fn drop(&mut self) {
        // Best-effort restoration on any drop path (R-27: terminal
        // restoration remains reliable).
        if self.raw {
            let _ = self.restore();
        }
    }
}

/// RAII raw-mode guard: restores the terminal on drop, including unwinds.
/// Explicit [`TerminalGuard::disarm`] keeps the raw mode when the caller
/// hands control elsewhere.
pub struct TerminalGuard<'a> {
    term: &'a mut TermiosTerminal,
    armed: bool,
}

impl<'a> TerminalGuard<'a> {
    /// Enter raw mode and arm restoration.
    pub fn new(term: &'a mut TermiosTerminal) -> io::Result<Self> {
        term.enter_raw()?;
        Ok(TerminalGuard { term, armed: true })
    }

    pub fn disarm(mut self) {
        self.armed = false;
    }
}

impl Drop for TerminalGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = self.term.restore();
        }
    }
}

/// Open a fresh pseudo-terminal pair `(master, slave)` for hermetic terminal
/// tests.
pub fn openpty() -> io::Result<(OwnedFd, OwnedFd)> {
    let master = openpt(OpenptFlags::RDWR | OpenptFlags::NOCTTY)?;
    let name = ptsname(&master, Vec::new())?;
    unlockpt(&master)?;
    let slave = rustix::fs::open(&name, OFlags::RDWR | OFlags::NOCTTY, Mode::empty())?;
    Ok((master, slave))
}

/// In-memory terminal recording writes (tests; also serves as the diff
/// write-counter for hot-path assertions).
#[derive(Debug, Default)]
pub struct TestTerminal {
    pub bytes: Vec<u8>,
    pub writes: usize,
    pub fail_after: Option<usize>,
}

impl TestTerminal {
    pub fn new() -> Self {
        TestTerminal::default()
    }

    /// A terminal that fails every write after `n` successful ones (kernel
    /// render-fault tests).
    pub fn failing_after(n: usize) -> Self {
        TestTerminal {
            bytes: Vec::new(),
            writes: 0,
            fail_after: Some(n),
        }
    }
}

impl Terminal for TestTerminal {
    fn size(&mut self) -> io::Result<(u16, u16)> {
        Ok((24, 80))
    }

    fn write(&mut self, bytes: &[u8]) -> io::Result<()> {
        if let Some(n) = self.fail_after
            && self.writes >= n
        {
            return Err(io::Error::new(io::ErrorKind::BrokenPipe, "injected write failure"));
        }
        self.writes += 1;
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Raw-mode assertions need a real pty: helpers over any fd-backed handle.
pub fn raw_state_of<F: AsFd>(fd: F) -> io::Result<Termios> {
    Ok(tcgetattr(fd)?)
}

pub fn is_raw_mode<F: AsFd>(fd: F) -> io::Result<bool> {
    let t = tcgetattr(fd)?;
    // cfmakeraw clears ICANON and ECHO; check both via the raw-mode recipe
    // used by make_raw.
    let raw = {
        let mut r = t.clone();
        r.make_raw();
        r
    };
    Ok(t.input_modes == raw.input_modes && t.local_modes == raw.local_modes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dup(fd: &OwnedFd) -> OwnedFd {
        rustix::io::dup(fd).unwrap()
    }

    #[test]
    fn raw_mode_and_restore_round_trip() {
        let (_master, slave) = openpty().unwrap();
        let probe = dup(&slave);
        let saved = raw_state_of(&probe).unwrap();
        assert!(!is_raw_mode(&probe).unwrap());
        let mut term = TermiosTerminal::open(slave).unwrap();
        {
            let guard = TerminalGuard::new(&mut term).unwrap();
            assert!(is_raw_mode(&probe).unwrap());
            guard.disarm();
        }
        // disarmed: still raw after guard drop
        assert!(term.is_raw());
        assert!(is_raw_mode(&probe).unwrap());
        term.restore().unwrap();
        assert!(!term.is_raw());
        let after = raw_state_of(&probe).unwrap();
        assert_eq!(after.local_modes, saved.local_modes);
    }

    #[test]
    fn guard_restores_on_drop() {
        let (_master, slave) = openpty().unwrap();
        let probe = dup(&slave);
        let mut term = TermiosTerminal::open(slave).unwrap();
        {
            let _guard = TerminalGuard::new(&mut term).unwrap();
            assert!(is_raw_mode(&probe).unwrap());
        }
        assert!(!term.is_raw());
        assert!(!is_raw_mode(&probe).unwrap());
    }

    #[test]
    fn double_restore_is_safe() {
        let (_master, slave) = openpty().unwrap();
        let mut term = TermiosTerminal::open(slave).unwrap();
        term.enter_raw().unwrap();
        term.restore().unwrap();
        term.restore().unwrap();
        assert!(!term.is_raw());
    }

    #[test]
    fn non_tty_fd_is_noop_raw() {
        let (r, _w) = rustix::pipe::pipe().unwrap();
        let mut term = TermiosTerminal::open(r).unwrap();
        term.enter_raw().unwrap();
        assert!(term.is_raw());
        term.restore().unwrap();
        assert!(!term.is_raw());
        assert_eq!(term.size().unwrap(), (24, 80));
    }

    #[test]
    fn pty_write_read_round_trip() {
        let (master, slave) = openpty().unwrap();
        let mut term = TermiosTerminal::open(slave).unwrap();
        term.enter_raw().unwrap();
        term.write(b"hello").unwrap();
        term.flush().unwrap();
        let mut buf = [0u8; 8];
        let n = rio::read(&master, &mut buf).unwrap();
        assert_eq!(&buf[..n], b"hello");
        term.restore().unwrap();
    }
}
