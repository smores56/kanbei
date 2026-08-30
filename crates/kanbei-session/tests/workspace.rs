//! M9 wave 4: session-level workspace snapshot/restore — canonical
//! `workspace_snapshot` / `workspace_restore` events on the log, refs pinned
//! to the manifest digest, and no event committed on restore failure.

use std::path::{Path, PathBuf};

use kanbei_core::digest::Digest;
use kanbei_core::envelope::Envelope;
use kanbei_log::for_each_frame;
use kanbei_session::{Session, SessionConfig, SessionError};
use kanbei_workspace::SnapshotOptions;

// --- helpers ---------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kb-session-ws-{tag}-{}-{}",
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

/// All envelopes currently in the log, in seq order.
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

// --- tests -----------------------------------------------------------------

#[test]
fn snapshot_and_restore_roundtrip_with_canonical_events() {
    let dir = TempDir::new("roundtrip");
    let fs_root = dir.path().join("tree");
    std::fs::create_dir_all(fs_root.join("sub")).unwrap();
    std::fs::write(fs_root.join("a.txt"), "hello").unwrap();
    std::fs::write(fs_root.join("sub/b.txt"), "world").unwrap();

    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        fs_root: fs_root.clone(),
        ..Default::default()
    })
    .unwrap();

    let manifest = session.snapshot_workspace(SnapshotOptions::default()).unwrap();
    // the snapshot's blobs and manifest are ordinary objects in the store
    assert!(session.store().exists(&manifest));
    let manifest_bytes = session.store().get(&manifest).unwrap();
    assert_eq!(serde_json::from_slice::<serde_json::Value>(&manifest_bytes).unwrap()["schema"], 1);

    let snap = envelopes(&dir.path().join("log.zst"))
        .into_iter()
        .find(|env| env.kind == "workspace_snapshot")
        .expect("workspace_snapshot envelope on the log");
    assert_eq!(snap.refs, vec![manifest]);
    assert_eq!(snap.payload["manifest"], manifest.to_string());
    assert_eq!(snap.payload["root"], fs_root.to_string_lossy().to_string());
    assert_eq!(snap.payload["entries"], 2);

    // mutate the tree, then restore it from the snapshot
    std::fs::write(fs_root.join("a.txt"), "mutated").unwrap();
    std::fs::remove_file(fs_root.join("sub/b.txt")).unwrap();
    let report = session.restore_workspace(&manifest).unwrap();
    assert_eq!(report.entries_restored, 2);
    assert_eq!(report.bytes, 10); // "hello" 5 + "world" 5
    assert_eq!(report.symlinks, 0);
    assert_eq!(
        std::fs::read_to_string(fs_root.join("a.txt")).unwrap(),
        "hello"
    );
    assert_eq!(
        std::fs::read_to_string(fs_root.join("sub/b.txt")).unwrap(),
        "world"
    );

    let rest = envelopes(&dir.path().join("log.zst"))
        .into_iter()
        .find(|env| env.kind == "workspace_restore")
        .expect("workspace_restore envelope on the log");
    assert_eq!(rest.refs, vec![manifest]);
    assert_eq!(rest.payload["manifest"], manifest.to_string());
    assert_eq!(rest.payload["entries_restored"], 2);
    assert_eq!(rest.payload["bytes"], 10);
}

#[test]
fn failed_restore_commits_no_event() {
    let dir = TempDir::new("failed");
    let fs_root = dir.path().join("tree");
    std::fs::create_dir_all(&fs_root).unwrap();
    std::fs::write(fs_root.join("a.txt"), "hello").unwrap();

    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        fs_root: fs_root.clone(),
        ..Default::default()
    })
    .unwrap();
    let manifest = session.snapshot_workspace(SnapshotOptions::default()).unwrap();
    let before = envelopes(&dir.path().join("log.zst")).len();

    // a digest that was never installed
    let ghost = Digest::new(b"no such snapshot");
    let err = session.restore_workspace(&ghost).unwrap_err();
    assert!(
        matches!(err, SessionError::Workspace(kanbei_workspace::WorkspaceError::MissingObject { digest }) if digest == ghost),
        "unexpected error: {err:?}"
    );

    // no event for the failed restore — the log is unchanged
    let after = envelopes(&dir.path().join("log.zst"));
    assert_eq!(after.len(), before);
    assert!(!after.iter().any(|env| env.kind == "workspace_restore"));
    // the tree was not touched
    assert_eq!(
        std::fs::read_to_string(fs_root.join("a.txt")).unwrap(),
        "hello"
    );
    // and the session still works for a real restore
    let report = session.restore_workspace(&manifest).unwrap();
    assert_eq!(report.entries_restored, 1);
    assert!(envelopes(&dir.path().join("log.zst"))
        .iter()
        .any(|env| env.kind == "workspace_restore"));
}
