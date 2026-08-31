//! kanbei-workspace — content-addressed working-tree snapshots (M9 wave 4).
//!
//! A snapshot walks a workspace directory tree and installs every regular
//! file into the [`ObjectStore`] as an ordinary content-addressed object,
//! then installs one schema-versioned JSON manifest listing the tree. The
//! manifest digest is the snapshot's root digest: identical trees produce
//! identical digests (entries are bytewise-sorted by relative path, and the
//! manifest is a single canonical JSON object), so snapshots dedup
//! automatically and the digest alone pins the whole tree.
//!
//! Manifest shape (schema 1):
//! ```json
//! {"schema": 1, "entries": [
//!   {"type": "file", "path": "src/main.rs", "digest": "blake3:<64 hex>", "executable": false},
//!   {"type": "symlink", "path": "bin/run", "symlink": "../src/main.rs"}
//! ]}
//! ```
//!
//! Semantics:
//! - Directories are implicit — empty directories produce no entries (and
//!   restore does not recreate them).
//! - Symlinks are recorded as `symlink` entries without being followed
//!   (broken symlinks snapshot fine); only regular files, directories, and
//!   symlinks are supported — any other entry type (fifo, socket, device) is
//!   an explicit error, never silently skipped.
//! - Non-UTF-8 paths and unreadable files are explicit errors carrying the
//!   path; nothing is skipped silently.
//! - Restore is additive/overwrite, never a wipe: entries missing from the
//!   manifest are left untouched, files in the manifest are replaced (via
//!   temp write + rename in the target directory), and executable bits are
//!   reapplied. A restore never deletes anything outside the manifest.
//! - Symlink safety: every entry path must be relative with only `Normal`
//!   components (no absolute paths, no `..`), and the canonicalized target
//!   directory must stay under the canonicalized restore root — a symlinked
//!   intermediate directory resolving outside the root is rejected before
//!   anything is written.
//! - `ignore` is a bounded top-level-directory-name skip list (no glob
//!   patterns in this wave).

use std::io;
use std::path::{Component, Path, PathBuf};

use kanbei_core::digest::Digest;
use kanbei_objects::{ObjectError, ObjectStore};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Manifest schema version of this crate.
pub const MANIFEST_SCHEMA: u32 = 1;

/// Snapshot options: top-level directory names to skip.
///
/// Bounded by design — exact directory names only, no glob patterns.
/// Defaults to `[".git", "target"]`; supplying a custom `ignore` replaces
/// the defaults entirely.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SnapshotOptions {
    /// Top-level directory names (relative to the snapshot root) that are
    /// not walked. Files with a matching name are still snapshotted; only
    /// directories are skipped, and only at the root level.
    pub ignore: Vec<String>,
}

impl Default for SnapshotOptions {
    fn default() -> Self {
        Self {
            ignore: vec![".git".into(), "target".into()],
        }
    }
}

/// One manifest entry: either a regular file (content digest + executable
/// bit) or a symlink (its link target, as returned by `read_link`).
///
/// JSON form is tagged on `"type"` so the two shapes are unambiguous:
/// `{"type":"file",...}` versus `{"type":"symlink",...}`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Entry {
    /// A regular file. `digest` is the content digest of the bytes in the
    /// object store; `executable` is `mode & 0o111 != 0` at snapshot time.
    File {
        /// Workspace-relative path (UTF-8, `/`-separated, `Normal`
        /// components only on restore).
        path: String,
        digest: Digest,
        executable: bool,
    },
    /// A symlink; `symlink` is the link target exactly as `read_link`
    /// returned it (relative or absolute, possibly dangling).
    Symlink { path: String, symlink: String },
}

impl Entry {
    /// The workspace-relative path of the entry.
    pub fn path(&self) -> &str {
        match self {
            Entry::File { path, .. } | Entry::Symlink { path, .. } => path,
        }
    }
}

