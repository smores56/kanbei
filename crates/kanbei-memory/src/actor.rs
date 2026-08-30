//! The per-scope writer/CAS actor (R-11): one narrow writer per memory scope,
//! owning the scope's transition log, object store, head pointer, and
//! durability queue. A transition supplies expected old root, accepted new
//! root, origin session/event, decision principal, and TransitionId; the
//! actor validates, CAS-es, installs the root-manifest object, appends the
//! transition frame, and advances the atomic head pointer. Stale competing
//! roots fail with [`TransitionOutcome::CasFailed`] and must reconcile.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kanbei_core::queue::{DurabilityQueue, SyncOp};
use kanbei_core::{Digest, ENVELOPE_SCHEMA, Envelope, Id128};
use kanbei_log::{AppendLog, Profile, Recovered};
use kanbei_objects::ObjectStore;
use serde::{Deserialize, Serialize};

use crate::canonical_bytes;
use crate::error::MemoryError;
use crate::types::{
    Claim, ClaimEdge, EdgeKind, IdempotencyKey, MEMORY_ROOT_SCHEMA, MEMORY_TRANSITION_SCHEMA,
    MemoryScope, MemoryTransition, RootFold, RootManifest, TransitionKind,
};
use crate::{TRANSITIONS_STREAM, zero_digest, zero_id};

/// The atomic head pointer: `{"schema":1,"root":digest|null,"transition_id":id|null}`.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
struct HeadFile {
    schema: u32,
    root: Option<Digest>,
    transition_id: Option<Id128>,
}

/// The outcome of a transition proposal.
#[derive(Clone, Debug, PartialEq)]
pub enum TransitionOutcome {
    /// The transition committed: head advanced to `new_root`.
    Committed {
        transition_id: Id128,
        new_root: Digest,
    },
    /// CAS failed: `expected` did not match the actor's `actual` head. The
    /// claim/edge digests the caller installed before proposing are returned
    /// so the caller records them as expected orphans.
    CasFailed {
        expected: Option<Digest>,
        actual: Option<Digest>,
        installed: Vec<Digest>,
    },
}

/// Crash/fault point inside [`MemoryRootActor::propose`]'s commit path
/// (the memory half of the M4 crash-test contract; the session arms the
/// injector). Points fire only after all validation has passed — never on
/// CAS failures or other rejected proposals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryFaultPoint {
    /// Before the transition frame is appended to the scope log.
    BeforeTransition,
    /// After the log append returns Ok, before the head.json write.
    AfterTransition,
    /// Right before the atomic head.json write.
    BeforeHeadUpdate,
    /// Right after the atomic head.json write, before the flush.
    AfterHeadUpdate,
}

/// Fault injector for the memory commit path: called at the exact
/// [`MemoryFaultPoint`]s of a successful `propose`. Implementations must be
/// cheap and panic-free (a crash-test child aborts here).
pub trait MemoryFaultInjector: Send + Sync {
    fn inject(&self, point: MemoryFaultPoint);
}

/// What a log scan yields: the last committed root, all idempotency keys,
/// and the (origin_session, transition_id) pairs for backlink candidates.
#[derive(Default)]
struct LogScan {
    last: Option<(Digest, Id128)>,
    keys: Vec<IdempotencyKey>,
    origins: Vec<(Id128, Id128)>,
}

/// The single writer/CAS actor for one memory scope (R-11).
pub struct MemoryRootActor {
    scope: MemoryScope,
    /// Scope dir: `transitions.jsonl.zst`, `head.json`, `objects/`.
    dir: PathBuf,
    log: AppendLog,
    store: ObjectStore,
    queue: Arc<DurabilityQueue>,
    /// In-memory current root (repaired from the log at open).
    head: Option<Digest>,
    head_transition: Option<Id128>,
    /// Idempotency keys of all committed transitions (loaded at open).
    seen_keys: Vec<IdempotencyKey>,
    /// (origin_session, transition_id) per committed transition, exposed by
    /// [`MemoryRootActor::scan_backlink_candidates`] so the originating
    /// session can record idempotent backlinks (R-11).
    origins: Vec<(Id128, Id128)>,
    /// Optional fault injector fired at the commit-path points of
    /// `propose` (crash-test contract).
    fault: Option<Arc<dyn MemoryFaultInjector>>,
}

impl MemoryRootActor {
    /// Opens (creating if missing) the scope directory under `memory_root`:
    /// recovers the transition log (torn tail handled), starts the
    /// durability queue, scans the log for the last committed root and all
    /// idempotency keys, repairs `head.json` from the log when it is missing,
    /// unparseable, or divergent, and opens the scope object store.
    pub fn open(memory_root: &Path, scope: MemoryScope) -> Result<Self, MemoryError> {
        let dir = memory_root.join(scope.dir_name());
        std::fs::create_dir_all(dir.join("objects"))?;
        let log_path = dir.join("transitions.jsonl.zst");
        let recovered = recover_or_fresh(&log_path)?;
        let queue = Arc::new(DurabilityQueue::start(&format!(
            "mem-root-{}",
            scope.dir_name()
        )));
        let log = match AppendLog::open(&log_path, TRANSITIONS_STREAM, Arc::clone(&queue)) {
            Ok(log) => log,
            Err(e) => {
                shutdown_queue(queue);
                return Err(e.into());
            }
        };
        let scan = if recovered.events > 0 {
            scan_log(&log_path)?
        } else {
            LogScan::default()
        };
        let (head, head_transition) = match scan.last {
            Some((root, tid)) => (Some(root), Some(tid)),
            None => (None, None),
        };
        // head.json is a convenience pointer: missing, unparseable, or
        // divergent head files are repaired from the log (head repair rule).
        let expected = HeadFile {
            schema: 1,
            root: head,
            transition_id: head_transition,
        };
        let current = read_head_file(&dir).filter(|h| *h == expected);
        if current.is_none() {
            write_head_atomic(&dir, &expected)?;
            queue.enqueue(SyncOp::Dirsync(dir.clone()))?;
        }
        let store = match ObjectStore::open(&dir.join("objects"), Arc::clone(&queue)) {
            Ok(store) => store,
            Err(e) => {
                drop(log);
                shutdown_queue(queue);
                return Err(e.into());
            }
        };
        Ok(Self {
            scope,
            dir,
            log,
            store,
            queue,
            head,
            head_transition,
            seen_keys: scan.keys,
            origins: scan.origins,
            fault: None,
        })
    }

    /// The current root digest, or `None` before the first transition.
    pub fn head(&self) -> Option<Digest> {
        self.head
    }

    /// The transition id that committed the current head.
    pub fn head_transition(&self) -> Option<Id128> {
        self.head_transition
    }

