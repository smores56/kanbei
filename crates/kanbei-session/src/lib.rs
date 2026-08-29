//! kanbei-session — the M1 session actor: the serialized single-writer commit
//! path that orchestrates object installs + event frames through the shared
//! durability queue, with crash-injection fault points.
//!
//! Design inputs: docs/spikes/ratification-packet.md §3 (the actor ACKs after
//! write+enqueue; flush before consequential effects; object dirsync is
//! enqueued before the referencing frame's fsync) and §7 (inline ≤ 1 KB,
//! object ≥ 8 KB, middle band at kernel discretion — M1 inlines it);
//! docs/architecture.md R-08 (every canonical event references its pre-event
//! commit-snapshot digest; manifests materialize at event commit; genesis
//! uses the kernel bootstrap snapshot) and R-10 (object installation precedes
//! event commit — crashes may orphan objects, never commit a dangling ref).
//!
//! M1 scope decision: `Session` is a SYNCHRONOUS single-writer struct, not a
//! spawned thread — the threaded actor with responder lanes ships at M2 with
//! outcomes. The only background thread here is the shared durability queue's
//! fsync worker.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kanbei_core::digest::Digest;
use kanbei_core::envelope::{Envelope, EnvelopeError, ENVELOPE_SCHEMA};
use kanbei_core::id::Id128;
use kanbei_core::queue::DurabilityQueue;
use kanbei_log::{AppendLog, Profile, Recovered};
use kanbei_objects::{ObjectError, ObjectStore};
use serde_json::json;
use thiserror::Error;

// ---------- config ----------

/// Session configuration. `dir` is the session layout root: `<dir>/log.zst`
/// (append log) and `<dir>/objects/` (object store).
pub struct SessionConfig {
    pub dir: PathBuf,
    pub stream: String,
    pub profile: Profile,
    /// Serialized payloads larger than this are promoted to objects (§7).
    pub inline_max: usize,
    /// Payloads at/above this size may be promoted at kernel discretion by
    /// media type (§7); M1 inlines the 1–8 KB middle band, so the field is
    /// currently unused.
    pub object_min: usize,
    pub fault: Option<Arc<dyn FaultInjector>>,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            dir: PathBuf::from("."),
            stream: "default".into(),
            profile: Profile::Fast,
            inline_max: 1024,
            object_min: 8192,
            fault: None,
        }
    }
}

// ---------- fault injection ----------

/// Crash-injection points on the commit path. The testkit's injector aborts
/// the process at a configured point; `None` (the default) is a no-op.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FaultPoint {
    BeforeObjectInstall,
    AfterObjectInstall,
    BeforeFrameAppend,
    AfterFrameAppend,
}

pub trait FaultInjector: Send + Sync {
    fn inject(&self, point: FaultPoint);
}

// ---------- commit types ----------

/// One caller-authored event, not yet sequenced or validated.
pub struct NewEvent {
    pub kind: String,
    pub payload_schema: u32,
    pub payload: serde_json::Value,
    /// Installed as objects before the frame is appended; their digests are
    /// appended to `refs` (R-10).
    pub objects: Vec<Vec<u8>>,
    /// Must already exist in the store — a commit never creates a dangling
    /// reference.
    pub refs: Vec<Digest>,
}

/// What a committed batch consumed: sequence span, frame size, installed
/// object digests, and the manifest digests bracketing the commit.
#[derive(Debug)]
pub struct CommitReceipt {
    pub first_seq: u64,
    pub last_seq: u64,
    pub count: u64,
    pub frame_len: u64,
    /// Digests installed by this commit's step-2 object phase (event objects
    /// + promoted payloads; the post-state manifest, if any, is excluded).
    pub objects: Vec<Digest>,
    /// The manifest digest every envelope in this commit references (R-08);
    /// None when the session resumed without manifest state (M1).
    pub pre_snapshot: Option<Digest>,
    /// The manifest pinned because this commit changed state; None for pure
    /// commits (unchanged manifests dedup via content addressing).
    pub post_snapshot: Option<Digest>,
}

// ---------- session ----------

pub struct Session {
    log: AppendLog,
    store: ObjectStore,
    queue: Arc<DurabilityQueue>,
    next_seq: u64,
    current_snapshot: Option<Digest>,
    log_path: PathBuf,
    cfg: SessionConfig,
}

