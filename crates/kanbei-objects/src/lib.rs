//! kanbei-objects — the per-session content-addressed object store
//! (ratification-packet §3, §8.3): flat `blake3:<64 hex>` filenames, install
//! via temp write + rename with a queued dirsync (no per-object temp fsync —
//! hash verification detects damage), hash-verified reads, prune scan.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kanbei_core::digest::Digest;
use kanbei_core::queue::{DurabilityQueue, SyncOp};
use thiserror::Error;

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
    /// write + atomic rename, then enqueue a dirsync on the shared queue.
    /// No per-object temp fsync (relaxed per packet §8.3); the caller's
    /// contract is that this dirsync is enqueued before the referencing
    /// event frame's fsync, both FIFO on the same queue.
    pub fn install(&mut self, bytes: &[u8]) -> io::Result<Digest> {
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
        drop(f);
        std::fs::rename(&tmp, &dst)?;
        self.installs += 1;
        self.queue.enqueue(SyncOp::Dirsync(self.dir.clone()))?;
        self.dirsyncs += 1;
        Ok(digest)
    }

    /// Waits until every enqueued durability op (incl. our dirsyncs) ran.
    pub fn flush(&self) -> io::Result<()> {
        self.queue.flush()
    }

    /// Reads an object and verifies its hash. Never returns unverified bytes.
    pub fn get(&self, want: &Digest) -> Result<Vec<u8>, ObjectError> {
        let mut bytes = Vec::new();
        File::open(self.path_for(want))
            .and_then(|mut f| f.read_to_end(&mut bytes))
            .map_err(|e| match e.kind() {
                io::ErrorKind::NotFound => ObjectError::Missing { digest: *want },
                _ => ObjectError::Io(e),
            })?;
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
    /// digest (e.g. `.tmp-*` orphans from crashes) are ignored by design.
    pub fn scan(&self) -> io::Result<Vec<Digest>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
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
}
