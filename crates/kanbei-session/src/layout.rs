//! XDG state-layout session resolution and legacy migration (decision 33/T14).
//!
//! Under a configured [`kanbei_core::StateLayout`] a session lives at
//! `sessions/<SessionId>/` with its canonical log at `events.jsonl.zst` and a
//! persisted `session.json` manifest; the module resolves which session to
//! open, creates a fresh one when none exists, and migrates a legacy
//! cwd-relative dir (`log.zst`) into the layout. With no layout every path is
//! derived from `SessionConfig::dir` as before — this module is never reached,
//! so an explicit-dir session is never inspected, migrated or manifested.
//!
//! Exactly one case migrates: a layout open whose legacy source dir (the
//! session's `SessionConfig::dir`) carries a `log.zst`. The source is copied,
//! never moved, so a failed or repeated migration cannot lose it.

use std::io;
use std::path::Path;

use kanbei_core::id::Id128;
use kanbei_core::StateLayout;
use serde::{Deserialize, Serialize};

use crate::recovery::{recover_bound_project, recover_session_id};
use crate::SessionError;

/// The manifest schema this build reads and writes.
pub(crate) const SESSION_MANIFEST_SCHEMA: u32 = 1;

/// The manifest file name inside a session dir.
pub(crate) const MANIFEST_NAME: &str = "session.json";

/// The canonical log file name under the layout.
pub(crate) const SESSION_LOG_NAME: &str = "events.jsonl.zst";

/// The legacy cwd-relative log name.
const LEGACY_LOG_NAME: &str = "log.zst";

/// The persisted session identity (decision 33): the record that today is
/// recovered post-hoc from log markers. `created_us` mirrors the log frame
/// metadata (`kanbei_log::Meta::created_us`) of the first frame, so no new
/// clock field is invented; it is `0` until the first frame is committed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct SessionManifest {
    pub schema: u32,
    pub session: Id128,
    pub log: String,
    #[serde(default)]
    pub created_us: u64,
    /// The bound project (the `project_bound` fact's subject), so a resume
    /// re-binds it without the caller naming it again. `None` = unbound, and
    /// a caller-supplied `SessionConfig::project` always wins over this.
    #[serde(default)]
    pub project: Option<Id128>,
}

/// The session a layout open resolved: its identity, plus the project its
/// manifest persisted (the caller's own binding takes precedence).
pub(crate) struct ResolvedSession {
    pub id: Id128,
    pub project: Option<Id128>,
}

/// Resolves which session a layout open targets.
///
/// Rule, in order:
/// 1. A legacy dir carrying `log.zst` is the source of truth: it is migrated
///    (idempotently) and its recovered id wins.
/// 2. An explicit `requested` id wins.
/// 3. Otherwise the most recently created session — greatest manifest
///    `created_us`, ties broken by the greatest id text — is resumed;
///    with no session present a fresh id is generated.
///
/// The project each branch reports is the manifest's: a caller-supplied
/// `SessionConfig::project` overrides it in `Session::open`.
pub(crate) fn resolve_and_migrate(
    layout: &StateLayout,
    requested: Option<Id128>,
    legacy_dir: &Path,
    memory_root: &Path,
) -> Result<ResolvedSession, SessionError> {
    if legacy_dir.join(LEGACY_LOG_NAME).is_file() {
        let id = migrate_legacy(layout, legacy_dir, memory_root)?;
        return Ok(ResolvedSession {
            id,
            project: manifest_project(layout, id)?,
        });
    }
    if let Some(id) = requested {
        return Ok(ResolvedSession {
            id,
            project: manifest_project(layout, id)?,
        });
    }
    Ok(scan_sessions(layout)?.unwrap_or(ResolvedSession {
        id: Id128::generate(),
        project: None,
    }))
}

/// The project a session's manifest persisted, if any.
fn manifest_project(layout: &StateLayout, session: Id128) -> Result<Option<Id128>, SessionError> {
    Ok(read_manifest(&layout.session_manifest(session))?.and_then(|m| m.project))
}

/// Picks the most recently created session, if any. A session dir without a
/// manifest is a migration still in flight: it is skipped, never opened half
/// written.
fn scan_sessions(layout: &StateLayout) -> Result<Option<ResolvedSession>, SessionError> {
    let sessions = layout.root().join("sessions");
    let entries = match std::fs::read_dir(&sessions) {
        Ok(entries) => entries,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let mut best: Option<(u64, String, ResolvedSession)> = None;
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let Some(manifest) = read_manifest(&entry.path().join(MANIFEST_NAME))? else {
            continue;
        };
        let candidate = (manifest.created_us, manifest.session.to_string());
        let better = match &best {
            None => true,
            Some((created, text, _)) => candidate > (*created, text.clone()),
        };
        if better {
            best = Some((
                candidate.0,
                candidate.1,
                ResolvedSession {
                    id: manifest.session,
                    project: manifest.project,
                },
            ));
        }
    }
    Ok(best.map(|(_, _, resolved)| resolved))
}

