//! Integration tests for the XDG state layout (decision 33/T14): session
//! resolution under a layout, the persisted `session.json`, legacy migration
//! and its idempotency, and the unchanged no-layout default.

use std::path::{Path, PathBuf};

use kanbei_core::Digest;
use kanbei_core::envelope::Envelope;
use kanbei_core::id::Id128;
use kanbei_core::StateLayout;
use kanbei_log::for_each_frame;
use kanbei_modules::{ModuleOrigin, PACKAGE_SCHEMA, PackageManifest};
use kanbei_session::{NewEvent, Session, SessionConfig};
use kanbei_services::ScopePath;
use kanbei_vm::{GuestError, Vm, VmConfig};
use serde_json::json;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kb-layout-test-{tag}-{}-{}",
            std::process::id(),
            Id128::generate()
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

fn event(kind: &str, payload: serde_json::Value) -> NewEvent {
    NewEvent {
        kind: kind.into(),
        payload_schema: 1,
        payload,
        objects: Vec::new(),
        refs: Vec::new(),
    }
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

/// Opens under `layout`, using `legacy` only as the (typically empty) migration
/// source so resolution falls through to the manifest scan/new id.
fn open_under(layout: &StateLayout, legacy: &Path) -> Session {
    Session::open(SessionConfig {
        dir: legacy.to_path_buf(),
        layout: Some(layout.clone()),
        ..Default::default()
    })
    .unwrap()
}

/// (a) A session opened with a layout lands at `sessions/<id>/events.jsonl.zst`
/// with a persisted `session.json`.
#[test]
fn session_lands_under_the_layout_with_a_manifest() {
    let root = TempDir::new("a-root");
    let legacy = TempDir::new("a-legacy");
    let layout = StateLayout::new(root.path());
    let mut session = open_under(&layout, legacy.path());
    let id = session.session_id();
    session
        .commit(vec![event("probe", json!({"n": 1}))], None)
        .unwrap();

    let log = layout.session_log(id);
    assert!(log.is_file(), "canonical log at {}", log.display());
    assert_eq!(session.log_path(), log);
    assert!(layout.session_dir(id).join("objects").is_dir());

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(layout.session_manifest(id)).unwrap())
            .unwrap();
    assert_eq!(manifest["schema"], 1);
    assert_eq!(manifest["session"], id.to_string());
    assert_eq!(manifest["log"], "events.jsonl.zst");
    assert!(manifest["project"].is_null(), "unbound session: {manifest}");
    session.close().unwrap();
}

/// (b) Reopening with the same layout resolves the same session.
#[test]
fn reopening_resolves_the_same_session() {
    let root = TempDir::new("b-root");
    let legacy = TempDir::new("b-legacy");
    let layout = StateLayout::new(root.path());
    let id = {
        let mut session = open_under(&layout, legacy.path());
        session
            .commit(vec![event("probe", json!({"n": 2}))], None)
            .unwrap();
        let id = session.session_id();
        session.close().unwrap();
        id
    };

    let session = open_under(&layout, legacy.path());
    assert_eq!(session.session_id(), id, "manifest identity is resumed");
    assert_eq!(session.log_path(), layout.session_log(id));
    assert_eq!(envelopes(session.log_path()).len(), 1, "no new events on reopen");
    session.close().unwrap();
}

