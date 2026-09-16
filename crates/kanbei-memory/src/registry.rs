//! The ProjectId locator registry: the canonical project-registry stream.
//! The registry only records entries; creating the project scope directory
//! happens in [`MemoryRootActor::open`](crate::MemoryRootActor::open).
//!
//! One API, two on-disk forms. Under an XDG state layout (decision 33/T14) the
//! registry is an append log at `layout.projects_log()`
//! (`<state>/memory/projects/events.jsonl.zst`), holding the same
//! `kanbei_log::AppendLog` framing as the session and memory streams: one
//! envelope per registration, kind [`PROJECT_REGISTERED_KIND`], the
//! [`ProjectEntry`] as the payload. An explicit-dir (legacy) session keeps the
//! plain append-only JSONL at `<memory_root>/projects.jsonl`.
//!
//! Each entry is wrapped in an [`Envelope`] rather than the log being
//! generalized to un-typed records: the frozen frame format, the seq chain and
//! the digest verification stay in force for a stream that is not the session
//! log, and the memory scope transition logs already store non-event records
//! this way. The payload is `serde_json::Value`, so `ProjectEntry` owes nothing
//! to `Envelope`, and the record is deterministic — the seq is the stream's
//! own (no clock) and `evt` is derived from the entry's `project_id`.

use std::collections::HashSet;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kanbei_core::envelope::{ENVELOPE_SCHEMA, Envelope};
use kanbei_core::queue::DurabilityQueue;
use kanbei_core::{Id128, StateLayout};
use kanbei_log::{AppendLog, Profile};
use serde::{Deserialize, Serialize};

use crate::error::MemoryError;

pub const PROJECT_ENTRY_SCHEMA: u32 = 1;

/// The canonical AppendLog stream name of the layout registry.
pub const PROJECTS_STREAM: &str = "project-registry";

/// The envelope `kind` of one registration record.
const PROJECT_REGISTERED_KIND: &str = "project_registered";

/// The legacy registry file name under `<memory_root>/`.
const LEGACY_REGISTRY_NAME: &str = "projects.jsonl";

/// One project registration: one JSON object per line (plain JSONL), or one
/// envelope payload (append log).
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct ProjectEntry {
    pub schema: u32,
    /// `pro_`-branded id.
    pub project_id: Id128,
    pub name: String,
    /// The project scope directory name under `<memory_root>/`, e.g.
    /// "projects/<base58 ProjectId>".
    pub dir: String,
    pub created_session: Id128,
    pub created_event: u64,
}

/// The append-only project registry (see the module docs for the two forms).
/// `open`/`open_under` create the parent directory; `register` appends one
/// record (duplicate `project_id` rejected), `list` re-reads the whole stream
/// (corruption is an explicit [`MemoryError::Corrupt`], or the log's own
/// [`MemoryError::Log`] when a frame fails verification).
pub struct ProjectRegistry {
    backend: Backend,
}

enum Backend {
    /// Explicit-path form: plain JSONL at the caller's path (legacy).
    Jsonl(PathBuf),
    /// Layout form: the canonical append log at `projects/events.jsonl.zst`.
    Log(PathBuf),
}