/// The canonical snapshot manifest (schema-versioned, single JSON object).
/// Field order is the canonical JSON layout (derive order).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    pub schema: u32,
    /// Entries sorted bytewise by `path` — the sort makes manifest bytes (and
    /// hence the root digest) deterministic for a given tree.
    pub entries: Vec<Entry>,
}

/// Outcome of a restore.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RestoreReport {
    /// Regular files and symlinks written (entries restored).
    pub entries_restored: u64,
    /// File bytes written (symlinks contribute 0).
    pub bytes: u64,
    /// Symlinks (re)created.
    pub symlinks: u64,
}

#[derive(Debug, Error)]
pub enum WorkspaceError {
    #[error("io error at {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("workspace manifest object missing from store: {digest}")]
    MissingObject { digest: Digest },
    #[error("object failed hash verification: {digest}")]
    Corruption { digest: Digest },
    #[error("invalid workspace manifest: {reason}")]
    InvalidManifest { reason: String },
    #[error("snapshot entry path is not valid UTF-8: {path}")]
    NonUtf8Path { path: PathBuf },
    /// Restore guard: the entry path is absolute, empty, or contains a
    /// non-`Normal` component (`..`, `.`, ...), or the canonicalized target
    /// directory resolves outside the restore root.
    #[error("restore entry path escapes the workspace root: {path:?}")]
    PathOutsideRoot { path: String },
    /// Restore hit a missing blob mid-way; `written` reports how many
    /// entries were already restored before the failure (no partial
    /// silence — the caller sees exactly where the restore stopped).
    #[error(
        "restore missing blob {digest} for entry {path} (after {written} already-restored entries)"
    )]
    MissingBlob {
        digest: Digest,
        path: String,
        written: u64,
    },
    #[error("unsupported filesystem entry at {path}: {kind}")]
    Unsupported { path: PathBuf, kind: String },
}

/// Snapshots the tree at `root` into `store`: every regular file is
/// installed as an object, then the schema-versioned manifest is installed.
/// Returns the manifest digest — the root digest of the snapshot.
///
/// `root` is canonicalized first (the recorded paths are relative to the
/// canonicalized root), and must exist. Deterministic: the same tree always
/// yields the same digest.
pub fn snapshot(
    store: &mut ObjectStore,
    root: &Path,
    options: &SnapshotOptions,
) -> Result<Digest, WorkspaceError> {
    let canon = root
        .canonicalize()
        .map_err(|source| WorkspaceError::Io {
            path: root.to_path_buf(),
            source,
        })?;
    let mut entries = Vec::new();
    walk(&canon, &canon, store, options, true, &mut entries)?;
    entries.sort_by(|a, b| a.path().cmp(b.path()));
    let manifest = Manifest {
        schema: MANIFEST_SCHEMA,
        entries,
    };
    let bytes = serde_json::to_vec(&manifest)
        .expect("workspace manifest serialization cannot fail (plain data)");
    store
        .install(&bytes)
        .map_err(|source| WorkspaceError::Io {
            path: root.to_path_buf(),
            source,
        })
}