    /// Proposes a root-selection transition. Validation order: scope match,
    /// idempotency, origin verification, CAS, refs-to-committed objects,
    /// acyclicity, then the manifest digest check and commit.
    pub fn propose(
        &mut self,
        transition: MemoryTransition,
        claims: &[Digest],
        edges: &[Digest],
    ) -> Result<TransitionOutcome, MemoryError> {
        // 0 — refresh from the file tail: another live writer (a second
        // session's actor on this scope dir) may have committed since this
        // handle opened. The CAS head, idempotency keys, and the frame seq
        // must continue the FILE's state, not this handle's stale snapshot —
        // a stale handle would otherwise rebase against a head it can never
        // match and append frames with duplicate seqs (cross-session CAS
        // determinism).
        let log_path = self.dir.join("transitions.jsonl.zst");
        let recovered = recover_or_fresh(&log_path)?;
        if recovered.events > 0 {
            let scan = scan_log(&log_path)?;
            if let Some((root, tid)) = scan.last {
                self.head = Some(root);
                self.head_transition = Some(tid);
            }
            self.seen_keys = scan.keys;
            self.origins = scan.origins;
        }
        self.log = AppendLog::open(&log_path, TRANSITIONS_STREAM, Arc::clone(&self.queue))?;

        // 1 — scope
        if transition.scope != self.scope {
            return Err(MemoryError::InvalidInput(format!(
                "transition scope {:?} != actor scope {:?}",
                transition.scope, self.scope
            )));
        }
        if transition.schema != MEMORY_TRANSITION_SCHEMA {
            return Err(MemoryError::InvalidInput(format!(
                "transition schema {}, expected {MEMORY_TRANSITION_SCHEMA}",
                transition.schema
            )));
        }

        // 2 — idempotency (R-11): a second transition with the same key is
        // rejected even when the CAS would succeed
        if self.seen_keys.contains(&transition.idempotency_key) {
            return Err(MemoryError::DuplicateTransition(transition.idempotency_key));
        }

        // 3 — origin verification (R-11/M-12): the origin must be a typed
        // root-approval event matching the transition kind, with a
        // broker-issued (non-zero) decision digest and a present origin
        // session/event
        let expected_kind = match transition.kind {
            TransitionKind::RootApproval => "memory_root_approved",
            TransitionKind::Promotion => "memory_promotion_approved",
        };
        if transition.origin_kind != expected_kind {
            return Err(MemoryError::InvalidOrigin(format!(
                "{:?} requires origin_kind {expected_kind:?}, got {:?}",
                transition.kind, transition.origin_kind
            )));
        }
        if transition.decision_digest == zero_digest() {
            return Err(MemoryError::InvalidOrigin(
                "decision_digest is the zero digest (no broker approval)".into(),
            ));
        }
        if transition.origin_session == zero_id() {
            return Err(MemoryError::InvalidOrigin(
                "origin_session is the zero id".into(),
            ));
        }
        if transition.origin_event == 0 {
            return Err(MemoryError::InvalidOrigin("origin_event is zero".into()));
        }

        // 4 — CAS
        if transition.expected_old_root != self.head {
            let mut installed = claims.to_vec();
            for d in edges {
                if !installed.contains(d) {
                    installed.push(*d);
                }
            }
            return Ok(TransitionOutcome::CasFailed {
                expected: transition.expected_old_root,
                actual: self.head,
                installed,
            });
        }

        // 5 — refs-to-committed objects (R-12/M-01): every claim/edge digest
        // must exist and hash-verify in the store
        let mut added_claims: Vec<(Digest, Claim)> = Vec::with_capacity(claims.len());
        for d in claims {
            let bytes = self
                .store
                .get(d)
                .map_err(|_| MemoryError::MissingObject(*d))?;
            let claim: Claim = decode(&bytes, &format!("claim object {d}"))?;
            if claim.schema != crate::types::MEMORY_CLAIM_SCHEMA {
                return Err(MemoryError::Corrupt {
                    context: format!(
                        "claim object {d}: schema {}, expected {}",
                        claim.schema,
                        crate::types::MEMORY_CLAIM_SCHEMA
                    ),
                });
            }
            added_claims.push((*d, claim));
        }
        let mut added_edges: Vec<(Digest, ClaimEdge)> = Vec::with_capacity(edges.len());
        for d in edges {
            let bytes = self
                .store
                .get(d)
                .map_err(|_| MemoryError::MissingObject(*d))?;
            let edge: ClaimEdge = decode(&bytes, &format!("edge object {d}"))?;
            if edge.schema != crate::types::MEMORY_EDGE_SCHEMA {
                return Err(MemoryError::Corrupt {
                    context: format!(
                        "edge object {d}: schema {}, expected {}",
                        edge.schema,
                        crate::types::MEMORY_EDGE_SCHEMA
                    ),
                });
            }
            added_edges.push((*d, edge));
        }

        // 6 — acyclicity: canonical dependency edges point only to
        // already-committed claims; `from` must be a committed or newly
        // added claim
        let fold = self.fold(self.head)?;
        let mut committed: HashMap<Id128, Digest> = HashMap::new();
        for (d, c) in fold.claims.iter().chain(fold.retracted.iter()) {
            committed.insert(c.claim_id, *d);
        }
        let mut added_ids: HashMap<Id128, Digest> = HashMap::new();
        for (d, c) in &added_claims {
            added_ids.insert(c.claim_id, *d);
        }
        for (_, edge) in &added_edges {
            if let Some(to) = edge.to
                && !committed.contains_key(&to)
            {
                return Err(MemoryError::AcyclicViolation(format!(
                    "edge {} -> {to}: target claim is not committed in the fold",
                    edge.from
                )));
            }
            if !committed.contains_key(&edge.from) && !added_ids.contains_key(&edge.from) {
                return Err(MemoryError::AcyclicViolation(format!(
                    "edge from {}: claim is neither committed in the fold nor added by this transition",
                    edge.from
                )));
            }
        }

        // 7 — build and install the root-manifest delta; its digest must
        // equal the transition's accepted new root. Supersedes edges (with or
        // without successor) retract `from`.
        let mut id_to_digest: HashMap<Id128, Digest> = HashMap::new();
        id_to_digest.extend(committed);
        id_to_digest.extend(added_ids);
        let mut retracted: Vec<Digest> = added_edges
            .iter()
            .filter(|(_, e)| e.kind == EdgeKind::Supersedes)
            .filter_map(|(_, e)| id_to_digest.get(&e.from).copied())
            .collect();
        retracted.sort_unstable();
        retracted.dedup();
        let manifest = RootManifest {
            schema: MEMORY_ROOT_SCHEMA,
            parent: self.head,
            scope: self.scope.clone(),
            added_claims: claims.to_vec(),
            added_edges: edges.to_vec(),
            retracted,
            transition_id: transition.transition_id,
        };
        let manifest_digest = manifest.digest();
        if manifest_digest != transition.accepted_new_root {
            return Err(MemoryError::RootMismatch {
                expected: transition.accepted_new_root,
                actual: manifest_digest,
            });
        }
        // install enqueues the objects-dir dirsync BEFORE the frame fsync
        // below — the manifest object is durable before the referencing
        // frame (R-10)
        let installed = self.store.install(&canonical_bytes(&manifest))?;
        debug_assert_eq!(installed, manifest_digest);

        // 8 — one transition frame
        let seq = self.log.seq();
        let envelope = Envelope {
            env: ENVELOPE_SCHEMA,
            seq,
            evt: Id128::generate().to_string(),
            kind: "memory_transition".into(),
            payload_schema: MEMORY_TRANSITION_SCHEMA,
            payload: serde_json::to_value(&transition)
                .map_err(|e| MemoryError::InvalidInput(format!("transition serialization: {e}")))?,
            refs: vec![manifest_digest],
            snapshot: None,
        };
        self.fault_at(MemoryFaultPoint::BeforeTransition);
        self.log.append(&[envelope], Profile::Strict)?;
        self.fault_at(MemoryFaultPoint::AfterTransition);

        // 9-10 — atomic head pointer: temp write + rename, then dirsyncs for
        // the scope dir (covering the rename) and the objects dir (covering
        // the manifest install)
        self.fault_at(MemoryFaultPoint::BeforeHeadUpdate);
        write_head_atomic(
            &self.dir,
            &HeadFile {
                schema: 1,
                root: Some(manifest_digest),
                transition_id: Some(transition.transition_id),
            },
        )?;
        self.fault_at(MemoryFaultPoint::AfterHeadUpdate);
        self.queue.enqueue(SyncOp::Dirsync(self.dir.clone()))?;
        self.queue
            .enqueue(SyncOp::Dirsync(self.dir.join("objects")))?;
        self.head = Some(manifest_digest);
        self.head_transition = Some(transition.transition_id);
        self.seen_keys.push(transition.idempotency_key.clone());
        self.origins
            .push((transition.origin_session, transition.transition_id));

        // 11 — the commit is authoritative: barrier until every enqueued op
        // (manifest dirsync, frame fsync, head rename dirsyncs) has run
        self.queue.flush()?;

        Ok(TransitionOutcome::Committed {
            transition_id: transition.transition_id,
            new_root: manifest_digest,
        })
    }