impl ProjectRegistry {
    /// Opens the plain-JSONL registry at an explicit path — the form an
    /// explicit-dir session keeps, byte-identical to the pre-layout one.
    pub fn open(path: &Path) -> Result<Self, MemoryError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self {
            backend: Backend::Jsonl(path.to_path_buf()),
        })
    }

    /// Opens the layout registry: the append log at `layout.projects_log()`,
    /// with `<memory_root>/projects.jsonl` migrated into it first when present
    /// (see [`migrate`]). The log is the registry's only read path afterwards.
    pub fn open_under(layout: &StateLayout, memory_root: &Path) -> Result<Self, MemoryError> {
        let log_path = layout.projects_log();
        if let Some(parent) = log_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        migrate(&memory_root.join(LEGACY_REGISTRY_NAME), &log_path)?;
        Ok(Self {
            backend: Backend::Log(log_path),
        })
    }

    /// Appends `entry`, rejecting a duplicate `project_id` (the id is the
    /// registry key). The record is durable before returning.
    pub fn register(&mut self, entry: ProjectEntry) -> Result<(), MemoryError> {
        if entry.schema != PROJECT_ENTRY_SCHEMA {
            return Err(MemoryError::InvalidInput(format!(
                "project entry schema {}, expected {PROJECT_ENTRY_SCHEMA}",
                entry.schema
            )));
        }
        if self.lookup(entry.project_id)?.is_some() {
            return Err(MemoryError::InvalidInput(format!(
                "duplicate project registration: {entry:?}",
            )));
        }
        match &self.backend {
            Backend::Jsonl(file) => {
                let line = serde_json::to_string(&entry).map_err(|e| {
                    MemoryError::InvalidInput(format!("project entry serialization: {e}"))
                })?;
                let mut f = File::options().append(true).create(true).open(file)?;
                writeln!(f, "{line}")?;
                // canonical stream: the entry is durable before register acks
                // (sync_data is the content; the parent dir already exists)
                f.sync_data()?;
                Ok(())
            }
            Backend::Log(path) => append_entries(path, std::slice::from_ref(&entry)),
        }
    }

    /// The entry for `project_id`, or `None` when not registered.
    pub fn lookup(&self, project_id: Id128) -> Result<Option<ProjectEntry>, MemoryError> {
        Ok(self
            .list()?
            .into_iter()
            .find(|e| e.project_id == project_id))
    }

    /// All registered entries in stream order. A missing stream is an empty
    /// registry; an unparseable JSONL line is [`MemoryError::Corrupt`] naming
    /// the line number, and an unverifiable log frame is [`MemoryError::Log`].
    pub fn list(&self) -> Result<Vec<ProjectEntry>, MemoryError> {
        match &self.backend {
            Backend::Jsonl(file) => read_jsonl(file),
            Backend::Log(path) => read_log(path),
        }
    }
}

/// The [`ProjectEntry`] a canonical registry record carries, or `None` when
/// the line is not a registration (a record of another kind, or an
/// undecodable one). The lenient counterpart of [`read_log`], for identity
/// recovery, which reads the `created_session` marker best-effort.
pub fn entry_of_record(line: &str) -> Option<ProjectEntry> {
    decode_record(line).ok().flatten()
}

/// Decodes one registry record. `Ok(None)` is a record of another kind (the
/// stream is the project registry's, and future locator observations append
/// here); a `project_registered` record whose payload does not decode or
/// carries an unknown schema is corruption.
fn decode_record(line: &str) -> Result<Option<ProjectEntry>, MemoryError> {
    let env = Envelope::from_line(line).map_err(|e| MemoryError::Corrupt {
        context: format!("registry envelope: {e}"),
    })?;
    if env.kind != PROJECT_REGISTERED_KIND {
        return Ok(None);
    }
    let entry: ProjectEntry =
        serde_json::from_value(env.payload).map_err(|e| MemoryError::Corrupt {
            context: format!("project_registered payload: {e}"),
        })?;
    if entry.schema != PROJECT_ENTRY_SCHEMA {
        return Err(MemoryError::Corrupt {
            context: format!(
                "project entry schema {}, expected {PROJECT_ENTRY_SCHEMA}",
                entry.schema
            ),
        });
    }
    Ok(Some(entry))
}

/// The envelope one registration becomes. `seq` is the registry stream's own
/// counter, handed in by the writer.
fn envelope(seq: u64, entry: &ProjectEntry) -> Result<Envelope, MemoryError> {
    let payload = serde_json::to_value(entry)
        .map_err(|e| MemoryError::InvalidInput(format!("project entry serialization: {e}")))?;
    Ok(Envelope {
        env: ENVELOPE_SCHEMA,
        seq,
        evt: format!("{PROJECT_REGISTERED_KIND}:{}", entry.project_id),
        kind: PROJECT_REGISTERED_KIND.to_string(),
        payload_schema: PROJECT_ENTRY_SCHEMA,
        payload,
        refs: Vec::new(),
        snapshot: None,
    })
}

