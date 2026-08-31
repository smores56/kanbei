//! The ProjectId locator registry: the canonical project-registry stream,
//! append-only JSONL at `<memory_root>/projects.jsonl`. The registry only
//! records entries; creating the project scope directory happens in
//! [`MemoryRootActor::open`](crate::MemoryRootActor::open).

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use kanbei_core::Id128;
use serde::{Deserialize, Serialize};

use crate::error::MemoryError;

pub const PROJECT_ENTRY_SCHEMA: u32 = 1;

/// One project registration: one JSON object per line.
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

/// The append-only project registry. `open` creates the parent directory;
/// `register` appends one line (duplicate `project_id` rejected), `list`
/// re-reads and parses the whole file (a corrupt line is an explicit
/// [`MemoryError::Corrupt`] with the line number).
pub struct ProjectRegistry {
    file: PathBuf,
}

impl ProjectRegistry {
    pub fn open(path: &Path) -> Result<Self, MemoryError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        Ok(Self {
            file: path.to_path_buf(),
        })
    }

    /// Appends `entry`, rejecting a duplicate `project_id` (the id is the
    /// registry key). The line is flushed before returning.
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
        let line = serde_json::to_string(&entry)
            .map_err(|e| MemoryError::InvalidInput(format!("project entry serialization: {e}")))?;
        let mut f = File::options().append(true).create(true).open(&self.file)?;
        writeln!(f, "{line}")?;
        // canonical stream: the entry is durable before register acks
        // (sync_data is the content; the parent dir already exists)
        f.sync_data()?;
        Ok(())
    }

    /// The entry for `project_id`, or `None` when not registered.
    pub fn lookup(&self, project_id: Id128) -> Result<Option<ProjectEntry>, MemoryError> {
        Ok(self
            .list()?
            .into_iter()
            .find(|e| e.project_id == project_id))
    }

    /// All registered entries in file order. A missing file is an empty
    /// registry; an unparseable line is [`MemoryError::Corrupt`] naming the
    /// line number.
    pub fn list(&self) -> Result<Vec<ProjectEntry>, MemoryError> {
        let bytes = match std::fs::read(&self.file) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e.into()),
            Ok(bytes) => bytes,
        };
        let text = std::str::from_utf8(&bytes).map_err(|e| MemoryError::Corrupt {
            context: format!("{}: not utf-8: {e}", self.file.display()),
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
                        context: format!("{} line {line_no}: {e}", self.file.display()),
                    })
                }
            };
            if entry.schema != PROJECT_ENTRY_SCHEMA {
                return Err(MemoryError::Corrupt {
                    context: format!(
                        "{} line {line_no}: schema {}, expected {PROJECT_ENTRY_SCHEMA}",
                        self.file.display(),
                        entry.schema
                    ),
                });
            }
            out.push(entry);
        }
        Ok(out)
    }
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
        assert_eq!(reg.list().unwrap(), vec![a, b]);
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
}