    /// The projection-time fold (R-12/M-09) over the manifest chain rooted at
    /// `root`: applies deltas oldest → newest, keeping active claims, active
    /// edges, superseded/retracted claims (for contradiction annotation), and
    /// the genesis-first manifest history.
    pub fn fold(&self, root: Option<Digest>) -> Result<RootFold, MemoryError> {
        let mut manifests: Vec<(Digest, RootManifest)> = Vec::new();
        let mut current = root;
        while let Some(digest) = current {
            let bytes = self
                .store
                .get(&digest)
                .map_err(|_| MemoryError::MissingObject(digest))?;
            let manifest: RootManifest = decode(&bytes, &format!("root manifest {digest}"))?;
            if manifest.schema != MEMORY_ROOT_SCHEMA {
                return Err(MemoryError::Corrupt {
                    context: format!(
                        "root manifest {digest}: schema {}, expected {MEMORY_ROOT_SCHEMA}",
                        manifest.schema
                    ),
                });
            }
            current = manifest.parent;
            manifests.push((digest, manifest));
        }

        let mut claims: Vec<(Digest, Claim)> = Vec::new();
        let mut edges: Vec<(Digest, ClaimEdge)> = Vec::new();
        let mut retracted: Vec<(Digest, Claim)> = Vec::new();
        let history: Vec<Digest> = manifests.iter().rev().map(|(d, _)| *d).collect();
        for (_, manifest) in manifests.iter().rev() {
            for d in &manifest.added_claims {
                if claims.iter().any(|(c, _)| c == d) || retracted.iter().any(|(c, _)| c == d) {
                    continue;
                }
                let bytes = self
                    .store
                    .get(d)
                    .map_err(|_| MemoryError::MissingObject(*d))?;
                let claim: Claim = decode(&bytes, &format!("claim object {d}"))?;
                claims.push((*d, claim));
            }
            for d in &manifest.added_edges {
                if edges.iter().any(|(e, _)| e == d) {
                    continue;
                }
                let bytes = self
                    .store
                    .get(d)
                    .map_err(|_| MemoryError::MissingObject(*d))?;
                let edge: ClaimEdge = decode(&bytes, &format!("edge object {d}"))?;
                edges.push((*d, edge));
            }
            for d in &manifest.retracted {
                if retracted.iter().any(|(c, _)| c == d) {
                    continue;
                }
                if let Some(pos) = claims.iter().position(|(c, _)| c == d) {
                    retracted.push(claims.remove(pos));
                } else {
                    // retraction of a claim not in the active set (e.g. a
                    // double retraction): keep it visible in the history
                    let bytes = self
                        .store
                        .get(d)
                        .map_err(|_| MemoryError::MissingObject(*d))?;
                    let claim: Claim = decode(&bytes, &format!("claim object {d}"))?;
                    retracted.push((*d, claim));
                }
            }
        }
        Ok(RootFold {
            root,
            claims,
            edges,
            retracted,
            history,
        })
    }

    /// Barrier: waits until every enqueued durability op has run.
    pub fn flush(&self) -> Result<(), MemoryError> {
        Ok(self.queue.flush()?)
    }

    /// Transition ids of all committed transitions originating from
    /// `session` (R-11 backlink candidates).
    pub fn scan_backlink_candidates(&self, session: Id128) -> Vec<Id128> {
        self.origins
            .iter()
            .filter(|(s, _)| *s == session)
            .map(|(_, t)| *t)
            .collect()
    }

    /// Number of committed transitions (test helper).
    pub fn transition_count(&self) -> u64 {
        self.seen_keys.len() as u64
    }

    /// Install or clear the commit-path fault injector.
    pub fn set_fault(&mut self, fault: Option<Arc<dyn MemoryFaultInjector>>) {
        self.fault = fault;
    }

    /// The scope object store (the session installs claim/edge objects
    /// through it before proposing).
    pub fn store(&self) -> &ObjectStore {
        &self.store
    }

    /// The scope this actor owns.
    pub fn scope(&self) -> &MemoryScope {
        &self.scope
    }

    /// Fire the configured injector at `point`; no-op when none is set.
    fn fault_at(&self, point: MemoryFaultPoint) {
        if let Some(f) = &self.fault {
            f.inject(point);
        }
    }
}