/// Appends `entries` as one frame at the end of the log stream. The torn tail
/// is truncated first (a crash between write and fsync), and the append is
/// Strict — the ack implies durable, matching the legacy `sync_data`.
fn append_entries(log_path: &Path, entries: &[ProjectEntry]) -> Result<(), MemoryError> {
    if entries.is_empty() {
        return Ok(());
    }
    match std::fs::metadata(log_path) {
        Ok(m) if m.is_file() => {
            kanbei_log::recover(log_path)?;
        }
        Ok(_) => {
            return Err(MemoryError::InvalidInput(format!(
                "registry log path is not a file: {}",
                log_path.display()
            )));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    let queue = Arc::new(DurabilityQueue::start("project-registry"));
    let mut log = match AppendLog::open(log_path, PROJECTS_STREAM, Arc::clone(&queue)) {
        Ok(log) => log,
        Err(e) => {
            let _ = shutdown_queue(queue);
            return Err(e.into());
        }
    };
    let mut envelopes = Vec::with_capacity(entries.len());
    for (i, entry) in entries.iter().enumerate() {
        match envelope(log.seq() + i as u64, entry) {
            Ok(env) => envelopes.push(env),
            Err(e) => {
                drop(log);
                let _ = shutdown_queue(queue);
                return Err(e);
            }
        }
    }
    let appended = log.append(&envelopes, Profile::Strict);
    drop(log);
    let shutdown = shutdown_queue(queue);
    match (appended, shutdown) {
        (Err(e), _) => Err(e.into()),
        (Ok(_), Err(e)) => Err(e.into()),
        (Ok(_), Ok(())) => Ok(()),
    }
}

/// Best-effort worker shutdown: only reachable with no other Arc clone left.
fn shutdown_queue(queue: Arc<DurabilityQueue>) -> std::io::Result<()> {
    match Arc::try_unwrap(queue) {
        Ok(queue) => queue.shutdown(),
        Err(_) => Ok(()),
    }
}

/// Reads the whole log stream. A missing stream is an empty registry; a torn
/// frame is left to the writer's [`append_entries`] (a reader never truncates).
fn read_log(path: &Path) -> Result<Vec<ProjectEntry>, MemoryError> {
    match std::fs::metadata(path) {
        Ok(m) if m.is_file() => {}
        Ok(_) => {
            return Err(MemoryError::InvalidInput(format!(
                "registry log path is not a file: {}",
                path.display()
            )));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
    }
    let mut out = Vec::new();
    let mut first_err: Option<MemoryError> = None;
    kanbei_log::for_each_frame(path, |frame| {
        if first_err.is_some() {
            return;
        }
        for line in &frame.events {
            match decode_record(line) {
                Ok(Some(entry)) => out.push(entry),
                Ok(None) => {}
                Err(e) => {
                    first_err = Some(e);
                    return;
                }
            }
        }
    })?;
    match first_err {
        Some(e) => Err(e),
        None => Ok(out),
    }
}

/// Migrates a legacy plain-JSONL registry into the log: every entry whose
/// `project_id` is not already in the stream is appended, in file order, in
/// one frame. Idempotent and crash-safe by that filter — a re-run appends
/// nothing, and an interrupted migration is completed by the next open. The
/// JSONL file is never deleted: it is the entries' only copy until the append
/// returns, so it is left for the operator, and (being the migration source,
/// not a read path) it can never shadow the log.
fn migrate(legacy: &Path, log_path: &Path) -> Result<(), MemoryError> {
    if !legacy.is_file() {
        return Ok(());
    }
    let mut seen: HashSet<Id128> = read_log(log_path)?
        .into_iter()
        .map(|e| e.project_id)
        .collect();
    let pending: Vec<ProjectEntry> = read_jsonl(legacy)?
        .into_iter()
        .filter(|e| seen.insert(e.project_id))
        .collect();
    append_entries(log_path, &pending)
}

/// Reads a plain-JSONL registry. A missing file is an empty registry; an
/// unparseable line is [`MemoryError::Corrupt`] naming the line number.
fn read_jsonl(file: &Path) -> Result<Vec<ProjectEntry>, MemoryError> {
    let bytes = match std::fs::read(file) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.into()),
        Ok(bytes) => bytes,
    };
    let text = std::str::from_utf8(&bytes).map_err(|e| MemoryError::Corrupt {
        context: format!("{}: not utf-8: {e}", file.display()),
    })?;
    let mut out = Vec::new();
    let total = text.lines().count();
    for (idx, line) in text.lines().enumerate() {
        let line_no = idx + 1;
        let entry: ProjectEntry = match serde_json::from_str(line) {
            Ok(entry) => entry,
            // Torn final line (crash between write and flush): the
            // registry is the writer's own file — the last line is
            // dropped exactly like an append-log torn tail, instead of
            // bricking every future `list()` with Corrupt.
            Err(_e) if idx + 1 == total && !text.ends_with('\n') => break,
            Err(e) => {
                return Err(MemoryError::Corrupt {
                    context: format!("{} line {line_no}: {e}", file.display()),
                })
            }
        };
        if entry.schema != PROJECT_ENTRY_SCHEMA {
            return Err(MemoryError::Corrupt {
                context: format!(
                    "{} line {line_no}: schema {}, expected {PROJECT_ENTRY_SCHEMA}",
                    file.display(),
                    entry.schema
                ),
            });
        }
        out.push(entry);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp_file(tag: &str) -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("kb-memory-registry-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_file(&p);
        p
    }

    fn tmp_dir(tag: &str) -> PathBuf {
        let p =
            std::env::temp_dir().join(format!("kb-memory-registry-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        std::fs::create_dir_all(&p).unwrap();
        p
    }

    fn entry(project_id: Id128, name: &str) -> ProjectEntry {
        ProjectEntry {
            schema: PROJECT_ENTRY_SCHEMA,
            project_id,
            name: name.into(),
            dir: format!("projects/{project_id}"),
            created_session: Id128::generate(),
            created_event: 7,
        }
    }

    fn write_jsonl(path: &Path, entries: &[ProjectEntry]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut text = String::new();
        for entry in entries {
            text.push_str(&serde_json::to_string(entry).unwrap());
            text.push('\n');
        }
        std::fs::write(path, text).unwrap();
    }

    /// (c) The explicit-path (no layout) registry is untouched: plain JSONL,
    /// one object per line, no framing.
    #[test]
    fn register_lookup_list() {
        let path = tmp_file("lifecycle");
        let mut reg = ProjectRegistry::open(&path).unwrap();
        assert_eq!(reg.list().unwrap(), Vec::<ProjectEntry>::new());

        let a = entry(Id128::generate(), "alpha");
        let b = entry(Id128::generate(), "beta");
        reg.register(a.clone()).unwrap();
        reg.register(b.clone()).unwrap();
        assert_eq!(reg.lookup(a.project_id).unwrap(), Some(a.clone()));
        assert_eq!(reg.lookup(b.project_id).unwrap(), Some(b.clone()));
        assert_eq!(reg.lookup(Id128::generate()).unwrap(), None);
        assert_eq!(reg.list().unwrap(), vec![a.clone(), b.clone()]);

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert_eq!(serde_json::from_str::<ProjectEntry>(lines[0]).unwrap(), a);
        assert_eq!(serde_json::from_str::<ProjectEntry>(lines[1]).unwrap(), b);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn duplicate_rejected() {
        let path = tmp_file("duplicate");
        let mut reg = ProjectRegistry::open(&path).unwrap();
        let a = entry(Id128::generate(), "alpha");
        reg.register(a.clone()).unwrap();
        let err = reg.register(a.clone()).unwrap_err();
        assert!(matches!(err, MemoryError::InvalidInput(_)));
        // The file still holds one line.
        assert_eq!(reg.list().unwrap().len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn corrupt_line_is_explicit() {
        let path = tmp_file("corrupt");
        let mut reg = ProjectRegistry::open(&path).unwrap();
        reg.register(entry(Id128::generate(), "alpha")).unwrap();
        // Append a garbage line after the good one.
        let mut f = File::options().append(true).open(&path).unwrap();
        writeln!(f, "not json at all").unwrap();
        drop(f);
        match reg.list().unwrap_err() {
            MemoryError::Corrupt { context } => {
                assert!(context.contains("line 2"), "context: {context}");
            }
            other => panic!("expected Corrupt, got {other:?}"),
        }
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn reopen_sees_registered_entries() {
        let path = tmp_file("reopen");
        {
            let mut reg = ProjectRegistry::open(&path).unwrap();
            reg.register(entry(Id128::generate(), "alpha")).unwrap();
        }
        let reg = ProjectRegistry::open(&path).unwrap();
        assert_eq!(reg.list().unwrap().len(), 1);
        let _ = std::fs::remove_file(&path);
    }

    /// (a) Under a layout registrations land in the append log at the layout
    /// path and read back.
    #[test]
    fn layout_registry_is_the_append_log() {
        let root = tmp_dir("layout");
        let layout = StateLayout::new(&root);
        let memory_root = layout.memory_root();
        let a = entry(Id128::generate(), "alpha");
        let b = entry(Id128::generate(), "beta");
        {
            let mut reg = ProjectRegistry::open_under(&layout, &memory_root).unwrap();
            reg.register(a.clone()).unwrap();
            reg.register(b.clone()).unwrap();
        }

        let log_path = layout.projects_log();
        assert!(log_path.is_file(), "registry stream at {}", log_path.display());
        assert!(!memory_root.join("projects.jsonl").exists());
        let recovered = kanbei_log::recover(&log_path).unwrap();
        assert_eq!(recovered.events, 2, "one record per registration");

        let reg = ProjectRegistry::open_under(&layout, &memory_root).unwrap();
        assert_eq!(reg.list().unwrap(), vec![a.clone(), b.clone()]);
        assert_eq!(reg.lookup(a.project_id).unwrap(), Some(a));
        assert_eq!(reg.lookup(Id128::generate()).unwrap(), None);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// (d) Duplicate rejection holds on the log form too.
    #[test]
    fn duplicate_rejected_in_the_log() {
        let root = tmp_dir("layout-duplicate");
        let layout = StateLayout::new(&root);
        let mut reg = ProjectRegistry::open_under(&layout, &layout.memory_root()).unwrap();
        let a = entry(Id128::generate(), "alpha");
        reg.register(a.clone()).unwrap();
        let err = reg.register(a.clone()).unwrap_err();
        assert!(matches!(err, MemoryError::InvalidInput(_)));
        assert_eq!(reg.list().unwrap(), vec![a]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// (b) An existing `projects.jsonl` migrates into the log with no data
    /// loss, idempotently, and later registrations append after it.
    #[test]
    fn legacy_jsonl_migrates_into_the_log_once() {
        let root = tmp_dir("migrate");
        let layout = StateLayout::new(&root);
        let memory_root = layout.memory_root();
        let legacy = memory_root.join("projects.jsonl");
        let a = entry(Id128::generate(), "alpha");
        let b = entry(Id128::generate(), "beta");
        write_jsonl(&legacy, &[a.clone(), b.clone()]);

        let reg = ProjectRegistry::open_under(&layout, &memory_root).unwrap();
        assert_eq!(reg.list().unwrap(), vec![a.clone(), b.clone()]);
        assert_eq!(kanbei_log::recover(&layout.projects_log()).unwrap().events, 2);
        // The source is left in place (it is the entries' only copy until the
        // append returns), and the migration is idempotent.
        assert!(legacy.is_file());

        let mut again = ProjectRegistry::open_under(&layout, &memory_root).unwrap();
        assert_eq!(again.list().unwrap(), vec![a.clone(), b.clone()]);
        assert_eq!(kanbei_log::recover(&layout.projects_log()).unwrap().events, 2);

        let c = entry(Id128::generate(), "gamma");
        again.register(c.clone()).unwrap();
        let reopened = ProjectRegistry::open_under(&layout, &memory_root).unwrap();
        assert_eq!(reopened.list().unwrap(), vec![a, b, c]);
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A registry record of an unknown kind is skipped, not corruption: the
    /// stream grows new locator-observation kinds without bricking the reader.
    #[test]
    fn unknown_record_kind_is_skipped() {
        let root = tmp_dir("unknown-kind");
        let layout = StateLayout::new(&root);
        let memory_root = layout.memory_root();
        let a = entry(Id128::generate(), "alpha");
        {
            let mut reg = ProjectRegistry::open_under(&layout, &memory_root).unwrap();
            reg.register(a.clone()).unwrap();
        }
        let foreign = Envelope {
            env: ENVELOPE_SCHEMA,
            seq: 2,
            evt: "project_linked:1".into(),
            kind: "project_linked".into(),
            payload_schema: 1,
            payload: serde_json::json!({"project_id": a.project_id}),
            refs: Vec::new(),
            snapshot: None,
        };
        let queue = Arc::new(DurabilityQueue::start("test-registry"));
        {
            let mut log =
                AppendLog::open(&layout.projects_log(), PROJECTS_STREAM, Arc::clone(&queue))
                    .unwrap();
            log.append(&[foreign], Profile::Strict).unwrap();
        }
        shutdown_queue(queue).unwrap();

        let reg = ProjectRegistry::open_under(&layout, &memory_root).unwrap();
        assert_eq!(reg.list().unwrap(), vec![a]);
        let _ = std::fs::remove_dir_all(&root);
    }
}
