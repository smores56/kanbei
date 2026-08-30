//! Integration tests for kanbei-workspace: snapshot/restore round-trips,
//! determinism, ignore defaults, overwrite semantics, the restore guards
//! (missing blobs, path escape, symlinked intermediates), and explicit
//! errors for unreadable/unsupported/non-UTF-8 entries.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use kanbei_core::digest::Digest;
use kanbei_core::queue::DurabilityQueue;
use kanbei_objects::ObjectStore;
use kanbei_workspace::{
    restore, snapshot, RestoreReport, SnapshotOptions, WorkspaceError, MANIFEST_SCHEMA,
};

// --- helpers ---------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kb-workspace-{tag}-{}-{}",
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

/// Object store + its backing dir, alive for the whole test.
struct Fixture {
    _dir: TempDir,
    store: ObjectStore,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let dir = TempDir::new(tag);
        let queue = Arc::new(DurabilityQueue::start(&format!("test-workspace-{tag}")));
        let store = ObjectStore::open(&dir.path().join("objects"), queue).unwrap();
        Self { _dir: dir, store }
    }

    /// Installs arbitrary bytes and returns their digest (for crafting
    /// manifests the snapshot walk would never produce).
    fn install(&mut self, bytes: &[u8]) -> Digest {
        self.store.install(bytes).unwrap()
    }
}

fn set_executable(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path).unwrap().permissions().mode() & 0o111 != 0
}

/// A small tree exercising every entry kind: nested dirs, an executable
/// file, a symlink, and an empty dir (which must NOT be recorded).
fn sample_tree(root: &Path) {
    std::fs::create_dir_all(root.join("src/util")).unwrap();
    std::fs::create_dir_all(root.join("bin")).unwrap();
    std::fs::create_dir_all(root.join("empty")).unwrap();
    std::fs::write(root.join("README.md"), "readme").unwrap();
    std::fs::write(root.join("src/main.rs"), "fn main() {}").unwrap();
    std::fs::write(root.join("src/util/helper.txt"), "helper").unwrap();
    set_executable(&root.join("src/main.rs"));
    std::os::unix::fs::symlink("../src/main.rs", root.join("bin/run")).unwrap();
}

/// A manifest JSON document with the given entries (schema 1).
fn manifest_json(entries: &[String]) -> String {
    format!(
        "{{\"schema\": {MANIFEST_SCHEMA}, \"entries\": [{}]}}",
        entries.join(",")
    )
}

fn file_entry(path: &str, digest: &Digest, executable: bool) -> String {
    format!(
        "{{\"type\":\"file\",\"path\":\"{path}\",\"digest\":\"{digest}\",\"executable\":{executable}}}"
    )
}

fn symlink_entry(path: &str, target: &str) -> String {
    format!("{{\"type\":\"symlink\",\"path\":\"{path}\",\"symlink\":\"{target}\"}}")
}

// --- snapshot/restore round-trips ------------------------------------------

#[test]
fn roundtrip_files_dirs_executable_symlink_empty_dir_absent() {
    let src = TempDir::new("roundtrip-src");
    let dst = TempDir::new("roundtrip-dst");
    sample_tree(src.path());
    let mut fx = Fixture::new("roundtrip");

    let manifest = snapshot(&mut fx.store, src.path(), &SnapshotOptions::default()).unwrap();
    let report = restore(&fx.store, &manifest, dst.path()).unwrap();

    // 4 entries: 3 files + 1 symlink; the empty dir is absent from both
    // manifest and restored tree
    assert_eq!(
        report,
        RestoreReport {
            entries_restored: 4,
            bytes: 24, // "readme" 6 + "fn main() {}" 12 + "helper" 6
            symlinks: 1,
        }
    );
    assert_eq!(
        std::fs::read_to_string(dst.path().join("README.md")).unwrap(),
        "readme"
    );
    assert_eq!(
        std::fs::read_to_string(dst.path().join("src/main.rs")).unwrap(),
        "fn main() {}"
    );
    assert_eq!(
        std::fs::read_to_string(dst.path().join("src/util/helper.txt")).unwrap(),
        "helper"
    );
    assert!(is_executable(&dst.path().join("src/main.rs")));
    assert!(!is_executable(&dst.path().join("README.md")));
    assert_eq!(
        std::fs::read_link(dst.path().join("bin/run")).unwrap(),
        PathBuf::from("../src/main.rs")
    );
    assert!(!dst.path().join("empty").exists());
    // the snapshot recorded the symlink, not its target's content
    let bytes = fx.store.get(&manifest).unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["entries"].as_array().unwrap().len(), 4);
}

