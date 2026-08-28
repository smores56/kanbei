//! S4 spike: execution-snapshot manifests + object store at scale.
//! Disposable spike code — never promoted into the implementation.
//!
//! Measures: manifest materialization + dedup ratio across a simulated pinned
//! history, install-protocol cost (with and without batched dirsync), closure
//! verification, and object-store behavior up to 1M files.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const ALG: &str = "blake3";

pub fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

pub fn digest(bytes: &[u8]) -> String {
    hex(blake3::hash(bytes).as_bytes())
}

// ---------- object store ----------

pub struct ObjectStore {
    dir: PathBuf,
    pub batched_dirsync: bool,
    pub installs: u64,
    pub dirsyncs: u64,
}

impl ObjectStore {
    pub fn open(dir: &Path, batched_dirsync: bool) -> std::io::Result<Self> {
        std::fs::create_dir_all(dir)?;
        Ok(Self { dir: dir.to_path_buf(), batched_dirsync, installs: 0, dirsyncs: 0 })
    }

    fn path_for(&self, digest: &str) -> PathBuf {
        self.dir.join(format!("{ALG}:{digest}"))
    }

    /// Install protocol (R-10/B-03): temp write + fsync, rename, dirsync.
    /// With batched_dirsync, the caller must call `dirsync()` after a group.
    pub fn install(&mut self, bytes: &[u8]) -> std::io::Result<String> {
        let d = digest(bytes);
        let dst = self.path_for(&d);
        if dst.exists() {
            return Ok(d);
        }
        let tmp = self.dir.join(format!(".tmp-{}-{}", std::process::id(), self.installs));
        let mut f = File::create(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
        drop(f);
        std::fs::rename(&tmp, &dst)?;
        self.installs += 1;
        if !self.batched_dirsync {
            self.dirsync()?;
        }
        Ok(d)
    }

    pub fn dirsync(&mut self) -> std::io::Result<()> {
        let d = File::open(&self.dir)?;
        d.sync_all()?;
        drop(d);
        self.dirsyncs += 1;
        Ok(())
    }

    /// Read + hash-verify.
    pub fn get(&self, want: &str) -> std::io::Result<Vec<u8>> {
        let mut bytes = Vec::new();
        File::open(self.path_for(want))?.read_to_end(&mut bytes)?;
        let got = digest(&bytes);
        if got != want {
            return Err(std::io::Error::other(format!("hash mismatch: {got} != {want}")));
        }
        Ok(bytes)
    }

    pub fn exists(&self, digest: &str) -> bool {
        self.path_for(digest).exists()
    }

    pub fn scan(&self) -> std::io::Result<Vec<String>> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(&self.dir)? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if name.starts_with(&format!("{ALG}:")) {
                out.push(name);
            }
        }
        Ok(out)
    }
}

// ---------- execution-snapshot manifest ----------

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Manifest {
    pub schema: u32,
    pub state_head: String,
    pub memory_root: String,
    pub tool_registry: u64,
    pub projection: u64,
    pub provider: u64,
    pub policy: u64,
    pub schema_versions: Vec<u32>,
}

impl Manifest {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap()
    }
}

/// Pin one manifest: content-addressed; identical manifests dedup to the same
/// object. Returns (digest, created_new).
pub fn pin(store: &mut ObjectStore, m: &Manifest) -> std::io::Result<(String, bool)> {
    let bytes = m.to_bytes();
    let d = digest(&bytes);
    let created = !store.exists(&d);
    store.install(&bytes)?;
    Ok((d, created))
}

/// Verify closure: every referenced object exists with a valid hash.
pub fn verify_closure(store: &ObjectStore, refs: &[String]) -> std::io::Result<u64> {
    let mut verified = 0u64;
    for r in refs {
        store.get(r)?;
        verified += 1;
    }
    Ok(verified)
}

/// Prune scan: objects on disk not referenced by any pinned manifest.
pub fn prune_scan(store: &ObjectStore, referenced: &std::collections::HashSet<String>) -> std::io::Result<(u64, u64)> {
    let on_disk = store.scan()?;
    let mut orphans = 0u64;
    for o in on_disk {
        if !referenced.contains(&o) {
            orphans += 1;
        }
    }
    Ok((orphans, store.scan()?.len() as u64))
}
