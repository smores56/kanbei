//! The commit path: objects-first install (R-10), one appended frame, and the
//! post-state manifest pin (R-08).

use std::collections::HashSet;
use std::sync::Mutex;

use kanbei_core::digest::Digest;
use kanbei_core::envelope::{ENVELOPE_SCHEMA, Envelope};
use kanbei_core::id::Id128;
use kanbei_log::{AppendLog, Profile};
use kanbei_objects::ObjectStore;
use kanbei_snapshot::ExecutionManifest;
use serde_json::json;
use thiserror::Error;

use crate::event::NewEvent;
use crate::fault::{FaultInjector, FaultPoint};
use crate::pins::GcPinGuard;

#[derive(Debug, Error)]
pub enum CommitError {
    #[error("invalid input: {0}")]
    InvalidInput(String),
    #[error("event references missing object: {digest}")]
    MissingObject { digest: Digest },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Object(#[from] kanbei_objects::ObjectError),
}

/// Scalar commit parameters (identity/sequence state owned by the caller).
#[derive(Debug, Clone, Copy)]
pub struct CommitParams {
    pub profile: Profile,
    pub next_seq: u64,
    pub current_snapshot: Option<Digest>,
    pub inline_max: usize,
}

/// The post-state manifest a state-changing commit pins: the manifest, its
/// composition object bytes, and the config objects it references (installed
/// before the pin so the snapshot closure verifies from the store alone,
/// R-10).
pub struct PostManifest {
    pub manifest: ExecutionManifest,
    pub composition_bytes: Vec<u8>,
    pub config_objects: Vec<Vec<u8>>,
}

/// The tier-1 commit path over the append log, object store, and writer-pin
/// set. A tier-2 composition root owns the state and drives it.
/// Read-only secondary source for a ref check: the caller's storage may place
/// a legitimate explicit ref outside the writer store (the XDG layout keeps
/// the config/module packages in a global store), while the writer store still
/// owns the log's own objects. Consulted only after the writer store misses, so
/// a corrupt primary object stays corrupt rather than being masked.
pub trait RefSource {
    fn has(&self, digest: &Digest) -> bool;
}

pub struct CommitPath<'a> {
    log: &'a mut AppendLog,
    store: &'a mut ObjectStore,
    gc_pins: &'a Mutex<HashSet<Digest>>,
    refs: Option<&'a dyn RefSource>,
}

impl<'a> CommitPath<'a> {
    pub fn new(
        log: &'a mut AppendLog,
        store: &'a mut ObjectStore,
        gc_pins: &'a Mutex<HashSet<Digest>>,
    ) -> Self {
        Self {
            log,
            store,
            gc_pins,
            refs: None,
        }
    }

    /// Attaches a secondary ref source consulted when the writer store lacks an
    /// explicit ref (see [`RefSource`]).
    pub fn with_ref_source(mut self, refs: &'a dyn RefSource) -> Self {
        self.refs = Some(refs);
        self
    }

    /// Steps 2–4: objects-first install (R-10), reference verification, payload
    /// classification, envelope construction, and one appended frame. The
    /// caller pins the post-state manifest separately (step 5).
    pub fn commit(
        &mut self,
        events: &mut [NewEvent],
        params: CommitParams,
        fault: Option<&dyn FaultInjector>,
    ) -> Result<CommitOutcome, CommitError> {
        let inject = |point: FaultPoint| {
            if let Some(fault) = fault {
                fault.inject(point);
            }
        };

        // step 2 — objects first: the object dirsync is enqueued before the
        // referencing frame's fsync, so the object is durable before the frame
        // (R-10). Every digest installed here is writer-pinned before install
        // and unpinned on guard drop (after the append).
        inject(FaultPoint::BeforeObjectInstall);
        let mut objects: Vec<Digest> = Vec::new();
        let mut pins = GcPinGuard::new(self.gc_pins);
        for ev in events.iter_mut() {
            for bytes in &ev.objects {
                pins.pin(Digest::new(bytes));
                let digest = self.store.install(bytes)?;
                inject(FaultPoint::AfterObjectInstall);
                ev.refs.push(digest);
                objects.push(digest);
            }
            // explicit refs must already exist — never commit a newly created
            // dangling reference (R-10); an out-of-store ref is accepted only
            // when the caller's secondary source resolves it.
            for r in &ev.refs {
                if !self.store.exists(r) && !self.refs.is_some_and(|s| s.has(r)) {
                    return Err(CommitError::MissingObject { digest: *r });
                }
            }
            // payload classification (§7): > inline_max → object reference
            let serialized = serde_json::to_string(&ev.payload)
                .map_err(|e| CommitError::InvalidInput(format!("payload serialization: {e}")))?;
            if serialized.len() > params.inline_max {
                pins.pin(Digest::new(serialized.as_bytes()));
                let digest = self.store.install(serialized.as_bytes())?;
                inject(FaultPoint::AfterObjectInstall);
                ev.payload = json!({ "$object": digest.to_string() });
                ev.refs.push(digest);
                objects.push(digest);
            }
        }

        // step 3 — envelopes: every canonical event references its pre-event
        // commit-snapshot digest (R-08)
        let first_seq = params.next_seq;
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
                snapshot: params.current_snapshot,
            })
            .collect();

        // step 4 — one frame through the durability queue
        inject(FaultPoint::BeforeFrameAppend);
        let plan = self.log.append(&envelopes, params.profile)?;
        inject(FaultPoint::AfterFrameAppend);
        drop(pins);

        Ok(CommitOutcome {
            first_seq,
            last_seq: plan.last_seq,
            count: plan.count,
            frame_len: plan.frame_len,
            objects,
            pre_snapshot: params.current_snapshot,
            envelopes,
        })
    }
}

/// Step 5 — install the composition/config objects and pin the post-state
/// manifest (R-10/R-08). The caller owns the post-event sequence: it runs
/// after the frame append and the head advance.
pub fn pin_post_manifest(
    store: &mut ObjectStore,
    gc_pins: &Mutex<HashSet<Digest>>,
    post: PostManifest,
) -> Result<Digest, CommitError> {
    let mut pins = GcPinGuard::new(gc_pins);
    pins.pin(Digest::new(&post.composition_bytes));
    store.install(&post.composition_bytes)?;
    for bytes in &post.config_objects {
        pins.pin(Digest::new(bytes));
        store.install(bytes)?;
    }
    pins.pin(Digest::new(&post.manifest.to_bytes()));
    let digest = crate::pinning::pin(store, &post.manifest)?;
    Ok(digest)
}

/// The tier-1 outcome of the append phase; the caller applies its own tier-2
/// bookkeeping (recent rings, listeners, telemetry).
pub struct CommitOutcome {
    pub first_seq: u64,
    pub last_seq: u64,
    pub count: u64,
    pub frame_len: u64,
    pub objects: Vec<Digest>,
    pub pre_snapshot: Option<Digest>,
    pub envelopes: Vec<Envelope>,
}