#[test]
fn broken_symlink_snapshots_and_restores() {
    let src = TempDir::new("broken-src");
    let dst = TempDir::new("broken-dst");
    std::fs::write(src.path().join("real.txt"), "real").unwrap();
    std::os::unix::fs::symlink("does-not-exist.txt", src.path().join("dangling")).unwrap();
    let mut fx = Fixture::new("broken");

    let manifest = snapshot(&mut fx.store, src.path(), &SnapshotOptions::default()).unwrap();
    restore(&fx.store, &manifest, dst.path()).unwrap();

    assert_eq!(
        std::fs::read_link(dst.path().join("dangling")).unwrap(),
        PathBuf::from("does-not-exist.txt")
    );
    // the dangling link is a symlink, not a file
    assert!(dst.path().join("dangling").symlink_metadata().unwrap().file_type().is_symlink());
}

// --- determinism -----------------------------------------------------------

#[test]
fn same_tree_same_digest_change_tree_changes_digest() {
    let tree = TempDir::new("determinism");
    sample_tree(tree.path());
    let mut fx = Fixture::new("determinism");

    let first = snapshot(&mut fx.store, tree.path(), &SnapshotOptions::default()).unwrap();
    let second = snapshot(&mut fx.store, tree.path(), &SnapshotOptions::default()).unwrap();
    assert_eq!(first, second);

    std::fs::write(tree.path().join("extra.txt"), "extra").unwrap();
    let third = snapshot(&mut fx.store, tree.path(), &SnapshotOptions::default()).unwrap();
    assert_ne!(first, third);
    // the manifest object itself is content-addressed: 4 installs on the
    // first snapshot (3 files + manifest), 0 dedup hits on the second, 2
    // fresh installs for the changed tree (extra.txt + manifest)
    assert_eq!(fx.store.installs, 6);
}

// --- ignore ----------------------------------------------------------------

#[test]
fn ignore_defaults_skip_git_and_target_at_top_level_only() {
    let tree = TempDir::new("ignore");
    std::fs::create_dir_all(tree.path().join(".git")).unwrap();
    std::fs::create_dir_all(tree.path().join("target/debug")).unwrap();
    std::fs::create_dir_all(tree.path().join("src/target")).unwrap();
    std::fs::write(tree.path().join(".git/config"), "git config").unwrap();
    std::fs::write(tree.path().join("target/debug/app"), "binary").unwrap();
    std::fs::write(tree.path().join("src/target/nested"), "nested target").unwrap();
    std::fs::write(tree.path().join("keep.txt"), "keep").unwrap();
    let mut fx = Fixture::new("ignore");

    let manifest = snapshot(&mut fx.store, tree.path(), &SnapshotOptions::default()).unwrap();
    let bytes = fx.store.get(&manifest).unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let paths: Vec<&str> = parsed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    // top-level .git/target skipped; the nested `target` dir is walked
    assert_eq!(paths, vec!["keep.txt", "src/target/nested"]);
}

