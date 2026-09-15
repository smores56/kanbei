//! End-to-end REPL tests: pipe lines into the built `kanbei` binary with a
//! project `.kanbei/init.lua` selecting the config-driven fake engine (no
//! network, deterministic answer). The shipped binary has no provider or
//! approval argv flags any more — config layers drive the wiring (decision
//! 28), so these tests are also the regression guard that config, not argv,
//! drives the session.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A root-scope Luau config layer selecting the scripted fake provider.
const FAKE_CONFIG: &str = r#"-- project config: scripted fake provider (no network).
function kb_on_activate(ctx)
  ctx.contribution_publish('{"kind":"settings","provider":{"fake":true}}')
end
function kb_hot(dispatch)
  return dispatch
end
"#;

fn temp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kanbei-cli-{tag}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn write_fake_config(dir: &Path) {
    let config_dir = dir.join(".kanbei");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("init.lua"), FAKE_CONFIG).unwrap();
}

/// Spawn the binary with the process-level provider env cleared and an isolated
/// empty `XDG_CONFIG_HOME`, so only the project config layer is in play.
fn command(args: &[&str], stdin: &str) -> std::process::Child {
    let xdg = temp_dir("xdg");
    let mut child = Command::new(env!("CARGO_BIN_EXE_kanbei"))
        .args(args)
        .env("XDG_CONFIG_HOME", xdg)
        .env_remove("KANBEI_PROVIDER_URL")
        .env_remove("KANBEI_PROVIDER_KEY")
        .env_remove("KANBEI_PROVIDER_MODEL")
        .env_remove("KANBEI_YOLO")
        .env_remove("KANBEI_DIR")
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
    child
}

fn run(args: &[&str], stdin: &str) -> (String, String) {
    let out = command(args, stdin).wait_with_output().unwrap();
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

#[test]
fn config_driven_fake_repl_answers_and_exits() {
    let dir = temp_dir("repl");
    write_fake_config(&dir);
    let (stdout, stderr) = run(&[&dir.to_string_lossy()], "hello\n/exit\n");
    assert!(
        stdout.contains("kanbei ready"),
        "config-driven fake engine did not answer: stdout: {stdout:?} stderr: {stderr:?}"
    );
    // The prompt line stays on stderr; stdout carries only the answer.
    assert!(!stdout.contains("you>"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn config_driven_fake_status_shows_session() {
    let dir = temp_dir("status");
    write_fake_config(&dir);
    let (stdout, stderr) = run(&[&dir.to_string_lossy()], "/status\n/exit\n");
    assert!(
        stderr.contains("next_seq"),
        "stderr: {stderr:?} stdout: {stdout:?}"
    );
    assert!(stdout.is_empty(), "stdout: {stdout:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn config_driven_fake_eof_exits_cleanly() {
    let dir = temp_dir("eof");
    write_fake_config(&dir);
    let (stdout, _stderr) = run(&[&dir.to_string_lossy()], "one\n");
    assert!(stdout.contains("kanbei ready"), "stdout: {stdout:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// The removed provider/approval flags are rejected by argv parsing.
#[test]
fn removed_flags_are_rejected() {
    let dir = temp_dir("reject");
    let out = command(&[&dir.to_string_lossy(), "--fake"], "").wait_with_output();
    let out = out.unwrap();
    assert_eq!(out.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("unknown argument: --fake"),
        "stderr: {stderr:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