// ---------- helpers ----------

/// `recover` errors on a missing file; a fresh scope dir is a valid genesis
/// state (mirrors kanbei-session's recover_or_fresh).
fn recover_or_fresh(log_path: &Path) -> Result<Recovered, MemoryError> {
    match std::fs::metadata(log_path) {
        Ok(m) if m.is_file() => Ok(kanbei_log::recover(log_path)?),
        Ok(_) => Err(MemoryError::InvalidInput(format!(
            "log path is not a file: {}",
            log_path.display()
        ))),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Recovered {
            events: 0,
            frames: 0,
            truncated: false,
            last_seq: 0,
        }),
        Err(e) => Err(e.into()),
    }
}

/// Reads every committed transition: the last committed root, all idempotency
/// keys, and the origin-session → transition-id pairs. Any non-transition or
/// unparseable record in the scope log is corruption.
fn scan_log(path: &Path) -> Result<LogScan, MemoryError> {
    let mut scan = LogScan::default();
    let mut first_err: Option<MemoryError> = None;
    kanbei_log::for_each_frame(path, |frame| {
        if first_err.is_some() {
            return;
        }
        for line in &frame.events {
            let envelope = match Envelope::from_line(line) {
                Ok(e) => e,
                Err(e) => {
                    first_err = Some(MemoryError::Corrupt {
                        context: format!("transition log envelope parse: {e}"),
                    });
                    return;
                }
            };
            if envelope.kind != "memory_transition" {
                first_err = Some(MemoryError::Corrupt {
                    context: format!(
                        "transition log seq {}: kind {:?}, expected \"memory_transition\"",
                        envelope.seq, envelope.kind
                    ),
                });
                return;
            }
            let transition: MemoryTransition = match serde_json::from_value(envelope.payload) {
                Ok(t) => t,
                Err(e) => {
                    first_err = Some(MemoryError::Corrupt {
                        context: format!("transition log seq {}: payload parse: {e}", envelope.seq),
                    });
                    return;
                }
            };
            if transition.schema != MEMORY_TRANSITION_SCHEMA {
                first_err = Some(MemoryError::Corrupt {
                    context: format!(
                        "transition log seq {}: schema {}, expected {MEMORY_TRANSITION_SCHEMA}",
                        envelope.seq, transition.schema
                    ),
                });
                return;
            }
            scan.keys.push(transition.idempotency_key.clone());
            scan.origins
                .push((transition.origin_session, transition.transition_id));
            scan.last = Some((transition.accepted_new_root, transition.transition_id));
        }
    })?;
    if let Some(e) = first_err {
        return Err(e);
    }
    Ok(scan)
}

/// Reads `head.json`; `None` for a missing or unparseable file (both trigger
/// the head repair rule).
fn read_head_file(dir: &Path) -> Option<HeadFile> {
    let bytes = match std::fs::read(dir.join("head.json")) {
        Ok(bytes) => bytes,
        Err(_) => return None,
    };
    serde_json::from_slice(&bytes).ok()
}