#[test]
fn custom_ignore_replaces_defaults() {
    let tree = TempDir::new("custom-ignore");
    std::fs::create_dir_all(tree.path().join("vendor")).unwrap();
    std::fs::create_dir_all(tree.path().join(".git")).unwrap();
    std::fs::write(tree.path().join("vendor/lib.rs"), "lib").unwrap();
    std::fs::write(tree.path().join(".git/config"), "config").unwrap();
    std::fs::write(tree.path().join("main.rs"), "main").unwrap();
    let mut fx = Fixture::new("custom-ignore");

    let options = SnapshotOptions {
        ignore: vec!["vendor".into()],
    };
    let manifest = snapshot(&mut fx.store, tree.path(), &options).unwrap();
    let bytes = fx.store.get(&manifest).unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    let paths: Vec<&str> = parsed["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["path"].as_str().unwrap())
        .collect();
    // default .git/target list replaced by the custom one
    assert_eq!(paths, vec![".git/config", "main.rs"]);
}

// --- restore semantics -----------------------------------------------------

#[test]
fn restore_overwrites_manifest_files_and_leaves_others_untouched() {
    let src = TempDir::new("overwrite-src");
    std::fs::write(src.path().join("a.txt"), "snapshot content").unwrap();
    let mut fx = Fixture::new("overwrite");
    let manifest = snapshot(&mut fx.store, src.path(), &SnapshotOptions::default()).unwrap();

    let dst = TempDir::new("overwrite-dst");
    std::fs::write(dst.path().join("a.txt"), "old content").unwrap();
    std::fs::write(dst.path().join("extra.txt"), "not in manifest").unwrap();

    restore(&fx.store, &manifest, dst.path()).unwrap();

    assert_eq!(
        std::fs::read_to_string(dst.path().join("a.txt")).unwrap(),
        "snapshot content"
    );
    // additive, not a wipe: files outside the manifest survive
    assert_eq!(
        std::fs::read_to_string(dst.path().join("extra.txt")).unwrap(),
        "not in manifest"
    );
}

#[test]
fn restore_replaces_existing_symlink_with_file_and_vice_versa() {
    let src = TempDir::new("flip-src");
    std::fs::write(src.path().join("item"), "plain file").unwrap();
    let mut fx = Fixture::new("flip");
    let manifest = snapshot(&mut fx.store, src.path(), &SnapshotOptions::default()).unwrap();

    // a symlink sits where the manifest has a file: rename replaces the link
    let dst = TempDir::new("flip-dst");
    std::os::unix::fs::symlink("/somewhere/else", dst.path().join("item")).unwrap();
    restore(&fx.store, &manifest, dst.path()).unwrap();
    assert!(!dst.path().join("item").symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(
        std::fs::read_to_string(dst.path().join("item")).unwrap(),
        "plain file"
    );

    // the reverse: a file sits where the manifest has a symlink
    let src2 = TempDir::new("flip-src2");
    std::os::unix::fs::symlink("target.txt", src2.path().join("link")).unwrap();
    let manifest2 = snapshot(&mut fx.store, src2.path(), &SnapshotOptions::default()).unwrap();
    let dst2 = TempDir::new("flip-dst2");
    std::fs::write(dst2.path().join("link"), "stale file").unwrap();
    restore(&fx.store, &manifest2, dst2.path()).unwrap();
    assert!(dst2.path().join("link").symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(
        std::fs::read_link(dst2.path().join("link")).unwrap(),
        PathBuf::from("target.txt")
    );
}

// --- restore guards --------------------------------------------------------

#[test]
fn missing_manifest_object_is_typed_error() {
    let dst = TempDir::new("missing-manifest");
    let fx = Fixture::new("missing-manifest");
    let ghost = Digest::new(b"never installed");
    let err = restore(&fx.store, &ghost, dst.path()).unwrap_err();
    match err {
        WorkspaceError::MissingObject { digest } => assert_eq!(digest, ghost),
        other => panic!("expected MissingObject, got {other:?}"),
    }
}

#[test]
fn missing_blob_names_digest_path_and_partial_count() {
    let mut fx = Fixture::new("missing-blob");
    let present = fx.install(b"present");
    let ghost = Digest::new(b"ghost blob");
    let dst = TempDir::new("missing-blob");
    let json = manifest_json(&[
        file_entry("ok.txt", &present, false),
        file_entry("gone.txt", &ghost, false),
    ]);
    let manifest = fx.install(json.as_bytes());

    let err = restore(&fx.store, &manifest, dst.path()).unwrap_err();
    match err {
        WorkspaceError::MissingBlob {
            digest,
            path,
            written,
        } => {
            assert_eq!(digest, ghost);
            assert_eq!(path, "gone.txt");
            // the first entry was already written before the failure
            assert_eq!(written, 1);
            assert_eq!(
                std::fs::read_to_string(dst.path().join("ok.txt")).unwrap(),
                "present"
            );
        }
        other => panic!("expected MissingBlob, got {other:?}"),
    }
}

#[test]
fn path_escape_rejected_before_any_write() {
    let mut fx = Fixture::new("escape");
    let payload = fx.install(b"evil");
    let dst = TempDir::new("escape");
    let parent = dst.path().parent().unwrap();

    for bad in ["../evil.txt", "/absolute.txt", "a/../b.txt"] {
        let json = manifest_json(&[file_entry(bad, &payload, false)]);
        let manifest = fx.install(json.as_bytes());
        let err = restore(&fx.store, &manifest, dst.path()).unwrap_err();
        match err {
            WorkspaceError::PathOutsideRoot { path } => assert_eq!(path, bad),
            other => panic!("expected PathOutsideRoot for {bad:?}, got {other:?}"),
        }
        assert!(!parent.join("evil.txt").exists());
        assert!(!Path::new("/absolute.txt").exists());
    }
    // nothing inside the root either
    assert_eq!(std::fs::read_dir(dst.path()).unwrap().count(), 0);
}

#[test]
fn symlinked_intermediate_dir_outside_root_rejected() {
    let outside = TempDir::new("symlink-outside");
    let dst = TempDir::new("symlink-escape");
    let mut fx = Fixture::new("symlink-escape");
    let payload = fx.install(b"pwn");
    // dst/link -> outside; the manifest's file entry lands under it
    std::os::unix::fs::symlink(outside.path(), dst.path().join("link")).unwrap();
    let json = manifest_json(&[file_entry("link/evil.txt", &payload, false)]);
    let manifest = fx.install(json.as_bytes());

    let err = restore(&fx.store, &manifest, dst.path()).unwrap_err();
    match err {
        WorkspaceError::PathOutsideRoot { path } => assert_eq!(path, "link/evil.txt"),
        other => panic!("expected PathOutsideRoot, got {other:?}"),
    }
    assert!(!outside.path().join("evil.txt").exists());
}

#[test]
fn invalid_manifests_rejected() {
    let mut fx = Fixture::new("invalid");
    let dst = TempDir::new("invalid");
    for (tag, bytes, needle) in [
        ("bad-schema", br#"{"schema": 2, "entries": []}"#.to_vec(), "unsupported schema"),
        (
            "not-json",
            b"this is not json".to_vec(),
            "invalid workspace manifest",
        ),
        (
            "wrong-shape",
            br#"{"schema": 1, "entries": [{"type":"file","path":"a"}]}"#.to_vec(),
            "invalid workspace manifest",
        ),
    ] {
        let manifest = fx.install(&bytes);
        let err = restore(&fx.store, &manifest, dst.path()).unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(needle), "{tag}: {msg}");
    }
}

// --- explicit snapshot errors ----------------------------------------------

#[test]
fn unreadable_file_is_explicit_error_with_path() {
    let tree = TempDir::new("unreadable");
    std::fs::write(tree.path().join("ok.txt"), "ok").unwrap();
    std::fs::write(tree.path().join("secret.txt"), "secret").unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(tree.path().join("secret.txt"), std::fs::Permissions::from_mode(0o000))
            .unwrap();
    }
    let mut fx = Fixture::new("unreadable");

    let err = snapshot(&mut fx.store, tree.path(), &SnapshotOptions::default()).unwrap_err();
    match err {
        WorkspaceError::Io { path, .. } => {
            assert_eq!(path, tree.path().join("secret.txt"))
        }
        other => panic!("expected Io with path, got {other:?}"),
    }
    // no partial manifest was installed
    assert_eq!(fx.store.installs, 1); // only ok.txt
}

#[test]
fn unsupported_entry_type_is_explicit_error() {
    let tree = TempDir::new("socket");
    std::fs::write(tree.path().join("ok.txt"), "ok").unwrap();
    let listener = std::os::unix::net::UnixListener::bind(tree.path().join("sock")).unwrap();
    drop(listener);
    let mut fx = Fixture::new("socket");

    let err = snapshot(&mut fx.store, tree.path(), &SnapshotOptions::default()).unwrap_err();
    match err {
        WorkspaceError::Unsupported { path, kind } => {
            assert_eq!(path, tree.path().join("sock"));
            assert!(kind.contains("mode"), "kind: {kind}");
        }
        other => panic!("expected Unsupported, got {other:?}"),
    }
}

#[test]
fn non_utf8_path_is_explicit_error() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let tree = TempDir::new("non-utf8");
    std::fs::write(tree.path().join("ok.txt"), "ok").unwrap();
    let weird = OsString::from_vec(b"bad-\xff-name".to_vec());
    std::fs::write(tree.path().join(&weird), "weird").unwrap();
    let mut fx = Fixture::new("non-utf8");

    let err = snapshot(&mut fx.store, tree.path(), &SnapshotOptions::default()).unwrap_err();
    match err {
        WorkspaceError::NonUtf8Path { path } => {
            assert_eq!(path, tree.path().join(&weird));
        }
        other => panic!("expected NonUtf8Path, got {other:?}"),
    }
}

#[test]
fn missing_snapshot_root_is_io_error() {
    let missing = TempDir::new("missing-root").path().join("does-not-exist");
    let mut fx = Fixture::new("missing-root");
    let err = snapshot(&mut fx.store, &missing, &SnapshotOptions::default()).unwrap_err();
    match err {
        WorkspaceError::Io { path, .. } => assert_eq!(path, missing),
        other => panic!("expected Io, got {other:?}"),
    }
}

// --- manifest shape --------------------------------------------------------

#[test]
fn manifest_is_canonical_schema_1_json() {
    let tree = TempDir::new("shape");
    std::fs::write(tree.path().join("a.txt"), "aaa").unwrap();
    std::os::unix::fs::symlink("a.txt", tree.path().join("link")).unwrap();
    let mut fx = Fixture::new("shape");

    let manifest = snapshot(&mut fx.store, tree.path(), &SnapshotOptions::default()).unwrap();
    let bytes = fx.store.get(&manifest).unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["schema"], 1);
    assert_eq!(parsed["entries"][0]["type"], "file");
    assert_eq!(parsed["entries"][0]["path"], "a.txt");
    assert_eq!(parsed["entries"][0]["digest"], Digest::new(b"aaa").to_string());
    assert_eq!(parsed["entries"][0]["executable"], false);
    assert_eq!(parsed["entries"][1]["type"], "symlink");
    assert_eq!(parsed["entries"][1]["path"], "link");
    assert_eq!(parsed["entries"][1]["symlink"], "a.txt");
    // bytewise-sorted entries (a.txt < link) in one canonical JSON object
    assert_eq!(
        bytes,
        format!(
            "{{\"schema\":1,\"entries\":[{{\"type\":\"file\",\"path\":\"a.txt\",\"digest\":\"{}\",\"executable\":false}},{{\"type\":\"symlink\",\"path\":\"link\",\"symlink\":\"a.txt\"}}]}}",
            Digest::new(b"aaa")
        )
        .as_bytes()
    );
}
