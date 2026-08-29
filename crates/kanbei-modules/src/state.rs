//! Host-owned module state (architecture.md "Module state"; head contract
//! R-07/B-01/F2): immutable content-addressed snapshot objects plus an
//! atomically replaced per-key head pointer. Updates operate on a copy,
//! validate, and commit atomically; the head CAS is single-writer (the
//! session actor) and checks the generation token on every update.
//!
//! Layout under the session dir:
//! - `<dir>/state/<StateKey>.head` — the head file;
//! - `<dir>/state/objects/` — the per-session state-snapshot object store.
//!
//! Head replacement goes through the durability protocol: temp write +
//! rename + `Dirsync` on the shared [`DurabilityQueue`] — never a blocking
//! fsync (the queue thread fsyncs FIFO).
//!
//! M2 keeps state bytes as the compact JSON encoding of the value the module
//! wrote; `state_get` returns those bytes parsed back into JSON.

use std::collections::HashSet;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kanbei_core::digest::Digest;
use kanbei_core::queue::{DurabilityQueue, SyncOp};
use kanbei_objects::{ObjectError, ObjectStore};
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Default encoded-state ceiling when no limit is configured.
pub const DEFAULT_MAX_STATE_BYTES: usize = 1024 * 1024;

/// The state head: digest + schema version + checksum + last-pinned snapshot
/// digest + sequence. Canonical JSON with field order as listed.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
pub struct HeadFile {
    /// Snapshot object digest (immutable content-addressed state).
    pub digest: Digest,
    /// State schema version of the snapshot.
    pub schema: u32,
    /// Digest over the canonical JSON of the head WITHOUT the checksum field
    /// (self-reference avoided, mirroring the frame digest pattern).
    pub checksum: Digest,
    /// Snapshot digest pinned by the last canonical event, if any.
    pub last_pinned: Option<Digest>,
    /// Monotonic head-replacement sequence (starts at 1).
    pub seq: u64,
}

/// The checksum input: canonical JSON of every field except `checksum`.
/// Object keys are serialized sorted (serde_json's default map), so the
/// bytes are stable.
fn checksum_of(digest: &Digest, schema: u32, last_pinned: &Option<Digest>, seq: u64) -> Digest {
    let canonical = serde_json::json!({
        "digest": digest,
        "schema": schema,
        "last_pinned": last_pinned,
        "seq": seq,
    });
    Digest::new(&serde_json::to_vec(&canonical).expect("head checksum canonical JSON cannot fail"))
}

impl HeadFile {
    pub fn to_bytes(&self) -> Vec<u8> {
        serde_json::to_vec(self).expect("head serialization cannot fail")
    }

    pub fn from_bytes(bytes: &[u8]) -> Result<Self, StateError> {
        serde_json::from_slice(bytes).map_err(|e| {
            StateError::InvalidInput(format!("head file is not canonical JSON: {e}"))
        })
    }

    /// True when the stored checksum matches a fresh derivation.
    pub fn verify(&self) -> bool {
        self.checksum
            == checksum_of(&self.digest, self.schema, &self.last_pinned, self.seq)
    }

    fn new(digest: Digest, schema: u32, last_pinned: Option<Digest>, seq: u64) -> Self {
        let checksum = checksum_of(&digest, schema, &last_pinned, seq);
        Self {
            digest,
            schema,
            checksum,
            last_pinned,
            seq,
        }
    }
}

/// A candidate new state (a copy): the module's next snapshot for `key`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StateUpdate {
    pub key: String,
    pub schema: u32,
    pub bytes: Vec<u8>,
    /// The generation proposing the update; must still be current.
    pub generation: u64,
}

/// The module-state store. Single-writer: the session actor (or the kernel
/// state-store actor it owns) calls [`Self::cas`]; concurrent writers are not
/// supported (the head replacement is temp+rename, last writer wins).
pub struct StateStore {
    /// Session dir; heads live under `<dir>/state/<key>.head`.
    dir: PathBuf,
    /// State-snapshot objects at `<dir>/state/objects/` (a per-session store).
    objects: ObjectStore,
    queue: Arc<DurabilityQueue>,
    max_state_bytes: usize,
    /// Generation-currency gate: false ⇒ the update's generation is displaced.
    generation_current: Arc<dyn Fn(u64) -> bool + Send + Sync>,
}