/// Restores the tree pinned by `manifest_digest` under `root`.
///
/// Additive/overwrite semantics: directories are created as needed, manifest
/// files overwrite whatever is at their path (via temp write + rename — an
/// existing symlink at the path is replaced, not written through), symlinks
/// are recreated after removing any existing file at their path, and
/// everything not listed in the manifest is left untouched.
pub fn restore(
    store: &ObjectStore,
    manifest_digest: &Digest,
    root: &Path,
) -> Result<RestoreReport, WorkspaceError> {
    let bytes = store.get(manifest_digest).map_err(|e| match e {
        ObjectError::Missing { digest } => WorkspaceError::MissingObject { digest },
        ObjectError::Corruption { digest, .. } | ObjectError::Quota { digest, .. } => {
            WorkspaceError::Corruption { digest }
        }
        ObjectError::Io(source) => WorkspaceError::Io {
            path: store.path_for(manifest_digest),
            source,
        },
    })?;
    let manifest: Manifest = serde_json::from_slice(&bytes).map_err(|e| {
        WorkspaceError::InvalidManifest {
            reason: format!("manifest {manifest_digest}: {e}"),
        }
    })?;
    if manifest.schema != MANIFEST_SCHEMA {
        return Err(WorkspaceError::InvalidManifest {
            reason: format!(
                "manifest {manifest_digest}: unsupported schema {} (expected {MANIFEST_SCHEMA})",
                manifest.schema
            ),
        });
    }

    std::fs::create_dir_all(root).map_err(|source| WorkspaceError::Io {
        path: root.to_path_buf(),
        source,
    })?;
    let canon_root = root.canonicalize().map_err(|source| WorkspaceError::Io {
        path: root.to_path_buf(),
        source,
    })?;

    let mut report = RestoreReport::default();
    for entry in &manifest.entries {
        validate_rel_path(entry.path())?;
        // Parent dir: create, then canonicalize once — a symlinked
        // intermediate resolving outside the root is rejected before any
        // write (a `..` component was already rejected above).
        let parent = canon_root.join(Path::new(entry.path()).parent().expect("validated path has a parent"));
        std::fs::create_dir_all(&parent).map_err(|source| WorkspaceError::Io {
            path: parent.clone(),
            source,
        })?;
        let real_parent = parent
            .canonicalize()
            .map_err(|source| WorkspaceError::Io {
                path: parent.clone(),
                source,
            })?;
        if !real_parent.starts_with(&canon_root) {
            return Err(WorkspaceError::PathOutsideRoot {
                path: entry.path().into(),
            });
        }
        let file_name = Path::new(entry.path())
            .file_name()
            .expect("validated path ends in a normal component");
        let dst = real_parent.join(file_name);

        match entry {
            Entry::File {
                digest, executable, ..
            } => {
                let blob = store.get(digest).map_err(|e| match e {
                    ObjectError::Missing { digest } => WorkspaceError::MissingBlob {
                        digest,
                        path: entry.path().into(),
                        written: report.entries_restored,
                    },
                    ObjectError::Corruption { digest, .. }
                    | ObjectError::Quota { digest, .. } => WorkspaceError::Corruption { digest },
                    ObjectError::Io(source) => WorkspaceError::Io {
                        path: dst.clone(),
                        source,
                    },
                })?;
                let tmp = real_parent.join(format!(
                    ".kanbei-tmp-{}-{}",
                    std::process::id(),
                    report.entries_restored
                ));
                if let Err(source) = std::fs::write(&tmp, &blob) {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(WorkspaceError::Io { path: tmp, source });
                }
                // rename replaces an existing file or symlink at `dst`
                // without following it; an existing directory fails loudly
                if let Err(source) = std::fs::rename(&tmp, &dst) {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(WorkspaceError::Io { path: dst, source });
                }
                if *executable {
                    apply_executable(&dst)?;
                }
                report.entries_restored += 1;
                report.bytes += blob.len() as u64;
            }
            Entry::Symlink { symlink, .. } => {
                // remove any existing file/symlink at the path first
                // (rename would refuse to clobber a symlink; a directory at
                // the path fails loudly)
                match std::fs::remove_file(&dst) {
                    Ok(()) => {}
                    Err(source) if source.kind() == io::ErrorKind::NotFound => {}
                    Err(source) => return Err(WorkspaceError::Io { path: dst.clone(), source }),
                }
                std::os::unix::fs::symlink(symlink, &dst).map_err(|source| {
                    WorkspaceError::Io {
                        path: dst.clone(),
                        source,
                    }
                })?;
                report.entries_restored += 1;
                report.symlinks += 1;
            }
        }
    }
    Ok(report)
}

/// Adds the execute bits (0o111) to `path`'s permissions. Temp files are
/// created without execute bits, so only manifest-executable entries need
/// this — non-executable entries keep the default 0o644.
fn apply_executable(path: &Path) -> Result<(), WorkspaceError> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)
        .map_err(|source| WorkspaceError::Io {
            path: path.to_path_buf(),
            source,
        })?
        .permissions();
    perms.set_mode(perms.mode() | 0o111);
    std::fs::set_permissions(path, perms).map_err(|source| WorkspaceError::Io {
        path: path.to_path_buf(),
        source,
    })
}

