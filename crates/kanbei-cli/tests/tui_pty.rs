//! TUI integration test (C4): spawns the real `kanbei` binary on a
//! pseudo-terminal and drives one scripted turn end-to-end with the
//! `--fake` engine. The rendered byte stream is the only observable
//! surface of a full-screen TUI, so assertions target single-span rows
//! (each transcript row renders as one styled span, hence contiguous)
//! and a clean Ctrl-Q exit.

use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kanbei_ui::terminal::openpty;

const EXE: &str = env!("CARGO_BIN_EXE_kanbei");
const BOOT_TIMEOUT: Duration = Duration::from_secs(30);
const TURN_TIMEOUT: Duration = Duration::from_secs(60);

fn rendered(buf: &Arc<Mutex<Vec<u8>>>) -> String {
    String::from_utf8_lossy(&buf.lock().unwrap()).into_owned()
}

fn wait_for(buf: &Arc<Mutex<Vec<u8>>>, needle: &str, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    loop {
        if rendered(buf).contains(needle) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn send(master: &mut std::fs::File, bytes: &[u8]) {
    master.write_all(bytes).unwrap();
    master.flush().unwrap();
}

#[test]
fn tui_drives_a_fake_turn_and_exits_clean() {
    // pty pair with an explicit 24x80 winsize (a 0x0 winsize would render
    // an empty area — the child sizes off TIOCGWINSZ on its stdout).
    let (master, slave) = openpty().expect("openpty");
    rustix::termios::tcsetwinsize(
        &master,
        rustix::termios::Winsize {
            ws_row: 24,
            ws_col: 80,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )
    .expect("tcsetwinsize");

    // Fresh session dir (empty replay) for this run.
    let dir = std::env::temp_dir().join(format!("kanbei-tui-test-{}", std::process::id()));
    std::fs::remove_dir_all(&dir).ok();
    std::fs::create_dir_all(&dir).unwrap();

    // Second master handle for sending keystrokes to the child (dupped
    // before the reader thread takes `master`).
    let mut master_in = std::fs::File::from(
        master.as_fd().try_clone_to_owned().expect("dup master"),
    );

    // Reader thread: master → shared byte buffer (the rendered stream).
    let buf: Arc<Mutex<Vec<u8>>> = Arc::new(Mutex::new(Vec::new()));
    let buf_reader = Arc::clone(&buf);
    let reader = std::thread::spawn(move || {
        let mut master = std::fs::File::from(master);
        let mut chunk = [0u8; 8192];
        loop {
            match master.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(n) => buf_reader.lock().unwrap().extend_from_slice(&chunk[..n]),
            }
        }
    });

    // Spawn with the slave on stdin/stdout/stderr (the child's
    // stdin.is_terminal() selects the TUI path).
    let slave_out = std::fs::File::from(slave.as_fd().try_clone_to_owned().expect("dup slave"));
    let slave_err = std::fs::File::from(slave.as_fd().try_clone_to_owned().expect("dup slave"));
    let mut child = Command::new(EXE)
        .arg("--fake")
        .env("KANBEI_DIR", &dir)
        .stdin(slave)
        .stdout(slave_out)
        .stderr(slave_err)
        .spawn()
        .expect("spawn kanbei");

    // 1. The TUI comes up: the first frame carries the idle status bar.
    assert!(
        wait_for(&buf, "idle", BOOT_TIMEOUT),
        "TUI did not render an initial frame\n---\n{}",
        rendered(&buf)
    );

    // 2. Submit a prompt (Enter is \r on a raw pty).
    send(&mut master_in, b"hello world\r");

    // 3. The turn renders: the committed user line (one span), then the
    //    fake provider's answer (one span, first line starts at column 0).
    assert!(
        wait_for(&buf, "❯ hello world", TURN_TIMEOUT),
        "the user prompt did not render\n---\n{}",
        rendered(&buf)
    );
    assert!(
        wait_for(&buf, "kanbei ready (fake provider", TURN_TIMEOUT),
        "the fake response did not render\n---\n{}",
        rendered(&buf)
    );

    // 4. Ctrl-Q → clean exit (no active turn to cancel).
    send(&mut master_in, b"\x11");
    let deadline = Instant::now() + BOOT_TIMEOUT;
    let status = loop {
        match child.try_wait().expect("wait") {
            Some(s) => break s,
            None => {
                assert!(
                    Instant::now() < deadline,
                    "kanbei did not exit after Ctrl-Q\n---\n{}",
                    rendered(&buf)
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    assert!(status.success(), "exit status: {status}");

    std::fs::remove_dir_all(&dir).ok();
    drop(master_in); // EOF on the master → the reader thread exits.
    let _ = reader.join();
}
