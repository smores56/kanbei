//! End-to-end REPL tests: pipe lines into the built `kanbei` binary with
//! the `--fake` engine (no network, deterministic answer).

use std::io::Write;
use std::process::{Command, Stdio};

static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

fn temp_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kanbei-cli-test-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(args: &[&str], stdin: &str) -> (String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_kanbei"))
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn fake_repl_answers_and_exits() {
    let dir = temp_dir();
    let (stdout, stderr) = run(&[&dir.to_string_lossy(), "--fake"], "hello\n/exit\n");
    assert!(
        stdout.contains("kanbei ready"),
        "stdout: {stdout:?} stderr: {stderr:?}"
    );
    // The prompt line stays on stderr; stdout carries only the answer.
    assert!(!stdout.contains("you>"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fake_repl_status_shows_session() {
    let dir = temp_dir();
    let (stdout, stderr) = run(&[&dir.to_string_lossy(), "--fake"], "/status\n/exit\n");
    assert!(
        stderr.contains("next_seq"),
        "stderr: {stderr:?} stdout: {stdout:?}"
    );
    assert!(stdout.is_empty(), "stdout: {stdout:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fake_repl_eof_exits_cleanly() {
    let dir = temp_dir();
    let (stdout, _stderr) = run(&[&dir.to_string_lossy(), "--fake"], "one\n");
    assert!(stdout.contains("kanbei ready"), "stdout: {stdout:?}");
    let _ = std::fs::remove_dir_all(&dir);
}
