//! Integration test for config discovery (decision 28/T8): the shipped CLI's
//! `discover_config_layers` + `SessionConfig` path executes a real Luau config
//! generation at startup; an invalid user config falls back to safe mode with
//! a canonical fact.
//!
//! A missing guest is a hard failure: build it with `cargo xtask build-guest`
//! from the workspace root first.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use kanbei_core::envelope::Envelope;
use kanbei_log::for_each_frame;
use kanbei_session::{
    Session, SessionConfig, builtin_config_manifest, discover_config_layers,
};
use kanbei_vm::{GuestError, Vm, VmConfig};

/// `set_var` is process-global; serialize the env-mutating tests (nextest also
/// isolates them per process, but this keeps `cargo test` deterministic).
static ENV_LOCK: Mutex<()> = Mutex::new(());

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kb-discovery-it-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn require_guest() {
    match Vm::load(no_epoch()) {
        Ok(_) => {}
        Err(GuestError::NotBuilt) => {
            panic!("guest wasm not built: run `cargo xtask build-guest` from the workspace root")
        }
        Err(e) => panic!("Vm::load failed: {e}"),
    }
}

fn no_epoch() -> VmConfig {
    VmConfig {
        fuel_per_call: u64::MAX,
        epoch_deadline: u64::MAX,
        ..Default::default()
    }
}

fn write_user_config(config_home: &Path, contents: &str) {
    let path = config_home.join("kanbei").join("init.lua");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn envelopes(log_path: &Path) -> Vec<Envelope> {
    let mut out = Vec::new();
    for_each_frame(log_path, |frame| {
        for line in &frame.events {
            out.push(Envelope::from_line(line).unwrap());
        }
    })
    .unwrap();
    out
}

/// A discovered user layer is activated at open and its published settings are
/// reflected by `host_settings` (built-in defaults overridden field-wise).
#[test]
fn discovered_user_config_drives_host_settings() {
    require_guest();
    let _guard = ENV_LOCK.lock().unwrap();
    let config_home = TempDir::new("valid-cfg");
    let session_dir = TempDir::new("valid-session");
    write_user_config(
        config_home.path(),
        r#"function kb_on_activate(ctx)
  ctx.contribution_publish('{"kind":"settings","approval":{"auto_approve":true}}')
end
function kb_hot(dispatch) return dispatch end
"#,
    );
    // SAFETY: ENV_LOCK serializes every env mutation in this test binary.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", config_home.path()) };
    let layers = discover_config_layers(session_dir.path()).unwrap();
    unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
    assert_eq!(layers.len(), 2, "built-in + user layer");
    assert_eq!(layers[0], builtin_config_manifest());

    let session = Session::open(SessionConfig {
        dir: session_dir.path().to_path_buf(),
        engine: Some(no_epoch()),
        config_layers: layers,
        ..Default::default()
    })
    .unwrap();
    let approval = session
        .host_settings()
        .approval
        .as_ref()
        .expect("merged approval settings");
    assert_eq!(approval.auto_approve, Some(true), "user config applied");
    assert_eq!(approval.yolo, Some(false), "built-in default preserved");
    session.close().unwrap();
}

/// A syntactically invalid discovered user config fails activation; the session
/// still opens with the built-in generation active and a canonical
/// `safe_mode_activated` fact on the log.
#[test]
fn invalid_discovered_config_opens_safe_mode() {
    require_guest();
    let _guard = ENV_LOCK.lock().unwrap();
    let config_home = TempDir::new("invalid-cfg");
    let session_dir = TempDir::new("invalid-session");
    write_user_config(config_home.path(), "local x = = 1\n");
    // SAFETY: ENV_LOCK serializes every env mutation in this test binary.
    unsafe { std::env::set_var("XDG_CONFIG_HOME", config_home.path()) };
    let layers = discover_config_layers(session_dir.path()).unwrap();
    unsafe { std::env::remove_var("XDG_CONFIG_HOME") };
    assert_eq!(layers.len(), 2, "discovery succeeds; activation is what fails");

    let session = Session::open(SessionConfig {
        dir: session_dir.path().to_path_buf(),
        engine: Some(no_epoch()),
        config_layers: layers,
        ..Default::default()
    })
    .unwrap();
    let snapshot = session.modules().expect("modules enabled").snapshot();
    assert_eq!(snapshot.len(), 1, "only the built-in survives");
    assert_eq!(snapshot[0].0, builtin_config_manifest().module_id);
    let envs = envelopes(&session_dir.path().join("log.zst"));
    assert_eq!(envs[0].kind, "composition_changed");
    assert_eq!(envs[1].kind, "safe_mode_activated");
    assert!(
        envs[1].payload["reason"]
            .as_str()
            .is_some_and(|r| r.contains("compile")),
        "reason names the failure: {}",
        envs[1].payload
    );
    session.close().unwrap();
}