/// Restore guard: entry paths must be relative with only `Normal`
/// components — empty paths, absolute paths, and `..`/`.` components are all
/// rejected before any filesystem interaction.
fn validate_rel_path(path: &str) -> Result<(), WorkspaceError> {
    if path.is_empty() || Path::new(path).is_absolute() {
        return Err(WorkspaceError::PathOutsideRoot { path: path.into() });
    }
    for component in Path::new(path).components() {
        if !matches!(component, Component::Normal(_)) {
            return Err(WorkspaceError::PathOutsideRoot { path: path.into() });
        }
    }
    Ok(())
}

/// Recursive walk of `dir` (under the canonical `root`). Children are
/// processed in bytewise file-name order; the collected entries are sorted
/// globally by the caller. `is_root` gates the `ignore` skip list (top-level
/// directory names only).
fn walk(
    dir: &Path,
    root: &Path,
    store: &mut ObjectStore,
    options: &SnapshotOptions,
    is_root: bool,
    out: &mut Vec<Entry>,
) -> Result<(), WorkspaceError> {
    let mut children: Vec<_> = std::fs::read_dir(dir)
        .map_err(|source| WorkspaceError::Io {
            path: dir.to_path_buf(),
            source,
        })?
        .collect::<Result<_, _>>()
        .map_err(|source| WorkspaceError::Io {
            path: dir.to_path_buf(),
            source,
        })?;
    children.sort_by_key(|child| child.file_name());

    for child in children {
        let path = child.path();
        let name = child
            .file_name()
            .to_str()
            .ok_or_else(|| WorkspaceError::NonUtf8Path { path: path.clone() })?
            .to_string();
        let file_type = child.file_type().map_err(|source| WorkspaceError::Io {
            path: path.clone(),
            source,
        })?;
        let rel = path
            .strip_prefix(root)
            .expect("walk never leaves the canonical root")
            .to_str()
            .expect("file names are validated UTF-8 above")
            .to_string();

        if file_type.is_dir() {
            if is_root && options.ignore.iter().any(|ignored| ignored == &name) {
                continue;
            }
            walk(&path, root, store, options, false, out)?;
        } else if file_type.is_file() {
            let bytes = std::fs::read(&path).map_err(|source| WorkspaceError::Io {
                path: path.clone(),
                source,
            })?;
            let digest = store
                .install(&bytes)
                .map_err(|source| WorkspaceError::Io {
                    path: path.clone(),
                    source,
                })?;
            let executable = {
                use std::os::unix::fs::PermissionsExt;
                std::fs::metadata(&path)
                    .map_err(|source| WorkspaceError::Io {
                        path: path.clone(),
                        source,
                    })?
                    .permissions()
                    .mode()
                    & 0o111
                    != 0
            };
            out.push(Entry::File {
                path: rel,
                digest,
                executable,
            });
        } else if file_type.is_symlink() {
            let target = std::fs::read_link(&path).map_err(|source| WorkspaceError::Io {
                path: path.clone(),
                source,
            })?;
            let target = target
                .to_str()
                .ok_or_else(|| WorkspaceError::NonUtf8Path {
                    path: path.clone(),
                })?
                .to_string();
            out.push(Entry::Symlink {
                path: rel,
                symlink: target,
            });
        } else {
            // FileType's Debug form doesn't name the kind (e.g. a socket
            // prints only `is_file: false, ...`), so include the raw mode
            let mode = {
                use std::os::unix::fs::PermissionsExt;
                std::fs::symlink_metadata(&path)
                    .map_err(|source| WorkspaceError::Io {
                        path: path.clone(),
                        source,
                    })?
                    .permissions()
                    .mode()
            };
            return Err(WorkspaceError::Unsupported {
                path,
                kind: format!("{file_type:?} (mode {mode:o})"),
            });
        }
    }
    Ok(())
}