impl StateStore {
    /// Opens (creating) the state store under the session dir. `max_state_bytes`
    /// starts at [`DEFAULT_MAX_STATE_BYTES`]; the session lane overrides it
    /// with [`Self::set_max_state_bytes`]. Panics only when the session dir
    /// cannot be created (the API is infallible by contract).
    pub fn open(
        dir: &Path,
        queue: Arc<DurabilityQueue>,
        generation_current: Arc<dyn Fn(u64) -> bool + Send + Sync>,
    ) -> Self {
        let state_root = dir.join("state");
        std::fs::create_dir_all(&state_root)
            .expect("state store: cannot create the state dir under the session dir");
        let objects = ObjectStore::open(&state_root.join("objects"), Arc::clone(&queue))
            .expect("state store: cannot open the state-snapshot object store");
        Self {
            dir: dir.to_path_buf(),
            objects,
            queue,
            max_state_bytes: DEFAULT_MAX_STATE_BYTES,
            generation_current,
        }
    }

    /// Overrides the encoded-state ceiling (architecture.md: "Rust initially
    /// enforces maximum encoded current state"; oversized updates throw
    /// atomically and leave the old head active).
    pub fn set_max_state_bytes(&mut self, max: usize) {
        self.max_state_bytes = max;
    }

    pub fn max_state_bytes(&self) -> usize {
        self.max_state_bytes
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    pub(crate) fn queue(&self) -> Arc<DurabilityQueue> {
        Arc::clone(&self.queue)
    }

    fn state_root(&self) -> PathBuf {
        self.dir.join("state")
    }

    fn head_path(&self, key: &str) -> PathBuf {
        self.state_root().join(format!("{key}.head"))
    }

    /// State keys become file names; reject anything that could escape the
    /// state dir.
    fn validate_key(key: &str) -> Result<(), StateError> {
        if key.is_empty()
            || key.starts_with('.')
            || key.contains('/')
            || key.contains('\\')
            || key.contains('\0')
        {
            return Err(StateError::InvalidInput(format!(
                "state key {key:?} must be a bare name (no path separators, no leading dot)"
            )));
        }
        Ok(())
    }

    /// Reads + verifies the head file for `key`; `Ok(None)` when no head
    /// exists. A corrupt head is an error, never silently overwritten
    /// (fail-closed).
    fn read_head(&self, key: &str) -> Result<Option<HeadFile>, StateError> {
        let path = self.head_path(key);
        let mut bytes = Vec::new();
        match File::open(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(StateError::Io(e)),
            Ok(mut f) => {
                f.read_to_end(&mut bytes)?;
            }
        }
        let head = HeadFile::from_bytes(&bytes).map_err(|_| StateError::CorruptHead {
            key: key.to_string(),
            reason: "head file is not canonical JSON".into(),
        })?;
        if !head.verify() {
            return Err(StateError::CorruptHead {
                key: key.to_string(),
                reason: "head checksum mismatch".into(),
            });
        }
        Ok(Some(head))
    }

    /// The durability protocol for head replacement: temp write + atomic
    /// rename + enqueue a `Dirsync` on the shared queue — never a blocking
    /// fsync (the queue's worker fsyncs FIFO).
    fn write_head(&mut self, key: &str, head: &HeadFile) -> Result<(), StateError> {
        let path = self.head_path(key);
        let tmp = self
            .state_root()
            .join(format!(".tmp-{key}.head-{}", std::process::id()));
        let mut f = File::create(&tmp)?;
        f.write_all(&head.to_bytes())?;
        drop(f);
        std::fs::rename(&tmp, &path)?;
        self.queue.enqueue(SyncOp::Dirsync(self.state_root()))?;
        Ok(())
    }

    /// The atomic head CAS — single-writer, callable only by the session
    /// actor. Order: generation currency check → size check (old head
    /// untouched) → existing-head schema check (fail-closed, old head
    /// untouched) → snapshot install (content-deduped) → head replacement.
    pub fn cas(&mut self, update: StateUpdate) -> Result<HeadFile, StateError> {
        Self::validate_key(&update.key)?;
        if !(self.generation_current)(update.generation) {
            return Err(StateError::StaleGeneration {
                generation: update.generation,
            });
        }
        if update.bytes.len() > self.max_state_bytes {
            return Err(StateError::Oversized {
                key: update.key.clone(),
                bytes: update.bytes.len(),
                limit: self.max_state_bytes,
            });
        }
        let old = self.read_head(&update.key)?;
        if let Some(head) = &old
            && head.schema != update.schema
        {
            return Err(StateError::SchemaMismatch {
                key: update.key.clone(),
                expected: head.schema,
                actual: update.schema,
            });
        }
        let digest = self.objects.install(&update.bytes)?;
        let head = HeadFile::new(
            digest,
            update.schema,
            old.as_ref().and_then(|h| h.last_pinned),
            old.map(|h| h.seq + 1).unwrap_or(1),
        );
        self.write_head(&update.key, &head)?;
        Ok(head)
    }

    /// Reads the head file (checksum-verified) and the snapshot object.
    /// `Ok(None)` when no head exists for `key`.
    pub fn get(&self, key: &str) -> Result<Option<(HeadFile, Vec<u8>)>, StateError> {
        Self::validate_key(key)?;
        let Some(head) = self.read_head(key)? else {
            return Ok(None);
        };
        let bytes = self.objects.get(&head.digest).map_err(|e| match e {
            ObjectError::Missing { digest } => StateError::MissingSnapshot {
                key: key.to_string(),
                digest,
            },
            other => StateError::Object(other),
        })?;
        Ok(Some((head, bytes)))
    }

    /// Sets `last_pinned` on the head (the session calls this when a canonical
    /// event pins the state). Head CAS, same durability protocol; generation
    /// not involved — kernel-internal.
    pub fn mark_pinned(&mut self, key: &str, snapshot: Digest) -> Result<(), StateError> {
        Self::validate_key(key)?;
        let Some(old) = self.read_head(key)? else {
            return Err(StateError::InvalidInput(format!(
                "mark_pinned({key}): no head exists to pin onto"
            )));
        };
        let head = HeadFile::new(old.digest, old.schema, Some(snapshot), old.seq + 1);
        self.write_head(key, &head)
    }

    /// `prune-unpinned-state` (R-08/B-02): deletes state-snapshot objects
    /// referenced by neither any current head nor `referenced` (digests pinned
    /// by committed execution-snapshot manifests). Never touches current-head
    /// snapshots or `.tmp-*` files (`ObjectStore::scan` ignores those).
    /// `grace_objects` keeps at least that many candidates (default 0), the
    /// digest-sorted tail. Returns the number of objects deleted.
    pub fn prune_unpinned(
        &mut self,
        referenced: &HashSet<Digest>,
        grace_objects: usize,
    ) -> Result<u64, StateError> {
        let mut keep: HashSet<Digest> = referenced.clone();
        for (_, head) in self.heads()? {
            keep.insert(head.digest);
        }
        let mut candidates: Vec<Digest> = self
            .objects
            .scan()?
            .into_iter()
            .filter(|d| !keep.contains(d))
            .collect();
        candidates.sort();
        let delete_from = candidates.len().saturating_sub(grace_objects);
        let mut deleted = 0u64;
        for digest in &candidates[..delete_from] {
            std::fs::remove_file(self.objects.path_for(digest))?;
            deleted += 1;
        }
        Ok(deleted)
    }

    /// Every current head as `(key, head)` in key order — for execution
    /// snapshot manifests.
    pub fn heads(&self) -> Result<Vec<(String, HeadFile)>, StateError> {
        let mut out = Vec::new();
        for entry in std::fs::read_dir(self.state_root())? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();
            if let Some(key) = name.strip_suffix(".head")
                && let Some(head) = self.read_head(key)?
            {
                out.push((key.to_string(), head));
            }
        }
        out.sort_by(|a, b| a.0.cmp(&b.0));
        Ok(out)
    }
}

#[derive(Debug, Error)]
pub enum StateError {
    #[error("state update from displaced generation {generation} rejected (stale)")]
    StaleGeneration { generation: u64 },
    #[error("state update for `{key}` is {bytes} bytes, over the {limit}-byte limit (old head active)")]
    Oversized {
        key: String,
        bytes: usize,
        limit: usize,
    },
    #[error("state update for `{key}` has schema {actual}, head requires {expected} (fail-closed, old head untouched)")]
    SchemaMismatch {
        key: String,
        expected: u32,
        actual: u32,
    },
    #[error("head for `{key}` is corrupt: {reason}")]
    CorruptHead { key: String, reason: String },
    #[error("snapshot object {digest} for `{key}` is missing")]
    MissingSnapshot { key: String, digest: Digest },
    #[error(transparent)]
    Object(#[from] ObjectError),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error("invalid input: {0}")]
    InvalidInput(String),
}
