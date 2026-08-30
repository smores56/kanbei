//! M7 workbench smoke test: the dogfooding binary over a real (piped)
//! stdin — bracketed paste, a mouse escape, Enter (submit), Ctrl-C (clean
//! exit). Asserts the canonical user_message is durable in the session log
//! and that the session reopens cleanly. Skips when the guest wasm is not
//! built (require_guest pattern, see m6.rs).

use std::io::{Read, Write};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use kanbei_core::envelope::Envelope;
use kanbei_core::id::Id128;
use kanbei_log::for_each_frame;
use kanbei_session::{Session, SessionConfig};
use kanbei_vm::{GuestError, Vm, VmConfig};

/// Module tests need the guest wasm; without it they skip with a note.
fn require_guest() -> bool {
    match Vm::load(VmConfig {
        fuel_per_call: u64::MAX,
        epoch_deadline: u64::MAX,
        ..Default::default()
    }) {
        Ok(_) => true,
        Err(GuestError::NotBuilt) => {
            eprintln!(
                "skip: guest wasm not built (run `cargo build -p kanbei-guest \
                 --target wasm32-wasip1 --release`)"
            );
            false
        }
        Err(e) => panic!("Vm::load failed: {e}"),
    }
}

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kanbei-wb-{tag}-{}-{}",
        std::process::id(),
        Id128::generate()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn workbench_stdin_e2e() {
    if !require_guest() {
        return;
    }
    let dir = tempdir("e2e");

    // Piped stdin: the binary must skip raw mode and read bytes directly.
    let mut child = Command::new(env!("CARGO_BIN_EXE_workbench"))
        .arg(&dir)
        .env("KANBEI_WB_FAKE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();

    // Feed like a real terminal: the paste burst, then the Enter keystroke
    // (the decoder defers the paste terminator's ESC, and the kernel drains
    // incomplete tails after every ui_handle_input — so everything after
    // `\x1b[201~` within one call is dropped; a terminal delivers them as
    // separate reads). First make sure the child is in its read loop by
    // polling for the UI-activation frame on disk.
    let log_path = dir.join("log.zst");
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if std::fs::metadata(&log_path)
            .map(|m| m.len() > 0)
            .unwrap_or(false)
        {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("workbench did not open its session log");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"\x1b[200~hello paste\x1b[201~")
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    // Mouse escape (its CSI prefix is dropped by the decoder), Enter
    // (submit), Ctrl-C (clean exit).
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"\x1b[<0;10;5M\r\x03")
        .unwrap();
    drop(child.stdin.take());

    let status = wait_exit(&mut child, Duration::from_secs(15));
    assert!(status.success(), "workbench must exit 0");

    let mut stderr = String::new();
    if let Some(mut err) = child.stderr.take() {
        err.read_to_string(&mut stderr).unwrap();
    }
    assert!(
        stderr.contains("workbench: "),
        "startup banner on stderr: {stderr:?}"
    );

    // The Enter submit must have committed the canonical user_message whose
    // text carries the pasted text (the mouse escape leaks its trailing
    // chars into the draft, so the committed text contains "hello paste").
    let mut found = false;
    for_each_frame(&dir.join("log.zst"), |frame| {
        for line in &frame.events {
            let Ok(env) = Envelope::from_line(line) else {
                continue;
            };
            if env.kind == "user_message"
                && env
                    .payload
                    .get("text")
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| t.contains("hello paste"))
            {
                found = true;
            }
        }
    })
    .unwrap();
    assert!(found, "user_message with the paste text must be committed");

    // The session reopens cleanly on the same dir.
    let reopened = Session::open(SessionConfig {
        dir: dir.clone(),
        ..Default::default()
    })
    .expect("session reopens cleanly");
    reopened.close().unwrap();

    std::fs::remove_dir_all(&dir).ok();
}

fn wait_exit(child: &mut Child, timeout: Duration) -> std::process::ExitStatus {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            return status;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("workbench did not exit within {}s", timeout.as_secs());
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}
