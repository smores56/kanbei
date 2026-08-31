//! kanbei-objects — the per-session content-addressed object store
//! (ratification-packet §3, §8.3): flat `blake3:<64 hex>` filenames, install
//! via temp write + rename with a queued dirsync (no per-object temp fsync —
//! hash verification detects damage), hash-verified reads, prune scan.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use kanbei_core::digest::Digest;
use kanbei_core::queue::{DurabilityQueue, SyncOp};
use thiserror::Error;

/// Hard per-object size quota (R-22: closes part of `ledger:572`).
/// Install rejects larger payloads before any write; reads classify
/// on-disk overshoot as corruption-classified quota violation. Sized at
/// 64 MiB — comfortably above any single session payload the MVP tool
/// surface produces (the MAX_STATE_BYTES head quota is 1 MiB) while
/// bounding a hostile/artifacted store.
pub const MAX_OBJECT_BYTES: usize = 64 * 1024 * 1024;

/// Current time since the epoch (quarantine clock start).
fn quarantine_clock() -> std::time::SystemTime {
    std::time::SystemTime::now()
}

/// Sets a file's mtime via std (no new dependency): the quarantine mtime
/// IS the grace clock, so quarantine-begin is stamped over the rename's
/// preserved original mtime.
fn stamp_mtime(path: &std::path::Path, at: std::time::SystemTime) {
    if let Ok(f) = std::fs::OpenOptions::new().write(true).open(path) {
        let _ = f.set_modified(at);
    }
}

/// Per-session content-addressed object store.
///
/// Objects live flat in one directory, named `<digest display>` (e.g.
/// `blake3:<64 hex>` — the colon is part of the filename). Install is
/// temp-write + atomic rename; durability is delegated to the shared
/// [`DurabilityQueue`] (a `Dirsync` per install, never a synchronous fsync).
pub struct ObjectStore {
    dir: PathBuf,
    queue: Arc<DurabilityQueue>,
    /// Objects written (dedup hits don't count).
    pub installs: u64,
    /// Dirsyncs enqueued (one per non-dedup install).
    pub dirsyncs: u64,
}