impl Session {
    /// Opens `<dir>/log.zst` + `<dir>/objects/`. Runs [`kanbei_log::recover`]
    /// first — REQUIRED before open so a torn tail is truncated before the
    /// writer resumes. A fresh log pins the kernel bootstrap snapshot as the
    /// genesis manifest (R-08); a resumed log does NOT re-pin — M1 sessions
    /// resume without manifest state (current_snapshot is None; the audit
    /// reconstruction is the authority, not the resumed session).
    pub fn open(cfg: SessionConfig) -> Result<Self, SessionError> {
        std::fs::create_dir_all(&cfg.dir)?;
        let log_path = cfg.dir.join("log.zst");
        let recovered = recover_or_fresh(&log_path)?;
        let queue = Arc::new(DurabilityQueue::start(&format!("kb-session-{}", cfg.stream)));
        let log = match AppendLog::open(&log_path, &cfg.stream, Arc::clone(&queue)) {
            Ok(log) => log,
            Err(e) => {
                shutdown_queue(queue);
                return Err(e.into());
            }
        };
        let mut store = match ObjectStore::open(&cfg.dir.join("objects"), Arc::clone(&queue)) {
            Ok(store) => store,
            Err(e) => {
                drop(log);
                shutdown_queue(queue);
                return Err(e.into());
            }
        };
        let next_seq = if recovered.events == 0 { 1 } else { recovered.last_seq + 1 };
        // genesis: pin the kernel bootstrap snapshot as the pre-event
        // snapshot for the first commit (R-08)
        let current_snapshot = if recovered.events == 0 {
            let manifest = kanbei_snapshot::ExecutionManifest::bootstrap();
            match kanbei_snapshot::pin(&mut store, &manifest) {
                Ok((genesis, _deduped)) => Some(genesis),
                Err(e) => {
                    drop(log);
                    drop(store);
                    shutdown_queue(queue);
                    return Err(e.into());
                }
            }
        } else {
            None
        };
        Ok(Self { log, store, queue, next_seq, current_snapshot, log_path, cfg })
    }

    /// Serialized single-writer commit path: install objects (R-10), verify
    /// explicit refs, classify payloads (§7), build envelopes against the
    /// pre-event snapshot (R-08), append one frame, then pin a post-event
    /// manifest iff `state_head` is given. Ack = write + enqueue on the
    /// durability queue (§3); call [`Session::flush`] before consequential
    /// effects.
    pub fn commit(
        &mut self,
        mut events: Vec<NewEvent>,
        state_head: Option<Digest>,
    ) -> Result<CommitReceipt, SessionError> {
        if events.is_empty() {
            return Err(SessionError::InvalidInput("empty commit".into()));
        }

        // step 2 — objects first: the object dirsync is enqueued before the
        // referencing frame's fsync, so the object is durable before the
        // frame (ratification-packet §3, R-10)
        self.fault(FaultPoint::BeforeObjectInstall);
        let mut objects: Vec<Digest> = Vec::new();
        let mut payload_schemas: Vec<u32> = Vec::new();
        for ev in &mut events {
            for bytes in &ev.objects {
                let digest = self.store.install(bytes)?;
                self.fault(FaultPoint::AfterObjectInstall);
                ev.refs.push(digest);
                objects.push(digest);
            }
            // explicit refs must already exist — never commit a newly created
            // dangling reference (R-10)
            for r in &ev.refs {
                if !self.store.exists(r) {
                    return Err(SessionError::MissingObject { digest: *r });
                }
            }
            // payload classification (§7): > inline_max → object reference;
            // the 1–8 KB middle band stays inline (M1 default), so object_min
            // is not consulted
            let serialized = serde_json::to_string(&ev.payload)
                .map_err(|e| SessionError::InvalidInput(format!("payload serialization: {e}")))?;
            if serialized.len() > self.cfg.inline_max {
                let digest = self.store.install(serialized.as_bytes())?;
                self.fault(FaultPoint::AfterObjectInstall);
                ev.payload = json!({ "$object": digest.to_string() });
                ev.refs.push(digest);
                objects.push(digest);
            }
            payload_schemas.push(ev.payload_schema);
        }

        // step 3 — envelopes: every canonical event references its pre-event
        // commit-snapshot digest (R-08)
        let first_seq = self.next_seq;
        let pre_snapshot = self.current_snapshot;
        let envelopes: Vec<Envelope> = events
            .iter()
            .enumerate()
            .map(|(i, ev)| Envelope {
                env: ENVELOPE_SCHEMA,
                seq: first_seq + i as u64,
                evt: Id128::generate().to_string(),
                kind: ev.kind.clone(),
                payload_schema: ev.payload_schema,
                payload: ev.payload.clone(),
                refs: ev.refs.clone(),
                snapshot: pre_snapshot,
            })
            .collect();

        // step 4 — one frame through the durability queue
        self.fault(FaultPoint::BeforeFrameAppend);
        let plan = self.log.append(&envelopes, self.cfg.profile)?;
        self.fault(FaultPoint::AfterFrameAppend);
        self.next_seq = plan.last_seq + 1;

        // step 5 — state-changing commits pin a post-event manifest; pure
        // commits leave the manifest unchanged (content addressing dedups
        // identical manifests)
        let post_snapshot = match state_head {
            Some(head) => {
                let mut manifest = kanbei_snapshot::ExecutionManifest::bootstrap();
                manifest.state_head = Some(head);
                let mut schema_versions = payload_schemas;
                schema_versions.push(1);
                schema_versions.sort_unstable();
                schema_versions.dedup();
                manifest.schema_versions = schema_versions;
                let (digest, _deduped) = kanbei_snapshot::pin(&mut self.store, &manifest)?;
                self.current_snapshot = Some(digest);
                Some(digest)
            }
            None => None,
        };

        Ok(CommitReceipt {
            first_seq: plan.first_seq,
            last_seq: plan.last_seq,
            count: plan.count,
            frame_len: plan.frame_len,
            objects,
            pre_snapshot,
            post_snapshot,
        })
    }