/// (c) A legacy dir (with `log.zst`) migrates into the layout, preserving
/// identity and continuing the same log.
#[test]
fn legacy_dir_migrates_and_resumes() {
    let legacy = TempDir::new("c-legacy");
    let id = Id128::generate();
    let project = Id128::generate();
    {
        // A bound project records `created_session` in the registry — the
        // identity marker migration recovers.
        let mut session = Session::open(SessionConfig {
            dir: legacy.path().to_path_buf(),
            session_id: Some(id),
            project: Some(project),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(session.session_id(), id);
        session
            .commit(vec![event("legacy_probe", json!({"n": 7}))], None)
            .unwrap();
        session.close().unwrap();
    }
    assert!(legacy.path().join("log.zst").is_file());

    let root = TempDir::new("c-root");
    let layout = StateLayout::new(root.path());
    let mut session = open_under(&layout, legacy.path());
    assert_eq!(session.session_id(), id, "migration preserves identity");
    assert_eq!(session.log_path(), layout.session_log(id));
    assert!(layout.session_manifest(id).is_file());

    let before = session.next_seq();
    session
        .commit(vec![event("after_migration", json!({}))], None)
        .unwrap();
    assert!(session.next_seq() > before, "the migrated log continues");
    let kinds: Vec<String> = envelopes(session.log_path())
        .into_iter()
        .map(|e| e.kind)
        .collect();
    assert!(kinds.contains(&"legacy_probe".to_string()));
    assert!(kinds.contains(&"after_migration".to_string()));
    session.close().unwrap();
}

/// (d) Migration is idempotent: a second open copies nothing and the session
/// remains resolvable from the manifest alone.
#[test]
fn migration_is_idempotent() {
    let legacy = TempDir::new("d-legacy");
    let id = Id128::generate();
    let project = Id128::generate();
    {
        let mut session = Session::open(SessionConfig {
            dir: legacy.path().to_path_buf(),
            session_id: Some(id),
            project: Some(project),
            ..Default::default()
        })
        .unwrap();
        session
            .commit(vec![event("legacy_probe", json!({}))], None)
            .unwrap();
        session.close().unwrap();
    }
    let root = TempDir::new("d-root");
    let layout = StateLayout::new(root.path());

    let events_after_first = {
        let session = open_under(&layout, legacy.path());
        assert_eq!(session.session_id(), id);
        let n = envelopes(session.log_path()).len();
        session.close().unwrap();
        n
    };

    // A second migration over the same legacy source.
    let session = open_under(&layout, legacy.path());
    assert_eq!(session.session_id(), id);
    assert_eq!(envelopes(session.log_path()).len(), events_after_first);
    session.close().unwrap();

    // With the legacy source gone, the manifest alone resolves the session.
    let empty = TempDir::new("d-empty");
    let session = open_under(&layout, empty.path());
    assert_eq!(session.session_id(), id);
    assert_eq!(envelopes(session.log_path()).len(), events_after_first);

    let sessions = std::fs::read_dir(layout.root().join("sessions")).unwrap().count();
    assert_eq!(sessions, 1, "one session dir, never a duplicate");
    session.close().unwrap();
}

/// (e) The default (no layout) path is unchanged: `log.zst` under `dir`, no
/// manifest, including on close.
#[test]
fn default_path_is_unchanged() {
    let dir = TempDir::new("e-default");
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        ..Default::default()
    })
    .unwrap();
    session
        .commit(vec![event("probe", json!({}))], None)
        .unwrap();
    assert_eq!(session.log_path(), dir.path().join("log.zst"));
    assert!(dir.path().join("objects").is_dir());
    assert!(dir.path().join("memory").is_dir());
    session.close().unwrap();
    assert!(!dir.path().join("session.json").exists());
}

