//! M7 dogfooding workbench: the builtin workbench UI over a real terminal.
//! Opens a session at `dir` (argv[1], else `KANBEI_WB_DIR`, else "."),
//! activates the builtin UI, then wires stdin into the kernel input
//! boundary: bytes are forwarded verbatim to `ui_handle_input` (the
//! InputDecoder inside kanbei-ui handles escapes, bracketed paste, mouse,
//! CR/LF), and each batch is rendered and presented. Ctrl-C (0x03) and EOF
//! exit cleanly; the raw-mode guard restores the terminal and the session
//! is closed (flushed) before exit. No cognition loop — this binary
//! dogfoods the input path only.
//!
//! Provider selection: `KANBEI_WB_FAKE=1` injects a scripted FakeEngine
//! (one response, "workbench ready"); otherwise an HttpEngine is built from
//! `KANBEI_PROVIDER_URL` / `KANBEI_PROVIDER_KEY` / `KANBEI_PROVIDER_MODEL`.
//! `fs_root` is the session dir.

use std::io::{IsTerminal, Read};
use std::os::fd::AsFd;
use std::path::PathBuf;
use std::process::exit;
use std::time::Duration;

use kanbei_provider::{
    CompletionResponse, FakeEngine, FinishReason, KeySource, ProviderConfig, Usage,
};
use kanbei_session::{Session, SessionConfig};
use kanbei_ui::terminal::{TerminalGuard, TermiosTerminal};
use kanbei_vm::VmConfig;

fn main() {
    let dir = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("KANBEI_WB_DIR").ok())
        .unwrap_or_else(|| ".".into());
    eprintln!("workbench: {dir}");
    let dir = PathBuf::from(dir);

    let provider_engine: Box<dyn kanbei_provider::ProviderEngine> =
        if matches!(std::env::var("KANBEI_WB_FAKE").as_deref(), Ok("1")) {
            Box::new(FakeEngine::new(
                ProviderConfig {
                    provider: "fake".into(),
                    model: "workbench".into(),
                    base_url: "http://localhost:0/v1".into(),
                    key: KeySource::Env("KANBEI_PROVIDER_KEY".into()),
                    temperature: None,
                    max_tokens: None,
                    timeout: Duration::from_secs(5),
                },
                vec![CompletionResponse {
                    content: Some("workbench ready".into()),
                    tool_calls: Vec::new(),
                    finish_reason: FinishReason::Stop,
                    usage: Usage {
                        input_tokens: 0,
                        output_tokens: 0,
                    },
                    discontinuity: None,
                    opaque_artifacts: None,
                }],
            ))
        } else {
            Box::new(kanbei_provider::HttpEngine::new(ProviderConfig {
                provider: "http".into(),
                model: std::env::var("KANBEI_PROVIDER_MODEL").unwrap_or_else(|_| "default".into()),
                base_url: std::env::var("KANBEI_PROVIDER_URL")
                    .unwrap_or_else(|_| "http://localhost:0/v1".into()),
                key: KeySource::Env("KANBEI_PROVIDER_KEY".into()),
                temperature: None,
                max_tokens: None,
                timeout: Duration::from_secs(60),
            }))
        };

    // The builtin UI module needs fuel beyond the default 1M per call (M2
    // recipe; same engine config as the M5 gate).
    let mut session = match Session::open(SessionConfig {
        dir: dir.clone(),
        stream: "workbench".into(),
        engine: Some(VmConfig {
            fuel_per_call: u64::MAX,
            epoch_deadline: u64::MAX / 2,
            ..Default::default()
        }),
        provider_engine: Some(provider_engine),
        fs_root: dir.clone(),
        ..Default::default()
    }) {
        Ok(session) => session,
        Err(e) => {
            eprintln!("workbench: session open failed: {e}");
            exit(2);
        }
    };
    if session.modules().is_some() {
        if let Err(e) = session.activate_builtin_ui() {
            eprintln!("workbench: activate_builtin_ui failed: {e}");
        }
    } else {
        eprintln!("workbench: warning: guest wasm not built; UI unavailable");
    }
    if session.ui().is_none() {
        eprintln!("workbench: warning: no UI host bound");
    }

    // Raw mode only when stdin is a tty (a piped stdin — the smoke test —
    // skips raw mode and reads bytes directly). The raw-mode guard borrows
    // one terminal handle for the whole run; frames are presented through a
    // second handle over the same fd (raw mode is device-wide, so both see
    // the same termios state). Without a terminal the input loop still runs
    // with presentation skipped.
    let mut raw_term: Option<TermiosTerminal> = None;
    let mut present_term: Option<TermiosTerminal> = None;
    if let Some((raw, present)) = open_terminal() {
        raw_term = Some(raw);
        present_term = Some(present);
    }
    let _guard = raw_term.as_mut().and_then(|t| match TerminalGuard::new(t) {
        Ok(guard) => Some(guard),
        Err(e) => {
            eprintln!("workbench: raw mode failed: {e}");
            None
        }
    });

    // Chunked reads, not byte-at-a-time: the kernel calls the decoder's
    // `finish()` after every `ui_handle_input`, which drains incomplete
    // escape sequences — a sequence must arrive within one call (gate_m5
    // feeds whole sequences the same way). 4 KiB keeps CSI/paste markers
    // whole; the decoder buffers content between calls.
    let mut buf = [0u8; 4096];
    loop {
        let n = match std::io::stdin().read(&mut buf) {
            Ok(0) => break, // EOF
            Ok(n) => n,
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => {
                eprintln!("workbench: stdin read failed: {e}");
                break;
            }
        };
        let chunk = &buf[..n];
        if let Err(e) = session.ui_handle_input(chunk) {
            eprintln!("workbench: ui_handle_input failed: {e}");
            break;
        }
        if chunk.contains(&0x03) {
            break; // Ctrl-C: exit cleanly, the guard restores the terminal
        }
        if let Err(e) = session.ui_render_frame() {
            eprintln!("workbench: ui_render_frame failed: {e}");
            break;
        }
        if let Some(present) = present_term.as_mut()
            && let Err(e) = session.ui_present(present)
        {
            eprintln!("workbench: ui_present failed: {e}");
        }
    }
    drop(_guard);
    if let Err(e) = session.close() {
        eprintln!("workbench: session close failed: {e}");
        exit(1);
    }
}

/// Two independent handles over the terminal: stdin's fd when it is a tty
/// (raw-mode path), stdout's fd otherwise (writes land in the pipe; size
/// 24x80). Returns None when the fds cannot be duplicated.
fn open_terminal() -> Option<(TermiosTerminal, TermiosTerminal)> {
    let (raw_fd, present_fd) = if std::io::stdin().is_terminal() {
        let stdin = std::io::stdin();
        (
            stdin.as_fd().try_clone_to_owned().ok()?,
            stdin.as_fd().try_clone_to_owned().ok()?,
        )
    } else {
        let stdout = std::io::stdout();
        (
            stdout.as_fd().try_clone_to_owned().ok()?,
            stdout.as_fd().try_clone_to_owned().ok()?,
        )
    };
    Some((
        TermiosTerminal::open(raw_fd).ok()?,
        TermiosTerminal::open(present_fd).ok()?,
    ))
}