/// Reads a session manifest. Absent is `None`. A manifest that exists but does
/// not decode, or carries an unknown schema, is corruption and fails loud
/// rather than resurrecting or silently dropping a session.
fn read_manifest(path: &Path) -> Result<Option<SessionManifest>, SessionError> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.into()),
    };
    let manifest: SessionManifest = serde_json::from_str(&text).map_err(|e| {
        SessionError::CorruptRecord(format!("session manifest {}: {e}", path.display()))
    })?;
    if manifest.schema != SESSION_MANIFEST_SCHEMA {
        return Err(SessionError::CorruptRecord(format!(
            "session manifest {}: unsupported schema {}",
            path.display(),
            manifest.schema
        )));
    }
    Ok(Some(manifest))
}

/// Migrates a legacy dir into the layout. Idempotent by the target manifest:
/// a second call for an already-migrated id copies nothing. The log becomes
/// `events.jsonl.zst`, `objects/` + `state/` move into the session dir, and
/// `memory/` merges into the resolved memory root (the import copy semantics,
/// so the source is never mutated and a retried copy overwrites cleanly). The
/// manifest is written LAST, so a crash mid-copy leaves no session visible to
/// resolution and the next open resumes the migration. An ambiguous legacy
/// dir (no recoverable id) fails loud rather than guessing an identity.
fn migrate_legacy(
    layout: &StateLayout,
    source: &Path,
    memory_root: &Path,
) -> Result<Id128, SessionError> {
    let id = recover_session_id(source)?.ok_or_else(|| {
        SessionError::InvalidInput(format!(
            "legacy session dir {} carries no recoverable session id; refusing to migrate",
            source.display()
        ))
    })?;
    if layout.session_manifest(id).is_file() {
        return Ok(id);
    }
    let session_dir = layout.session_dir(id);
    std::fs::create_dir_all(&session_dir)?;
    let src_log = source.join(LEGACY_LOG_NAME);
    std::fs::copy(&src_log, layout.session_log(id)).map_err(|e| copy_error(&src_log, e))?;
    for sub in ["objects", "state"] {
        let src = source.join(sub);
        if src.is_dir() {
            copy_dir_all(&src, &session_dir.join(sub)).map_err(|e| copy_error(&src, e))?;
        }
    }
    let src_memory = source.join("memory");
    if src_memory.is_dir() {
        copy_dir_all(&src_memory, memory_root).map_err(|e| copy_error(&src_memory, e))?;
    }
    // The source's project binding survives the move (the marker the import
    // path recovers from too), so a resume re-binds it.
    write_manifest(layout, id, recover_bound_project(source)?)?;
    Ok(id)
}

/// Writes (atomically, via a temp file + rename) the manifest for `session`.
pub(crate) fn write_manifest(
    layout: &StateLayout,
    session: Id128,
    project: Option<Id128>,
) -> Result<(), SessionError> {
    let log = layout.session_log(session);
    let manifest = SessionManifest {
        schema: SESSION_MANIFEST_SCHEMA,
        session,
        log: SESSION_LOG_NAME.to_string(),
        created_us: first_frame_us(&log)?.unwrap_or(0),
        project,
    };
    let path = layout.session_manifest(session);
    let tmp = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(&manifest).expect("manifest serialization cannot fail");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

/// The `created_us` of the log's first frame; `None` for a missing/empty log
/// (a freshly created session has no frame yet).
fn first_frame_us(log: &Path) -> Result<Option<u64>, SessionError> {
    if !log.is_file() {
        return Ok(None);
    }
    let mut created = None;
    kanbei_log::for_each_frame(log, |info| {
        if created.is_none() {
            created = Some(info.meta.created_us);
        }
    })?;
    Ok(created)
}

/// Recursive directory copy (the import/migration semantics; an existing
/// target is merged, overwriting same-named files, so a retried copy is safe).
pub(crate) fn copy_dir_all(src: &Path, dst: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Names the source path in a copy failure (the bare `io::Error` does not).
fn copy_error(src: &Path, e: io::Error) -> SessionError {
    SessionError::Io(io::Error::new(
        e.kind(),
        format!("copy {}: {e}", src.display()),
    ))
}