    /// fsync-before-consequential-effect contract (§3): waits until every
    /// enqueued durability op ran — the log frames and all pending object
    /// dirsyncs.
    pub fn flush(&self) -> Result<(), SessionError> {
        Ok(self.log.flush()?)
    }

    pub fn store(&self) -> &ObjectStore {
        &self.store
    }

    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    pub fn current_snapshot(&self) -> Option<Digest> {
        self.current_snapshot
    }

    /// Flush, then stop the durability worker and join it.
    pub fn close(self) -> Result<(), SessionError> {
        let Session { log, store, queue, .. } = self;
        log.flush()?;
        drop(log);
        drop(store);
        let queue = Arc::try_unwrap(queue)
            .map_err(|_| SessionError::InvalidInput("durability queue still shared".into()))?;
        queue.shutdown()?;
        Ok(())
    }

    fn fault(&self, point: FaultPoint) {
        if let Some(f) = &self.cfg.fault {
            f.inject(point);
        }
    }
}

// ---------- errors ----------

#[derive(Debug, Error)]
pub enum SessionError {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Log(#[from] kanbei_log::RecoveryError),
    #[error(transparent)]
    Object(#[from] ObjectError),
    #[error("envelope: {0}")]
    Envelope(EnvelopeError),
    #[error("event references missing object: {digest}")]
    MissingObject { digest: Digest },
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("snapshot: {0}")]
    Snapshot(String),
}

// ---------- helpers ----------

/// `recover` errors on a missing file; a fresh dir is a valid genesis state.
fn recover_or_fresh(log_path: &Path) -> Result<Recovered, SessionError> {
    match std::fs::metadata(log_path) {
        Ok(m) if m.is_file() => Ok(kanbei_log::recover(log_path)?),
        Ok(_) => Err(SessionError::InvalidInput(format!(
            "log path is not a file: {}",
            log_path.display()
        ))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            Ok(Recovered { events: 0, frames: 0, truncated: false, last_seq: 0 })
        }
        Err(e) => Err(e.into()),
    }
}

/// Best-effort worker cleanup on a failed open, when no other Arc clones
/// exist. Only reachable while returning an error, so a secondary shutdown
/// failure is not propagated.
fn shutdown_queue(queue: Arc<DurabilityQueue>) {
    if let Ok(queue) = Arc::try_unwrap(queue) {
        let _ = queue.shutdown();
    }
}