/// Atomic head write: temp file + rename. The caller enqueues the scope-dir
/// dirsync after the rename so the rename itself is covered.
fn write_head_atomic(dir: &Path, head: &HeadFile) -> io::Result<()> {
    let path = dir.join("head.json");
    let tmp = dir.join(format!(".tmp-head-{}", std::process::id()));
    let mut f = File::create(&tmp)?;
    f.write_all(&serde_json::to_vec(head).expect("head serialization cannot fail"))?;
    drop(f);
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn decode<T: serde::de::DeserializeOwned>(bytes: &[u8], what: &str) -> Result<T, MemoryError> {
    serde_json::from_slice(bytes).map_err(|e| MemoryError::Corrupt {
        context: format!("{what}: {e}"),
    })
}

/// Best-effort queue shutdown on a failed open (mirrors kanbei-session).
fn shutdown_queue(queue: Arc<DurabilityQueue>) {
    if let Ok(queue) = Arc::try_unwrap(queue) {
        let _ = queue.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ClaimProvenance, MEMORY_CLAIM_SCHEMA};
    use kanbei_capabilities::Principal;
    use std::collections::HashSet;
    use std::sync::Mutex;

    fn tmp_root(tag: &str) -> PathBuf {
        let p = std::env::temp_dir().join(format!("kb-memory-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&p);
        p
    }

    /// Installs objects into a scope's objects dir through an independent
    /// store handle (the caller's duty — the actor only verifies refs).
    fn install_objects(objects_dir: &Path, objects: &[Vec<u8>]) -> Vec<Digest> {
        let queue = Arc::new(DurabilityQueue::start("kb-memory-test-install"));
        let mut store = ObjectStore::open(objects_dir, Arc::clone(&queue)).unwrap();
        let digests: Vec<Digest> = objects.iter().map(|b| store.install(b).unwrap()).collect();
        store.flush().unwrap();
        drop(store);
        Arc::try_unwrap(queue)
            .unwrap_or_else(|_| panic!("install queue Arc still shared"))
            .shutdown()
            .unwrap();
        digests
    }

    /// Records every injected point; the crash child later aborts instead.
    struct RecordingInjector(Arc<Mutex<Vec<MemoryFaultPoint>>>);

    impl MemoryFaultInjector for RecordingInjector {
        fn inject(&self, point: MemoryFaultPoint) {
            self.0.lock().unwrap().push(point);
        }
    }

    fn principal(session: Id128) -> Principal {
        Principal {
            session,
            generation: 1,
            run: None,
        }
    }

    fn decision_digest(session: Id128, event: u64) -> Digest {
        Digest::new(format!("decision-{session}-{event}").as_bytes())
    }

    fn make_claim(scope: &MemoryScope, session: Id128, text: &str) -> Claim {
        Claim {
            schema: MEMORY_CLAIM_SCHEMA,
            claim_id: Id128::generate(),
            kind: "decision".into(),
            content: text.into(),
            owner: principal(session),
            visibility_scope: scope.clone(),
            provenance: ClaimProvenance::new_ordinary(session, 1),
            observed_at: Some(1_700_000_000),
            valid_from: None,
            sensitivity: "public".into(),
        }
    }

    fn make_edge(from: Id128, to: Option<Id128>, kind: EdgeKind, session: Id128) -> ClaimEdge {
        ClaimEdge::new(
            from,
            to,
            kind,
            vec![],
            ClaimProvenance::new_ordinary(session, 1),
        )
        .unwrap()
    }

    /// Builds the transition the caller would construct: the manifest the
    /// actor will build internally (parent = actor's current head, retracted
    /// derived from Supersedes edges), so `accepted_new_root` matches.
    fn make_transition(
        actor: &MemoryRootActor,
        tid: Id128,
        claims: &[Claim],
        edges: &[ClaimEdge],
        session: Id128,
        event: u64,
        kind: TransitionKind,
    ) -> MemoryTransition {
        let claim_digests: Vec<Digest> = claims.iter().map(|c| c.digest()).collect();
        let edge_digests: Vec<Digest> = edges.iter().map(|e| e.digest()).collect();
        let fold = actor.fold(actor.head()).unwrap();
        let mut id_to_digest: HashMap<Id128, Digest> = HashMap::new();
        for (d, c) in fold.claims.iter().chain(fold.retracted.iter()) {
            id_to_digest.insert(c.claim_id, *d);
        }
        for (c, d) in claims.iter().zip(&claim_digests) {
            id_to_digest.insert(c.claim_id, *d);
        }
        let mut retracted: Vec<Digest> = edges
            .iter()
            .filter(|e| e.kind == EdgeKind::Supersedes)
            .filter_map(|e| id_to_digest.get(&e.from).copied())
            .collect();
        retracted.sort_unstable();
        retracted.dedup();
        let manifest = RootManifest {
            schema: MEMORY_ROOT_SCHEMA,
            parent: actor.head(),
            scope: actor.scope.clone(),
            added_claims: claim_digests.clone(),
            added_edges: edge_digests.clone(),
            retracted,
            transition_id: tid,
        };
        let origin_kind = match kind {
            TransitionKind::RootApproval => "memory_root_approved",
            TransitionKind::Promotion => "memory_promotion_approved",
        };
        MemoryTransition {
            schema: MEMORY_TRANSITION_SCHEMA,
            transition_id: tid,
            scope: actor.scope.clone(),
            kind: kind.clone(),
            expected_old_root: actor.head(),
            accepted_new_root: manifest.digest(),
            origin_session: session,
            origin_event: event,
            origin_kind: origin_kind.into(),
            decision_principal: principal(session),
            decision_digest: decision_digest(session, event),
            idempotency_key: IdempotencyKey {
                session,
                event,
                decision: decision_digest(session, event),
            },
        }
    }

    /// Installs claim/edge objects and proposes; panics unless committed.
    fn commit(
        actor: &mut MemoryRootActor,
        tid: Id128,
        claims: &[Claim],
        edges: &[ClaimEdge],
        session: Id128,
        event: u64,
    ) -> Digest {
        let claim_digests: Vec<Digest> = claims.iter().map(|c| c.digest()).collect();
        let edge_digests: Vec<Digest> = edges.iter().map(|e| e.digest()).collect();
        let mut objects: Vec<Vec<u8>> = Vec::new();
        objects.extend(claims.iter().map(|c| c.to_canonical_bytes()));
        objects.extend(edges.iter().map(|e| e.to_canonical_bytes()));
        install_objects(&actor.dir.join("objects"), &objects);
        let transition = make_transition(
            actor,
            tid,
            claims,
            edges,
            session,
            event,
            TransitionKind::RootApproval,
        );
        match actor
            .propose(transition, &claim_digests, &edge_digests)
            .expect("propose should commit")
        {
            TransitionOutcome::Committed { new_root, .. } => new_root,
            other => panic!("expected Committed, got {other:?}"),
        }
    }

    #[test]
    fn fold_applies_deltas_genesis_first() {
        let root = tmp_root("fold");
        let scope = MemoryScope::Lifetime;
        let actor = MemoryRootActor::open(&root, scope.clone()).unwrap();
        let session = Id128::generate();
        let c1 = make_claim(&scope, session, "one");
        let c2 = make_claim(&scope, session, "two");
        let c3 = make_claim(&scope, session, "three");
        let d1 = c1.digest();
        let d2 = c2.digest();
        let d3 = c3.digest();
        let m1 = RootManifest {
            schema: MEMORY_ROOT_SCHEMA,
            parent: None,
            scope: scope.clone(),
            added_claims: vec![d1, d2],
            added_edges: vec![],
            retracted: vec![],
            transition_id: Id128::generate(),
        };
        let m1d = m1.digest();
        let m2 = RootManifest {
            schema: MEMORY_ROOT_SCHEMA,
            parent: Some(m1d),
            scope: scope.clone(),
            added_claims: vec![d3],
            added_edges: vec![],
            retracted: vec![d1],
            transition_id: Id128::generate(),
        };
        let m2d = m2.digest();
        let objects = vec![
            c1.to_canonical_bytes(),
            c2.to_canonical_bytes(),
            c3.to_canonical_bytes(),
            m1.to_canonical_bytes(),
            m2.to_canonical_bytes(),
        ];
        install_objects(&actor.dir.join("objects"), &objects);

        // empty fold at None
        let empty = actor.fold(None).unwrap();
        assert_eq!(empty.root, None);
        assert!(empty.claims.is_empty() && empty.history.is_empty());

        let fold = actor.fold(Some(m2d)).unwrap();
        assert_eq!(fold.root, Some(m2d));
        assert_eq!(fold.history, vec![m1d, m2d]);
        let active: Vec<Id128> = fold.claims.iter().map(|(_, c)| c.claim_id).collect();
        assert_eq!(active, vec![c2.claim_id, c3.claim_id]);
        let retr: Vec<Id128> = fold.retracted.iter().map(|(_, c)| c.claim_id).collect();
        assert_eq!(retr, vec![c1.claim_id]);
        assert!(fold.edges.is_empty());
        drop(actor);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fold_dedups_claims_by_digest() {
        let root = tmp_root("fold-dedup");
        let scope = MemoryScope::Lifetime;
        let actor = MemoryRootActor::open(&root, scope.clone()).unwrap();
        let session = Id128::generate();
        let c1 = make_claim(&scope, session, "same");
        let d1 = c1.digest();
        let m1 = RootManifest {
            schema: MEMORY_ROOT_SCHEMA,
            parent: None,
            scope: scope.clone(),
            added_claims: vec![d1],
            added_edges: vec![],
            retracted: vec![],
            transition_id: Id128::generate(),
        };
        let m1d = m1.digest();
        let m2 = RootManifest {
            schema: MEMORY_ROOT_SCHEMA,
            parent: Some(m1d),
            scope: scope.clone(),
            added_claims: vec![d1],
            added_edges: vec![],
            retracted: vec![],
            transition_id: Id128::generate(),
        };
        let m2d = m2.digest();
        let objects = vec![
            c1.to_canonical_bytes(),
            m1.to_canonical_bytes(),
            m2.to_canonical_bytes(),
        ];
        install_objects(&actor.dir.join("objects"), &objects);
        let fold = actor.fold(Some(m2d)).unwrap();
        assert_eq!(fold.claims.len(), 1);
        assert_eq!(fold.claims[0].0, d1);
        drop(actor);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn actor_cas_genesis_stale_retry() {
        let root = tmp_root("cas");
        let scope = MemoryScope::Lifetime;
        let mut actor = MemoryRootActor::open(&root, scope.clone()).unwrap();
        let session = Id128::generate();
        assert_eq!(actor.head(), None);
        assert_eq!(actor.transition_count(), 0);

        // genesis propose succeeds
        let c1 = make_claim(&scope, session, "first");
        let new_root = commit(
            &mut actor,
            Id128::generate(),
            std::slice::from_ref(&c1),
            &[],
            session,
            1,
        );
        assert_eq!(actor.head(), Some(new_root));
        assert!(actor.head_transition().is_some());

        // stale expected fails with the installed digests
        let c2 = make_claim(&scope, session, "second");
        let d2 = c2.digest();
        install_objects(&actor.dir.join("objects"), &[c2.to_canonical_bytes()]);
        let stale = make_transition(
            &actor,
            Id128::generate(),
            &[c2],
            &[],
            session,
            2,
            TransitionKind::RootApproval,
        );
        let stale = MemoryTransition {
            expected_old_root: Some(Digest::new(b"stale")),
            ..stale
        };
        let outcome = actor
            .propose(stale, &[d2], &[])
            .expect("CAS failure is an Ok outcome");
        match outcome {
            TransitionOutcome::CasFailed {
                expected,
                actual,
                installed,
            } => {
                assert_eq!(expected, Some(Digest::new(b"stale")));
                assert_eq!(actual, Some(new_root));
                assert_eq!(installed, vec![d2]);
            }
            other => panic!("expected CasFailed, got {other:?}"),
        }
        assert_eq!(actor.head(), Some(new_root));
        assert_eq!(actor.transition_count(), 1);

        // retry with the actual head succeeds
        let c3 = make_claim(&scope, session, "third");
        let new_root2 = commit(&mut actor, Id128::generate(), &[c3], &[], session, 3);
        assert_eq!(actor.head(), Some(new_root2));
        assert_eq!(actor.transition_count(), 2);
        drop(actor);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn duplicate_idempotency_key_rejected() {
        let root = tmp_root("idem");
        let scope = MemoryScope::Lifetime;
        let mut actor = MemoryRootActor::open(&root, scope.clone()).unwrap();
        let session = Id128::generate();
        let c1 = make_claim(&scope, session, "one");
        commit(
            &mut actor,
            Id128::generate(),
            std::slice::from_ref(&c1),
            &[],
            session,
            1,
        );

        // Same key (session/event/decision), correct expected root, new
        // transition id: still rejected.
        let c2 = make_claim(&scope, session, "two");
        let d2 = c2.digest();
        install_objects(&actor.dir.join("objects"), &[c2.to_canonical_bytes()]);
        let dup = make_transition(
            &actor,
            Id128::generate(),
            &[c2],
            &[],
            session,
            1, // same event → same idempotency key
            TransitionKind::RootApproval,
        );
        let err = actor.propose(dup, &[d2], &[]).unwrap_err();
        assert!(matches!(err, MemoryError::DuplicateTransition(_)));
        assert_eq!(actor.head(), Some(actor.head().unwrap()));
        assert_eq!(actor.transition_count(), 1);
        drop(actor);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn origin_verification_rejects_bad_kind_and_zero_fields() {
        let root = tmp_root("origin");
        let scope = MemoryScope::Lifetime;
        let mut actor = MemoryRootActor::open(&root, scope.clone()).unwrap();
        let session = Id128::generate();
        let c1 = make_claim(&scope, session, "one");
        let d1 = c1.digest();
        install_objects(&actor.dir.join("objects"), &[c1.to_canonical_bytes()]);

        // RootApproval with the promotion origin kind
        let mut t = make_transition(
            &actor,
            Id128::generate(),
            std::slice::from_ref(&c1),
            &[],
            session,
            1,
            TransitionKind::RootApproval,
        );
        t.origin_kind = "memory_promotion_approved".into();
        let err = actor.propose(t.clone(), &[d1], &[]).unwrap_err();
        assert!(matches!(err, MemoryError::InvalidOrigin(_)));

        // Promotion with the root-approval origin kind
        let mut p = make_transition(
            &actor,
            Id128::generate(),
            std::slice::from_ref(&c1),
            &[],
            session,
            1,
            TransitionKind::Promotion,
        );
        p.origin_kind = "memory_root_approved".into();
        let err = actor.propose(p, &[d1], &[]).unwrap_err();
        assert!(matches!(err, MemoryError::InvalidOrigin(_)));

        // Zero decision digest
        let mut z = t.clone();
        z.decision_digest = zero_digest();
        let err = actor.propose(z, &[d1], &[]).unwrap_err();
        assert!(matches!(err, MemoryError::InvalidOrigin(_)));

        // Zero origin session
        let mut s = t.clone();
        s.origin_session = zero_id();
        let err = actor.propose(s, &[d1], &[]).unwrap_err();
        assert!(matches!(err, MemoryError::InvalidOrigin(_)));

        // Zero origin event
        let mut e = t.clone();
        e.origin_event = 0;
        let err = actor.propose(e, &[d1], &[]).unwrap_err();
        assert!(matches!(err, MemoryError::InvalidOrigin(_)));

        assert_eq!(actor.transition_count(), 0);
        drop(actor);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn acyclicity_enforced() {
        let root = tmp_root("acyclic");
        let scope = MemoryScope::Lifetime;
        let mut actor = MemoryRootActor::open(&root, scope.clone()).unwrap();
        let session = Id128::generate();
        let c1 = make_claim(&scope, session, "one");
        let stranger = Id128::generate();
        let d1 = c1.digest();

        // `to` not in the fold (genesis fold is empty)
        let edge = make_edge(c1.claim_id, Some(stranger), EdgeKind::Supports, session);
        let d_edge = edge.digest();
        install_objects(
            &actor.dir.join("objects"),
            &[c1.to_canonical_bytes(), edge.to_canonical_bytes()],
        );
        let t = make_transition(
            &actor,
            Id128::generate(),
            std::slice::from_ref(&c1),
            &[edge],
            session,
            1,
            TransitionKind::RootApproval,
        );
        let err = actor.propose(t, &[d1], &[d_edge]).unwrap_err();
        assert!(matches!(err, MemoryError::AcyclicViolation(_)));

        // `from` neither committed nor added
        let edge2 = make_edge(stranger, Some(c1.claim_id), EdgeKind::Supports, session);
        let d_edge2 = edge2.digest();
        install_objects(&actor.dir.join("objects"), &[edge2.to_canonical_bytes()]);
        let t = make_transition(
            &actor,
            Id128::generate(),
            std::slice::from_ref(&c1),
            &[edge2],
            session,
            1,
            TransitionKind::RootApproval,
        );
        let err = actor.propose(t, &[d1], &[d_edge2]).unwrap_err();
        assert!(matches!(err, MemoryError::AcyclicViolation(_)));

        // Valid: supersedes with to: None (retraction) at genesis
        let retract = make_edge(c1.claim_id, None, EdgeKind::Supersedes, session);
        let d_retract = retract.digest();
        install_objects(&actor.dir.join("objects"), &[retract.to_canonical_bytes()]);
        let t = make_transition(
            &actor,
            Id128::generate(),
            std::slice::from_ref(&c1),
            &[retract],
            session,
            2,
            TransitionKind::RootApproval,
        );
        match actor
            .propose(t, &[d1], &[d_retract])
            .expect("retraction should commit")
        {
            TransitionOutcome::Committed { .. } => {}
            other => panic!("expected Committed, got {other:?}"),
        }
        let fold = actor.fold(actor.head()).unwrap();
        assert!(fold.claims.is_empty());
        assert_eq!(fold.retracted.len(), 1);
        assert_eq!(fold.retracted[0].0, d1);
        drop(actor);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn missing_claim_object_rejected() {
        let root = tmp_root("missing");
        let scope = MemoryScope::Lifetime;
        let mut actor = MemoryRootActor::open(&root, scope.clone()).unwrap();
        let session = Id128::generate();
        let c1 = make_claim(&scope, session, "ghost");
        let d1 = c1.digest();
        let edge = make_edge(c1.claim_id, None, EdgeKind::Supersedes, session);
        let d_edge = edge.digest();

        // claim digest never installed
        let t = make_transition(
            &actor,
            Id128::generate(),
            &[c1],
            &[],
            session,
            1,
            TransitionKind::RootApproval,
        );
        let err = actor.propose(t, &[d1], &[]).unwrap_err();
        assert!(matches!(err, MemoryError::MissingObject(d) if d == d1));

        // edge digest never installed
        let t = make_transition(
            &actor,
            Id128::generate(),
            &[],
            &[edge],
            session,
            1,
            TransitionKind::RootApproval,
        );
        let err = actor.propose(t, &[], &[d_edge]).unwrap_err();
        assert!(matches!(err, MemoryError::MissingObject(d) if d == d_edge));
        assert_eq!(actor.transition_count(), 0);
        drop(actor);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn root_mismatch_rejected() {
        let root = tmp_root("mismatch");
        let scope = MemoryScope::Lifetime;
        let mut actor = MemoryRootActor::open(&root, scope.clone()).unwrap();
        let session = Id128::generate();
        let c1 = make_claim(&scope, session, "one");
        let d1 = c1.digest();
        install_objects(&actor.dir.join("objects"), &[c1.to_canonical_bytes()]);
        let mut t = make_transition(
            &actor,
            Id128::generate(),
            &[c1],
            &[],
            session,
            1,
            TransitionKind::RootApproval,
        );
        t.accepted_new_root = Digest::new(b"forged manifest");
        match actor.propose(t, &[d1], &[]).unwrap_err() {
            MemoryError::RootMismatch { expected, actual } => {
                assert_eq!(expected, Digest::new(b"forged manifest"));
                assert_ne!(actual, expected);
            }
            other => panic!("expected RootMismatch, got {other:?}"),
        }
        assert_eq!(actor.transition_count(), 0);
        drop(actor);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn head_repair_on_missing_and_corrupt() {
        let root = tmp_root("head-repair");
        let scope = MemoryScope::Lifetime;
        let session = Id128::generate();
        let tid2 = Id128::generate();
        let root2;
        {
            let mut actor = MemoryRootActor::open(&root, scope.clone()).unwrap();
            let c1 = make_claim(&scope, session, "one");
            commit(&mut actor, Id128::generate(), &[c1], &[], session, 1);
            let c2 = make_claim(&scope, session, "two");
            root2 = commit(&mut actor, tid2, &[c2], &[], session, 2);
        }
        let head_path = root.join("lifetime/head.json");
        assert!(head_path.exists());

        // missing head.json → repaired from the log
        std::fs::remove_file(&head_path).unwrap();
        let actor = MemoryRootActor::open(&root, scope.clone()).unwrap();
        assert_eq!(actor.head(), Some(root2));
        assert_eq!(actor.head_transition(), Some(tid2));
        drop(actor);
        assert!(head_path.exists());

        // corrupt head.json (garbage bytes) → repaired
        std::fs::write(&head_path, b"not json { garbage").unwrap();
        let actor = MemoryRootActor::open(&root, scope.clone()).unwrap();
        assert_eq!(actor.head(), Some(root2));
        assert_eq!(actor.head_transition(), Some(tid2));
        drop(actor);
        assert!(head_path.exists());
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn torn_tail_recovery() {
        let root = tmp_root("torn");
        let scope = MemoryScope::Lifetime;
        let session = Id128::generate();
        let tid1 = Id128::generate();
        let tid2 = Id128::generate();
        let tid3 = Id128::generate();
        let root2;
        {
            let mut actor = MemoryRootActor::open(&root, scope.clone()).unwrap();
            let c1 = make_claim(&scope, session, "one");
            commit(&mut actor, tid1, &[c1], &[], session, 1);
            let c2 = make_claim(&scope, session, "two");
            root2 = commit(&mut actor, tid2, &[c2], &[], session, 2);
            let c3 = make_claim(&scope, session, "three");
            commit(&mut actor, tid3, &[c3], &[], session, 3);
        }
        let log_path = root.join("lifetime/transitions.jsonl.zst");
        let len = std::fs::metadata(&log_path).unwrap().len();
        // Simulate a torn tail: cut a few bytes off the last frame.
        let f = std::fs::File::options()
            .write(true)
            .open(&log_path)
            .unwrap();
        f.set_len(len - 5).unwrap();
        drop(f);

        let actor = MemoryRootActor::open(&root, scope.clone()).unwrap();
        assert_eq!(actor.head(), Some(root2));
        assert_eq!(actor.head_transition(), Some(tid2));
        // The torn transition is gone; committed state is the last good one.
        assert_eq!(actor.transition_count(), 2);
        assert_eq!(actor.scan_backlink_candidates(session).len(), 2);
        drop(actor);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fold_over_multi_delta_chain_via_commits() {
        let root = tmp_root("multi-delta");
        let scope = MemoryScope::Lifetime;
        let mut actor = MemoryRootActor::open(&root, scope.clone()).unwrap();
        let session = Id128::generate();
        let c1 = make_claim(&scope, session, "one");
        let c2 = make_claim(&scope, session, "two");
        commit(
            &mut actor,
            Id128::generate(),
            &[c1.clone(), c2.clone()],
            &[],
            session,
            1,
        );

        // delta 2: commit the successor claim c3
        let c3 = make_claim(&scope, session, "three");
        commit(
            &mut actor,
            Id128::generate(),
            std::slice::from_ref(&c3),
            &[],
            session,
            2,
        );

        // delta 3: the supersedes edge departs from the superseded claim and
        // points to the already-committed successor (R-12/M-13: edges point
        // only to committed claims)
        let edge = make_edge(
            c1.claim_id,
            Some(c3.claim_id),
            EdgeKind::Supersedes,
            session,
        );
        commit(&mut actor, Id128::generate(), &[], &[edge], session, 3);

        let fold = actor.fold(actor.head()).unwrap();
        assert_eq!(fold.history.len(), 3);
        let active: Vec<Id128> = fold.claims.iter().map(|(_, c)| c.claim_id).collect();
        assert_eq!(active, vec![c2.claim_id, c3.claim_id]);
        let retr: Vec<Id128> = fold.retracted.iter().map(|(_, c)| c.claim_id).collect();
        assert_eq!(retr, vec![c1.claim_id]);
        assert_eq!(fold.edges.len(), 1);
        drop(actor);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn cross_instance_cas_determinism() {
        let root = tmp_root("cross");
        let scope = MemoryScope::Lifetime;
        let session_a = Id128::generate();
        let session_b = Id128::generate();
        let winner;
        {
            let mut a = MemoryRootActor::open(&root, scope.clone()).unwrap();
            let c1 = make_claim(&scope, session_a, "from a");
            winner = commit(&mut a, Id128::generate(), &[c1], &[], session_a, 1);
        }

        // Fresh instance over the same scope dir sees the committed state.
        let mut b = MemoryRootActor::open(&root, scope.clone()).unwrap();
        assert_eq!(b.head(), Some(winner));
        assert_eq!(b.transition_count(), 1);

        // Stale proposal (b was opened before a's commit — simulate by
        // proposing with the old expected root).
        let c2 = make_claim(&scope, session_b, "from b");
        let d2 = c2.digest();
        install_objects(&b.dir.join("objects"), &[c2.to_canonical_bytes()]);
        let stale = make_transition(
            &b,
            Id128::generate(),
            std::slice::from_ref(&c2),
            &[],
            session_b,
            1,
            TransitionKind::RootApproval,
        );
        let stale = MemoryTransition {
            expected_old_root: None, // the pre-commit genesis root
            ..stale
        };
        match b
            .propose(stale, &[d2], &[])
            .expect("CAS failure is an Ok outcome")
        {
            TransitionOutcome::CasFailed {
                expected: None,
                actual,
                installed,
            } => {
                assert_eq!(actual, Some(winner));
                assert_eq!(installed, vec![d2]);
            }
            other => panic!("expected genesis CasFailed, got {other:?}"),
        }
        assert_eq!(b.head(), Some(winner));

        // Rebase over the winner's root: new transition id + new key.
        let new_root = commit(&mut b, Id128::generate(), &[c2], &[], session_b, 2);
        assert_ne!(new_root, winner);
        let fold = b.fold(Some(new_root)).unwrap();
        let active: Vec<Id128> = fold.claims.iter().map(|(_, c)| c.claim_id).collect();
        assert_eq!(active.len(), 2);
        drop(b);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn scan_backlink_candidates_filters_by_session() {
        let root = tmp_root("backlinks");
        let scope = MemoryScope::Lifetime;
        let mut actor = MemoryRootActor::open(&root, scope.clone()).unwrap();
        let session_a = Id128::generate();
        let session_b = Id128::generate();
        let ta1 = Id128::generate();
        let ta2 = Id128::generate();
        let tb1 = Id128::generate();

        let c1 = make_claim(&scope, session_a, "a-one");
        commit(&mut actor, ta1, &[c1], &[], session_a, 1);
        let c2 = make_claim(&scope, session_b, "b-one");
        commit(&mut actor, tb1, &[c2], &[], session_b, 1);
        let c3 = make_claim(&scope, session_a, "a-two");
        commit(&mut actor, ta2, &[c3], &[], session_a, 2);

        let from_a: HashSet<Id128> = actor
            .scan_backlink_candidates(session_a)
            .into_iter()
            .collect();
        assert_eq!(from_a, HashSet::from([ta1, ta2]));
        let from_b: HashSet<Id128> = actor
            .scan_backlink_candidates(session_b)
            .into_iter()
            .collect();
        assert_eq!(from_b, HashSet::from([tb1]));
        assert!(actor.scan_backlink_candidates(Id128::generate()).is_empty());
        drop(actor);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn fault_points_fire_in_order_on_commit_path_only() {
        let root = tmp_root("fault");
        let scope = MemoryScope::Lifetime;
        let mut actor = MemoryRootActor::open(&root, scope.clone()).unwrap();
        let recorded = Arc::new(Mutex::new(Vec::new()));
        actor.set_fault(Some(Arc::new(RecordingInjector(Arc::clone(&recorded)))));
        let session = Id128::generate();

        // Successful commit fires all four points in order.
        let c1 = make_claim(&scope, session, "one");
        commit(&mut actor, Id128::generate(), &[c1], &[], session, 1);
        assert_eq!(
            *recorded.lock().unwrap(),
            vec![
                MemoryFaultPoint::BeforeTransition,
                MemoryFaultPoint::AfterTransition,
                MemoryFaultPoint::BeforeHeadUpdate,
                MemoryFaultPoint::AfterHeadUpdate,
            ]
        );

        // A stale CAS proposal is a rejection, not a commit: nothing fires.
        recorded.lock().unwrap().clear();
        let c2 = make_claim(&scope, session, "two");
        let d2 = c2.digest();
        install_objects(&actor.dir.join("objects"), &[c2.to_canonical_bytes()]);
        let stale = make_transition(
            &actor,
            Id128::generate(),
            &[c2],
            &[],
            session,
            2,
            TransitionKind::RootApproval,
        );
        let stale = MemoryTransition {
            expected_old_root: Some(Digest::new(b"stale")),
            ..stale
        };
        match actor
            .propose(stale, &[d2], &[])
            .expect("CAS failure is an Ok outcome")
        {
            TransitionOutcome::CasFailed { .. } => {}
            other => panic!("expected CasFailed, got {other:?}"),
        }
        assert!(
            recorded.lock().unwrap().is_empty(),
            "no fault point may fire on a rejected proposal"
        );
        drop(actor);
        let _ = std::fs::remove_dir_all(&root);
    }
}
