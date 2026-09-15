//! End-to-end REPL tests: pipe lines into the built `kanbei` binary with a
//! trusted USER config `init.lua` selecting the config-driven fake engine (no
//! network, deterministic answer). The shipped binary has no provider or
//! approval argv flags any more — config layers drive the wiring (decision
//! 28), so these tests are also the regression guard that config, not argv,
//! drives the session. `provider.fake` is a SENSITIVE field (A): only the
//! trusted `$XDG_CONFIG_HOME` layer may select it, never a cloned project.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// A root-scope Luau config layer selecting the scripted fake provider.
const FAKE_CONFIG: &str = r#"-- user config: scripted fake provider (no network).
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

/// Spawn the binary with the process-level provider env cleared and an isolated
/// `XDG_CONFIG_HOME` seeded with a TRUSTED user fake layer (A: an untrusted
/// project layer's `provider.fake` is stripped), so only config drives wiring.
fn command(args: &[&str], stdin: &str) -> std::process::Child {
    let xdg = temp_dir("xdg");
    let config_dir = xdg.join("kanbei");
    std::fs::create_dir_all(&config_dir).unwrap();
    std::fs::write(config_dir.join("init.lua"), FAKE_CONFIG).unwrap();
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

/// F12: a resolvable provider URL with an unavailable key degrades to a
/// storage-only session (never fails open) and prints an actionable,
/// secret-free stderr line.
#[test]
fn unavailable_provider_key_degrades_to_storage_only() {
    let dir = temp_dir("nokey");
    let xdg = temp_dir("nokey-xdg");
    let mut child = Command::new(env!("CARGO_BIN_EXE_kanbei"))
        .arg(dir.to_string_lossy().to_string())
        .env("XDG_CONFIG_HOME", &xdg)
        .env("KANBEI_PROVIDER_URL", "https://example.invalid/v1")
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
        .write_all(b"hello\n/exit\n")
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("provider key unavailable") && stderr.contains("storage-only"),
        "expected an actionable storage-only notice, stderr: {stderr:?}"
    );
    assert!(
        stderr.contains("(none)"),
        "the session reports no provider engine: {stderr:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    let _ = std::fs::remove_dir_all(&xdg);
}