/// (f) The manifest persists the bound project; a resume that names no project
/// re-binds the persisted one (decision 33: the identity precedes the log).
#[test]
fn manifest_persists_the_bound_project() {
    let root = TempDir::new("g-root");
    let legacy = TempDir::new("g-legacy");
    let layout = StateLayout::new(root.path());
    let project = Id128::generate();
    let id = {
        let session = Session::open(SessionConfig {
            dir: legacy.path().to_path_buf(),
            layout: Some(layout.clone()),
            project: Some(project),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(session.project_entry().unwrap().project_id, project);
        let id = session.session_id();
        session.close().unwrap();
        id
    };

    let manifest: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(layout.session_manifest(id)).unwrap())
            .unwrap();
    assert_eq!(manifest["project"], project.to_string());

    let session = open_under(&layout, legacy.path());
    assert_eq!(session.session_id(), id);
    assert_eq!(
        session.project_entry().map(|entry| entry.project_id),
        Some(project),
        "resume re-binds the persisted project"
    );
    session.close().unwrap();
}

/// An ambiguous legacy dir (a `log.zst` with no recoverable identity) fails
/// loud rather than guessing.
#[test]
fn ambiguous_legacy_dir_fails_loud() {
    let legacy = TempDir::new("f-legacy");
    {
        let mut session = Session::open(SessionConfig {
            dir: legacy.path().to_path_buf(),
            ..Default::default()
        })
        .unwrap();
        session
            .commit(vec![event("anonymous", json!({}))], None)
            .unwrap();
        session.close().unwrap();
    }
    assert!(legacy.path().join("log.zst").is_file());

    let root = TempDir::new("f-root");
    let layout = StateLayout::new(root.path());
    let err = match Session::open(SessionConfig {
        dir: legacy.path().to_path_buf(),
        layout: Some(layout),
        ..Default::default()
    }) {
        Ok(_) => panic!("ambiguous legacy migration must fail"),
        Err(e) => e,
    };
    assert!(
        matches!(
            err,
            kanbei_session::SessionError::InvalidInput(ref m)
                if m.contains("no recoverable session id")
        ),
        "expected a loud ambiguous-migration error, got {err:?}"
    );
}

// --- global module store (decision 33/T14) ---------------------------------

/// Module activation needs the guest wasm; a missing guest is a hard failure
/// (the module-install assertions must not pass by omission).
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

fn config_package(name: &str) -> PackageManifest {
    PackageManifest {
        schema: PACKAGE_SCHEMA,
        module_id: Id128::generate(),
        origin: ModuleOrigin::UserConfig,
        trust_class: kanbei_capabilities::TrustClass::User,
        scope: ScopePath(vec![]),
        deps: vec![],
        capabilities: vec![],
        source: format!(
            "function kb_on_activate(ctx) ctx.service_publish('{{\"scope\":[],\"name\":\"{name}\"}}', 1, '[]') end\nfunction kb_hot(x) return x end"
        ),
        state_schema: None,
        state_key: None,
    }
}

/// The canonical package digest (`install_package` hashes the canonical JSON).
fn package_digest(manifest: &PackageManifest) -> Digest {
    Digest::new(&serde_json::to_vec(manifest).unwrap())
}

/// (g) With a layout the package installs into the GLOBAL module store
/// (`<state>/modules/<digest>`) and not into the session dir, which keeps only
/// the session's own objects (the genesis snapshot).
#[test]
fn package_installs_into_the_global_module_store() {
    require_guest();
    let root = TempDir::new("g-root");
    let legacy = TempDir::new("g-legacy");
    let layout = StateLayout::new(root.path());
    let config = config_package("global-greeter");
    let digest = package_digest(&config);

    let mut session = Session::open(SessionConfig {
        dir: legacy.path().to_path_buf(),
        layout: Some(layout.clone()),
        config_layers: vec![config],
        engine: Some(no_epoch()),
        ..Default::default()
    })
    .unwrap();
    let id = session.session_id();
    let genesis = session.current_snapshot().expect("genesis snapshot");
    assert!(session.store().exists(&genesis), "session objects stay per-session");
    session.close().unwrap();

    assert!(
        layout.module_root().join(digest.to_string()).is_file(),
        "the package lands in the global module store"
    );
    let session_objects = layout.session_dir(id).join("objects");
    assert!(session_objects.join(genesis.to_string()).is_file());
    assert!(
        !session_objects.join(digest.to_string()).exists(),
        "the package is not copied into the session dir"
    );
}

/// (h) The same digest is reused across two sessions under one layout: the
/// second activation finds the package in the global store (no rewrite), and
/// neither session dir holds a copy.
#[test]
fn the_global_module_store_is_shared_across_sessions() {
    require_guest();
    let root = TempDir::new("h-root");
    let legacy = TempDir::new("h-legacy");
    let layout = StateLayout::new(root.path());
    let config = config_package("shared-greeter");
    let digest = package_digest(&config);

    let open = |session_id: Option<Id128>| {
        Session::open(SessionConfig {
            dir: legacy.path().to_path_buf(),
            layout: Some(layout.clone()),
            session_id,
            config_layers: vec![config.clone()],
            engine: Some(no_epoch()),
            ..Default::default()
        })
        .unwrap()
    };
    let first = open(Some(Id128::generate()));
    let first_id = first.session_id();
    first.close().unwrap();

    let path = layout.module_root().join(digest.to_string());
    let installed_at = std::fs::metadata(&path).unwrap().modified().unwrap();

    let second = open(Some(Id128::generate()));
    let second_id = second.session_id();
    second.close().unwrap();

    assert_ne!(first_id, second_id);
    let names: Vec<String> = std::fs::read_dir(layout.module_root())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
        .collect();
    assert!(
        names.contains(&digest.to_string()),
        "the shared package is one entry of the store: {names:?}"
    );
    assert!(
        names.iter().all(|n| Digest::from_hex(n).is_ok()),
        "flat content-addressed <digest> files, never per-digest dirs: {names:?}"
    );
    assert_eq!(
        std::fs::metadata(&path).unwrap().modified().unwrap(),
        installed_at,
        "the second session deduped instead of rewriting the package"
    );
    for id in [first_id, second_id] {
        assert!(
            !layout
                .session_dir(id)
                .join("objects")
                .join(digest.to_string())
                .exists()
        );
    }
}

/// (i) An explicit dir keeps the legacy package store byte-identically: the
/// package lands in `<dir>/objects/` and the session stays on `log.zst`.
#[test]
fn explicit_dir_keeps_the_legacy_package_store() {
    require_guest();
    let dir = TempDir::new("i-legacy");
    let config = config_package("legacy-greeter");
    let digest = package_digest(&config);

    let session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        config_layers: vec![config],
        engine: Some(no_epoch()),
        ..Default::default()
    })
    .unwrap();

    assert_eq!(session.log_path(), dir.path().join("log.zst"));
    assert!(session.store().exists(&digest));
    assert!(dir.path().join("objects").join(digest.to_string()).is_file());
    session.close().unwrap();
}