impl ObjectStore {
    /// Opens (creating if missing) the store directory.
    pub fn open(dir: &Path, queue: Arc<DurabilityQueue>) -> io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Self {
            dir: dir.to_path_buf(),
            queue,
            installs: 0,
            dirsyncs: 0,
        })
    }

    /// `<dir>/<digest display>` — flat, no sharding.
    pub fn path_for(&self, digest: &Digest) -> PathBuf {
        self.dir.join(digest.to_string())
    }

    /// Content-addressed install: dedup on existing file; otherwise temp
    /// write + atomic rename, then enqueue file-data fsync + dirsync on the
    /// shared queue (R-10/B-03: fsync temp, rename, fsync parent directory).
    /// The data fsync keeps the file's fd open across the rename — syncing
    /// the still-open inode flushes the content the same queue slot the
    /// dirsync makes the name durable in; both land ahead of the
    /// referencing event frame's fsync (one FIFO queue). Without it a
    /// power loss could leave a committed event referencing a renamed-but-
    /// unwritten object (hash-verify detects, but the contract promises
    /// prevention, architecture.md:373).
    /// Objects above the hard per-object quota (R-22) are rejected before
    /// any write.
    pub fn install(&mut self, bytes: &[u8]) -> io::Result<Digest> {
        if bytes.len() > MAX_OBJECT_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "object payload {} exceeds the per-object quota {}",
                    bytes.len(),
                    MAX_OBJECT_BYTES
                ),
            ));
        }
        let digest = Digest::new(bytes);
        let dst = self.path_for(&digest);
        if dst.exists() {
            return Ok(digest);
        }
        let tmp = self
            .dir
            .join(format!(".tmp-{}-{}", std::process::id(), self.installs));
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        std::fs::rename(&tmp, &dst)?;
        self.queue.enqueue(SyncOp::Fsync(f))?;
        self.installs += 1;
        self.queue.enqueue(SyncOp::Dirsync(self.dir.clone()))?;
        self.dirsyncs += 1;
        Ok(digest)
    }

    /// Waits until every enqueued durability op (incl. our fsync/dirsyncs) ran.
    pub fn flush(&self) -> io::Result<()> {
        self.queue.flush()
    }

    /// Reads an object and verifies its hash. Never returns unverified bytes.
    /// Objects are quota-bounded at install; an on-disk object exceeding the
    /// quota is classified corruption (an externally planted or hand-edited
    /// file), never silently read.
    pub fn get(&self, want: &Digest) -> Result<Vec<u8>, ObjectError> {
        let mut bytes = Vec::new();
        File::open(self.path_for(want))
            .and_then(|mut f| f.read_to_end(&mut bytes))
            .map_err(|e| match e.kind() {
                io::ErrorKind::NotFound => ObjectError::Missing { digest: *want },
                _ => ObjectError::Io(e),
            })?;
        if bytes.len() > MAX_OBJECT_BYTES {
            return Err(ObjectError::Quota {
                digest: *want,
                bytes: bytes.len(),
                limit: MAX_OBJECT_BYTES,
            });
        }
        let actual = Digest::new(&bytes);
        if actual != *want {
            return Err(ObjectError::Corruption {
                digest: *want,
                expected: *want,
                actual,
            });
        }
        Ok(bytes)
    }

    pub fn exists(&self, digest: &Digest) -> bool {
        self.path_for(digest).exists()
    }

    /// All object digests on disk, sorted. Entries that don't parse as a
    /// digest (e.g. `.tmp-*` orphans from crashes) are ignored by design;
    /// directories (the `.gc/` quarantine sibling) are never objects.
    pub fn scan(&self) -> io::Result<Vec<Digest>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            if !entry.file_type()?.is_file() {
                continue;
            }
            if let Some(name) = entry.file_name().to_str()
                && let Ok(digest) = Digest::from_hex(name)
            {
                out.push(digest);
            }
        }
        out.sort();
        Ok(out)
    }

    /// Counts objects on disk not in `referenced`. Counts only — deletion is
    /// post-MVP GC, never performed here.
    pub fn prune_scan(&self, referenced: &HashSet<Digest>) -> io::Result<(u64, u64)> {
        let on_disk = self.scan()?;
        let total = on_disk.len() as u64;
        let orphans = on_disk.iter().filter(|d| !referenced.contains(d)).count() as u64;
        Ok((orphans, total))
    }

    // ---------- M8 wave 2 GC: quarantine + sweep ----------

    /// The quarantine directory, a sibling of the store dir: `<dir>/.gc/`.
    /// Quarantine lives OUTSIDE the store's flat namespace so `scan()`
    /// (and the M7 usage check over it) never sees quarantined objects.
    fn gc_dir(&self) -> PathBuf {
        self.dir.join(".gc")
    }

    /// Moves `digests` out of the store into quarantine (`.gc/<name>`, same
    /// filesystem, atomic rename per object). Missing objects are ignored —
    /// idempotent across crash/reopen. Returns the digests actually moved.
    pub fn quarantine(&mut self, digests: &[Digest]) -> io::Result<Vec<Digest>> {
        if digests.is_empty() {
            return Ok(Vec::new());
        }
        std::fs::create_dir_all(self.gc_dir())?;
        let now = quarantine_clock();
        let mut moved = Vec::new();
        for digest in digests {
            let src = self.path_for(digest);
            let dst = self.gc_dir().join(digest.to_string());
            match std::fs::rename(&src, &dst) {
                Ok(()) => {
                    // The quarantine mtime IS the grace clock (F-F4): stamp
                    // quarantine-begin at the rename, because the rename
                    // preserves the object's original mtime — a
                    // re-referenced-then-re-quarantined object would
                    // otherwise keep its first-quarantine clock and be
                    // deleted on the next sweep immediately.
                    stamp_mtime(&dst, now);
                    moved.push(*digest);
                }
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(moved)
    }

    /// All quarantined digests, sorted. An absent quarantine directory is
    /// the empty set (idempotent).
    pub fn quarantined(&self) -> io::Result<Vec<Digest>> {
        let mut out = Vec::new();
        match std::fs::read_dir(self.gc_dir()) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    if let Some(name) = entry.file_name().to_str()
                        && let Ok(digest) = Digest::from_hex(name)
                    {
                        out.push(digest);
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        }
        out.sort();
        Ok(out)
    }

    /// Quarantined digests with their file modification times — the
    /// quarantine timestamp IS the "last reference" grace clock (the store
    /// tracks no per-object metadata).
    pub fn gc_quarantine_meta(&self) -> io::Result<Vec<(Digest, SystemTime)>> {
        let mut out = Vec::new();
        match std::fs::read_dir(self.gc_dir()) {
            Ok(entries) => {
                for entry in entries {
                    let entry = entry?;
                    if let Some(name) = entry.file_name().to_str()
                        && let Ok(digest) = Digest::from_hex(name)
                        && let Ok(meta) = entry.metadata()
                    {
                        out.push((digest, meta.modified()?));
                    }
                }
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(e) => return Err(e),
        }
        out.sort_by_key(|(d, _)| *d);
        Ok(out)
    }

    /// Removes `digest` — the quarantine copy when present, else the
    /// store-dir copy. Missing files are ignored (idempotent); returns
    /// whether anything was removed.
    pub fn delete(&mut self, digest: &Digest) -> io::Result<bool> {
        for path in [self.gc_dir().join(digest.to_string()), self.path_for(digest)] {
            match std::fs::remove_file(&path) {
                Ok(()) => return Ok(true),
                Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                Err(e) => return Err(e),
            }
        }
        Ok(false)
    }

    /// Moves the quarantine copy of `digest` back into the store dir.
    /// No-op when the store-dir copy already exists (the install-dedup
    /// guarantee: content addressing makes the copies equivalent) or when
    /// no quarantine copy exists. Returns whether a file was moved.
    pub fn restore(&mut self, digest: &Digest) -> io::Result<bool> {
        if self.path_for(digest).exists() {
            return Ok(false);
        }
        match std::fs::rename(self.gc_dir().join(digest.to_string()), self.path_for(digest)) {
            Ok(()) => Ok(true),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
            Err(e) => Err(e),
        }
    }
}

/// Errors from verified object reads.
#[derive(Debug, Error)]
pub enum ObjectError {
    #[error("object not found: {digest}")]
    Missing { digest: Digest },
    #[error("object corrupt: {digest}: hash mismatch (expected {expected}, found {actual})")]
    Corruption {
        digest: Digest,
        expected: Digest,
        actual: Digest,
    },
    #[error("object exceeds the per-object quota: {digest}: {bytes} bytes > {limit}")]
    Quota { digest: Digest, bytes: usize, limit: usize },
    #[error(transparent)]
    Io(#[from] io::Error),
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn tmp_dir(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("kb-objects-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    fn store(tag: &str) -> (PathBuf, ObjectStore, Arc<DurabilityQueue>) {
        let dir = tmp_dir(tag);
        let queue = Arc::new(DurabilityQueue::start(&format!("test-objects-{tag}")));
        let store = ObjectStore::open(&dir, Arc::clone(&queue)).unwrap();
        (dir, store, queue)
    }

    #[test]
    fn install_get_roundtrip() {
        let (dir, mut store, queue) = store("roundtrip");
        let bytes = b"hello object world";
        let digest = store.install(bytes).unwrap();
        store.flush().unwrap();
        assert_eq!(store.get(&digest).unwrap(), bytes);
        drop(store);
        Arc::try_unwrap(queue)
            .unwrap_or_else(|_| panic!("queue Arc still shared"))
            .shutdown()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn dedup_installs_once() {
        let (dir, mut store, queue) = store("dedup");
        let bytes = b"same bytes twice";
        let a = store.install(bytes).unwrap();
        let b = store.install(bytes).unwrap();
        assert_eq!(a, b);
        assert_eq!(store.installs, 1);
        assert_eq!(store.dirsyncs, 1);
        drop(store);
        Arc::try_unwrap(queue)
            .unwrap_or_else(|_| panic!("queue Arc still shared"))
            .shutdown()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn exists_true_false() {
        let (dir, mut store, queue) = store("exists");
        let digest = store.install(b"present").unwrap();
        assert!(store.exists(&digest));
        assert!(!store.exists(&Digest::new(b"absent")));
        drop(store);
        Arc::try_unwrap(queue)
            .unwrap_or_else(|_| panic!("queue Arc still shared"))
            .shutdown()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_missing_names_digest() {
        let (dir, store, queue) = store("missing");
        let want = Digest::new(b"never installed");
        match store.get(&want) {
            Err(ObjectError::Missing { digest }) => {
                assert_eq!(digest, want);
                assert!(digest.to_string().starts_with("blake3:"));
            }
            other => panic!("expected Missing, got {other:?}"),
        }
        drop(store);
        Arc::try_unwrap(queue)
            .unwrap_or_else(|_| panic!("queue Arc still shared"))
            .shutdown()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn get_corruption_reports_expected_vs_actual() {
        let (dir, mut store, queue) = store("corruption");
        let bytes = b"to be corrupted";
        let digest = store.install(bytes).unwrap();
        std::fs::write(store.path_for(&digest), b"garbage").unwrap();
        match store.get(&digest) {
            Err(ObjectError::Corruption {
                digest: d,
                expected,
                actual,
            }) => {
                assert_eq!(d, digest);
                assert_eq!(expected, digest);
                assert_ne!(actual, digest);
            }
            other => panic!("expected Corruption, got {other:?}"),
        }
        assert!(store.scan().unwrap().contains(&digest));
        drop(store);
        Arc::try_unwrap(queue)
            .unwrap_or_else(|_| panic!("queue Arc still shared"))
            .shutdown()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn scan_ignores_tmp_orphans() {
        let (dir, mut store, queue) = store("tmp-orphans");
        std::fs::write(dir.join(".tmp-xyz"), b"stray").unwrap();
        std::fs::write(dir.join(".tmp-12345-0"), b"stray").unwrap();
        assert!(store.scan().unwrap().is_empty());
        let digest = store.install(b"real object").unwrap();
        assert_eq!(store.scan().unwrap(), vec![digest]);
        drop(store);
        Arc::try_unwrap(queue)
            .unwrap_or_else(|_| panic!("queue Arc still shared"))
            .shutdown()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn prune_scan_counts_without_deleting() {
        let (dir, mut store, queue) = store("prune");
        let a = store.install(b"object a").unwrap();
        let b = store.install(b"object b").unwrap();
        let c = store.install(b"object c").unwrap();
        let referenced = HashSet::from([a]);
        assert_eq!(store.prune_scan(&referenced).unwrap(), (2, 3));
        for d in [a, b, c] {
            assert!(store.exists(&d));
        }
        assert_eq!(store.scan().unwrap().len(), 3);
        drop(store);
        Arc::try_unwrap(queue)
            .unwrap_or_else(|_| panic!("queue Arc still shared"))
            .shutdown()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn file_name_is_digest_display() {
        let (dir, mut store, queue) = store("file-name");
        let digest = store.install(b"flat naming").unwrap();
        let path = store.path_for(&digest);
        assert_eq!(path, dir.join(digest.to_string()));
        let names: Vec<String> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        assert_eq!(names, vec![digest.to_string()]);
        assert!(digest.to_string().contains(':'));
        drop(store);
        Arc::try_unwrap(queue)
            .unwrap_or_else(|_| panic!("queue Arc still shared"))
            .shutdown()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn flush_ok_after_installs() {
        let (dir, mut store, queue) = store("flush");
        store.install(b"durable one").unwrap();
        store.install(b"durable two").unwrap();
        store.flush().unwrap();
        assert_eq!(store.dirsyncs, 2);
        drop(store);
        Arc::try_unwrap(queue)
            .unwrap_or_else(|_| panic!("queue Arc still shared"))
            .shutdown()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn queue_ordering_both_dirsyncs_done() {
        let (dir, mut store, queue) = store("ordering");
        let a = store.install(b"object A").unwrap();
        let b = store.install(b"object B").unwrap();
        store.flush().unwrap();
        assert_eq!(store.dirsyncs, 2);
        assert_eq!(store.get(&a).unwrap(), b"object A");
        assert_eq!(store.get(&b).unwrap(), b"object B");
        drop(store);
        Arc::try_unwrap(queue)
            .unwrap_or_else(|_| panic!("queue Arc still shared"))
            .shutdown()
            .unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// R-22: oversized payloads are rejected before any write — no orphan
    /// temp files, no partial name.
    #[test]
    fn oversized_install_rejected_before_write() {
        let (dir, mut store, queue) = store("quota");
        let big = vec![0u8; MAX_OBJECT_BYTES + 1];
        let err = store.install(&big).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::InvalidInput);
        // nothing landed (no partial temp, no object)
        let entries = std::fs::read_dir(&dir).unwrap().count();
        assert_eq!(entries, 0, "no temp or object may leak from a rejected install");
        std::fs::remove_dir_all(&dir).unwrap();
        drop(store);
        if let Ok(queue) = Arc::try_unwrap(queue) {
            let _ = queue.shutdown();
        }
    }
}
