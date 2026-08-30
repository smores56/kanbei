//! Integration tests for kanbei-session M9 wave 5b: `Session::adopt` (adopt a
//! fork's outcome as the active perpetual root after reconciling domain
//! state — the canonical `fork_adopted` fact, quiesce of the active run,
//! pinned follow roots) and `Session::import` (verbatim backup/restore
//! preserving IDs — identical envelopes, branch records, memory heads).
//! Mirrors the wave 5a fork.rs test style.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use kanbei_core::digest::Digest;
use kanbei_core::envelope::Envelope;
use kanbei_core::id::Id128;
use kanbei_core::queue::DurabilityQueue;
use kanbei_log::for_each_frame;
use kanbei_memory::{
    Claim, ClaimProvenance, IdempotencyKey, MEMORY_CLAIM_SCHEMA, MEMORY_ROOT_SCHEMA,
    MEMORY_TRANSITION_SCHEMA, MemoryFollowPolicy, MemoryRootActor, MemoryScope, MemoryTransition,
    RootManifest, TransitionKind, TransitionOutcome,
};
use kanbei_objects::ObjectStore;
use kanbei_policy::builtins::StoreAllPolicy;
use kanbei_session::{
    CheckpointRef, ForkOptions, NewEvent, PinnedRoots, Session, SessionConfig, SessionError,
};
use serde_json::{Value, json};

// --- helpers (mirror fork.rs) ----------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kb-session-adopt-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn event(kind: &str, payload: Value) -> NewEvent {
    NewEvent {
        kind: kind.into(),
        payload_schema: 1,
        payload,
        objects: Vec::new(),
        refs: Vec::new(),
    }
}

/// All envelopes currently in the log, in seq order.
fn envelopes(log_path: &Path) -> Vec<Envelope> {
    let mut out = Vec::new();
    for_each_frame(log_path, |frame| {
        for line in &frame.events {
            out.push(Envelope::from_line(line).unwrap());
        }
    })
    .unwrap();
    out
}

/// A source session over `dir` with its own memory root and identity.
fn open_source(dir: &Path, session_id: Id128) -> Session {
    Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        memory_root: Some(dir.join("memory")),
        session_id: Some(session_id),
        ..Default::default()
    })
    .unwrap()
}

/// Default fork options: a fresh identity, StoreAll retention, and a default
/// session lane.
fn fork_options(target: &Path) -> ForkOptions {
    ForkOptions {
        target_dir: target.to_path_buf(),
        session_id: None,
        policy: Arc::new(StoreAllPolicy),
        config: SessionConfig::default(),
    }
}

/// Commits one claim root in `scope` via a standalone actor (the gate_m4
/// seeding pattern): the session's actor picks the root up at open. Returns
/// the root digest.
fn seed_claim(memory_root: &Path, scope: MemoryScope, session_id: Id128, text: &str) -> Digest {
    let claim = Claim {
        schema: MEMORY_CLAIM_SCHEMA,
        claim_id: Id128::generate(),
        kind: "decision".into(),
        content: text.into(),
        owner: kanbei_capabilities::Principal {
            session: session_id,
            generation: 0,
            run: None,
        },
        visibility_scope: scope.clone(),
        provenance: ClaimProvenance::new_ordinary(session_id, 1),
        observed_at: Some(1_700_000_000),
        valid_from: None,
        sensitivity: "public".into(),
    };
    let mut actor = MemoryRootActor::open(memory_root, scope.clone()).unwrap();
    let expected_old_root = actor.head();
    let queue = Arc::new(DurabilityQueue::start("kb-adopt-seed"));
    let mut store =
        ObjectStore::open(&memory_root.join(scope.dir_name()).join("objects"), Arc::clone(&queue))
            .unwrap();
    let d = store.install(&claim.to_canonical_bytes()).unwrap();
    store.flush().unwrap();
    drop(store);
    if let Ok(q) = Arc::try_unwrap(queue) {
        let _ = q.shutdown();
    }
    let manifest = RootManifest {
        schema: MEMORY_ROOT_SCHEMA,
        parent: expected_old_root,
        scope: scope.clone(),
        added_claims: vec![d],
        added_edges: vec![],
        retracted: vec![],
        transition_id: Id128::generate(),
    };
    let transition = MemoryTransition {
        schema: MEMORY_TRANSITION_SCHEMA,
        transition_id: manifest.transition_id,
        scope: scope.clone(),
        kind: TransitionKind::RootApproval,
        expected_old_root,
        accepted_new_root: manifest.digest(),
        origin_session: session_id,
        origin_event: 1,
        origin_kind: "memory_root_approved".into(),
        decision_principal: kanbei_capabilities::Principal {
            session: session_id,
            generation: 0,
            run: None,
        },
        decision_digest: Digest::new(format!("adopt-seed-decision-{text}").as_bytes()),
        idempotency_key: IdempotencyKey {
            session: session_id,
            event: 1,
            decision: Digest::new(format!("adopt-seed-decision-{text}").as_bytes()),
        },
    };
    match actor.propose(transition, &[d], &[]).unwrap() {
        TransitionOutcome::Committed { .. } => {}
        other => panic!("seed propose: expected Committed, got {other:?}"),
    }
    actor.flush().unwrap();
    manifest.digest()
}

/// A fork of `source` at `checkpoint` that has committed one extra event —
/// a head to adopt.
fn fork_with_outcome(
    source: &Session,
    checkpoint: &CheckpointRef,
    fork_dir: &Path,
) -> (Session, Id128) {
    let receipt = source.fork(checkpoint, fork_options(fork_dir)).unwrap();
    let fork_id = receipt.session_id;
    let mut fork = receipt.session;
    fork.commit(vec![event("fork_work", json!({"n": 1}))], None)
        .unwrap();
    (fork, fork_id)
}

// --- adopt tests -----------------------------------------------------------

#[test]
fn adopt_happy_path_commits_fact_and_pins_roots() {
    let dir = TempDir::new("happy");
    let source_id = Id128::generate();
    let lifetime_root = seed_claim(
        &dir.path().join("memory"),
        MemoryScope::Lifetime,
        source_id,
        "adopt seed",
    );
    let mut source = open_source(dir.path(), source_id);
    let cp = source.create_checkpoint(Some("adopt point".into())).unwrap();
    let (mut fork, fork_id) = fork_with_outcome(&source, &cp, &dir.path().join("fork"));
    let fork_head = fork.current_snapshot().unwrap();
    let fork_seq = fork.next_seq() - 1;
    let before = envelopes(&dir.path().join("log.zst")).len();

    let adopted = source.adopt(&mut fork, Some("merge fork".into())).unwrap();

    // exactly one new event on the source log (no active run → no quiesce)
    let after = envelopes(&dir.path().join("log.zst"));
    assert_eq!(after.len(), before + 1, "only fork_adopted committed");
    let fa = after.last().unwrap();
    assert_eq!(fa.kind, "fork_adopted");
    assert_eq!(fa.payload_schema, 1);
    assert_eq!(fa.payload["fork_session"], fork_id.to_string());
    assert_eq!(fa.payload["fork_seq"], fork_seq);
    assert_eq!(fa.payload["fork_snapshot"], fork_head.to_string());
    assert_eq!(fa.payload["frontier_seq"], cp.seq);
    assert_eq!(fa.payload["label"], "merge fork");
    assert_eq!(
        fa.payload["follow"]["PinnedAt"]["lifetime_root"],
        lifetime_root.to_string()
    );
    assert_eq!(fa.payload["follow"]["PinnedAt"]["project_root"], Value::Null);
    // refs = [fork snapshot, fork memory roots]
    assert_eq!(fa.refs, vec![fork_head, lifetime_root]);
    // the quiesce record mirrors branch_transition's shape, empty here
    assert_eq!(fa.payload["quiesce"]["cancelled"], json!([]));
    assert_eq!(fa.payload["quiesce"]["ambiguous"], json!([]));

    // the receipt mirrors the fact
    assert_eq!(adopted.fork_session, fork_id);
    assert_eq!(adopted.fork_seq, fork_seq);
    assert_eq!(
        adopted.follow,
        MemoryFollowPolicy::PinnedAt {
            lifetime_root,
            project_root: None,
        }
    );
    // the source's projection pins the fork's roots (continue_from semantics)
    assert_eq!(
        source.pinned_roots(),
        Some(&PinnedRoots {
            lifetime: lifetime_root,
            project: None,
        })
    );

    // the fork session is untouched: no new events, still writable
    assert_eq!(envelopes(fork.log_path()).len(), 2, "fork log unchanged");
    fork.commit(vec![event("after_adopt", json!({"n": 1}))], None)
        .unwrap();
    assert_eq!(envelopes(fork.log_path()).len(), 3);

    fork.close().unwrap();
    source.close().unwrap();
}

#[test]
fn adopt_quiesces_active_source_run() {
    let dir = TempDir::new("quiesce");
    let source_id = Id128::generate();
    let mut source = open_source(dir.path(), source_id);
    let cp = source.create_checkpoint(None).unwrap();
    let (mut fork, _fork_id) = fork_with_outcome(&source, &cp, &dir.path().join("fork"));

    // an active run on the source — adopt must quiesce it (continue_from
    // semantics, unlike fork which never touches the source)
    source.observe_trigger(kanbei_scheduler::Trigger {
        kind: kanbei_scheduler::TriggerKind::NewCausalEvent,
        referent: None,
    });
    let run = source.accept_wake().unwrap().expect("wake accepted");
    source.run_start(run.run_id).unwrap();
    let before = envelopes(&dir.path().join("log.zst")).len();

    source.adopt(&mut fork, None).unwrap();

    let after = envelopes(&dir.path().join("log.zst"));
    assert_eq!(after.len(), before + 2, "run_outcome + fork_adopted");
    let outcome = &after[before];
    assert_eq!(outcome.kind, "run_outcome");
    assert_eq!(outcome.payload["outcome"]["Failed"], json!("Quiesced"));
    assert_eq!(outcome.seq, after[before + 1].seq - 1, "quiesce before adoption");
    let fa = after.last().unwrap();
    assert_eq!(fa.kind, "fork_adopted");
    assert_eq!(fa.payload["quiesce"]["cancelled"], json!([]));
    assert_eq!(fa.payload["quiesce"]["ambiguous"], json!([]));
    assert!(
        after.iter().all(|e| e.kind != "intent_classified"),
        "adopt commits no classification facts (mirror of continue_from)"
    );

    fork.close().unwrap();
    source.close().unwrap();
}

#[test]
fn adopt_rejects_fork_from_another_source() {
    let dir = TempDir::new("wrong-source");
    let a_id = Id128::generate();
    let b_id = Id128::generate();
    seed_claim(
        &dir.path().join("a/memory"),
        MemoryScope::Lifetime,
        a_id,
        "a seed",
    );
    let mut a = open_source(&dir.path().join("a"), a_id);
    let cp = a.create_checkpoint(None).unwrap();
    let (mut fork, fork_id) = fork_with_outcome(&a, &cp, &dir.path().join("fork"));
    let mut b = open_source(&dir.path().join("b"), b_id);
    let before = envelopes(&dir.path().join("b/log.zst")).len();

    let err = b
        .adopt(&mut fork, None)
        .err()
        .expect("a fork of another source is rejected");
    assert!(
        matches!(err, SessionError::InvalidInput(_)),
        "got {err:?}"
    );
    assert_eq!(
        envelopes(&dir.path().join("b/log.zst")).len(),
        before,
        "no fork_adopted committed on the wrong source"
    );
    assert_eq!(fork_id, fork.session_id());

    fork.close().unwrap();
    a.close().unwrap();
    b.close().unwrap();
}

#[test]
fn adopt_rejects_fork_without_outcome() {
    let dir = TempDir::new("no-outcome");
    let source_id = Id128::generate();
    seed_claim(
        &dir.path().join("memory"),
        MemoryScope::Lifetime,
        source_id,
        "seed",
    );
    let mut source = open_source(dir.path(), source_id);
    let cp = source.create_checkpoint(None).unwrap();
    let mut receipt = source.fork(&cp, fork_options(&dir.path().join("fork"))).unwrap();
    assert_eq!(receipt.session.next_seq(), 2, "fork holds only the forked fact");
    let before = envelopes(&dir.path().join("log.zst")).len();

    let err = source
        .adopt(&mut receipt.session, None)
        .err()
        .expect("an outcome-less fork is rejected");
    assert!(matches!(err, SessionError::InvalidInput(_)), "got {err:?}");
    assert_eq!(
        envelopes(&dir.path().join("log.zst")).len(),
        before,
        "no fork_adopted committed"
    );

    receipt.session.close().unwrap();
    source.close().unwrap();
}

#[test]
fn adopt_missing_snapshot_object_fails_cleanly() {
    let dir = TempDir::new("missing-snapshot");
    let source_id = Id128::generate();
    seed_claim(
        &dir.path().join("memory"),
        MemoryScope::Lifetime,
        source_id,
        "seed",
    );
    let mut source = open_source(dir.path(), source_id);
    let cp = source.create_checkpoint(None).unwrap();
    let fork_dir = dir.path().join("fork");
    let (mut fork, _fork_id) = fork_with_outcome(&source, &cp, &fork_dir);

    // remove the fork's head snapshot manifest object from the fork's store
    let head = fork.current_snapshot().unwrap();
    std::fs::remove_file(fork_dir.join("objects").join(head.to_string())).unwrap();
    let before = envelopes(&dir.path().join("log.zst")).len();

    let err = source
        .adopt(&mut fork, None)
        .err()
        .expect("an unreadable fork head snapshot is rejected");
    assert!(matches!(err, SessionError::Snapshot(_)), "got {err:?}");
    assert_eq!(
        envelopes(&dir.path().join("log.zst")).len(),
        before,
        "no fork_adopted committed after a failed adoption"
    );

    fork.close().unwrap();
    source.close().unwrap();
}

#[test]
fn adopt_follow_head_when_fork_pins_no_roots() {
    let dir = TempDir::new("followhead");
    let mut source = open_source(dir.path(), Id128::generate());
    let cp = source.create_checkpoint(None).unwrap();
    let (mut fork, fork_id) = fork_with_outcome(&source, &cp, &dir.path().join("fork"));
    let fork_head = fork.current_snapshot().unwrap();
    assert_eq!(fork.memory_lifetime().head(), None);
    let before = envelopes(&dir.path().join("log.zst")).len();

    let adopted = source.adopt(&mut fork, None).unwrap();

    assert_eq!(adopted.follow, MemoryFollowPolicy::FollowHead);
    assert_eq!(source.pinned_roots(), None, "FollowHead releases the pins");
    let after = envelopes(&dir.path().join("log.zst"));
    assert_eq!(after.len(), before + 1);
    let fa = after.last().unwrap();
    assert_eq!(fa.kind, "fork_adopted");
    assert_eq!(fa.payload["fork_session"], fork_id.to_string());
    assert_eq!(fa.payload["follow"], json!("FollowHead"));
    assert_eq!(fa.refs, vec![fork_head], "no memory roots to ref");

    fork.close().unwrap();
    source.close().unwrap();
}

// --- import tests ----------------------------------------------------------

#[test]
fn import_round_trip_preserves_ids() {
    let dir = TempDir::new("import");
    let source_id = Id128::generate();
    let lifetime_root = seed_claim(
        &dir.path().join("memory"),
        MemoryScope::Lifetime,
        source_id,
        "import seed",
    );
    let mut source = open_source(dir.path(), source_id);
    source
        .commit(vec![event("plain_commit", json!({"n": 1}))], None)
        .unwrap();
    let cp = source.create_checkpoint(Some("import point".into())).unwrap();
    let record = source.continue_from(&cp).unwrap();
    let src_envs = envelopes(&dir.path().join("log.zst"));
    let src_branches = source.branch_records().to_vec();
    let src_head = source.memory_lifetime().head();
    source.close().unwrap();

    let target = dir.path().join("imported");
    let mut imported = Session::import(dir.path(), &target).unwrap();

    // IDs preserved by construction: the session id is recovered from the
    // memory identity markers, and the canonical facts are the copied bytes
    assert_eq!(imported.session_id(), source_id, "session id preserved");
    let imp_envs = envelopes(&target.join("log.zst"));
    assert_eq!(
        imp_envs, src_envs,
        "identical envelopes: event ids, seqs, payloads, refs, snapshots"
    );
    assert_eq!(imported.branch_records(), &src_branches);
    assert_eq!(imported.branch(), record.id);
    assert_eq!(imported.memory_lifetime().head(), src_head);
    assert_eq!(imported.memory_lifetime().head(), Some(lifetime_root));

    // the imported session is fully writable
    imported
        .commit(vec![event("post_import", json!({"n": 1}))], None)
        .unwrap();
    assert_eq!(
        envelopes(&target.join("log.zst")).len(),
        src_envs.len() + 1
    );
    imported.close().unwrap();

    // crash-safety smoke: the imported dir reopens cleanly, twice
    let r1 = Session::open(SessionConfig {
        dir: target.clone(),
        session_id: Some(source_id),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(r1.branch_records(), &src_branches);
    r1.close().unwrap();
    let r2 = Session::open(SessionConfig {
        dir: target.clone(),
        session_id: Some(source_id),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(r2.session_id(), source_id);
    assert_eq!(r2.next_seq(), src_envs.len() as u64 + 2);
    r2.close().unwrap();
}

#[test]
fn import_rejects_missing_log_and_nonempty_target() {
    let dir = TempDir::new("import-reject");
    let source_id = Id128::generate();

    // a source with no log.zst is invalid input
    let empty = dir.path().join("empty");
    std::fs::create_dir_all(&empty).unwrap();
    let err = Session::import(&empty, &dir.path().join("t1"))
        .err()
        .expect("a log-less source is rejected");
    assert!(matches!(err, SessionError::InvalidInput(_)), "got {err:?}");

    // a real source for the target checks (seeded so the id is recoverable)
    seed_claim(
        &dir.path().join("src/memory"),
        MemoryScope::Lifetime,
        source_id,
        "reject seed",
    );
    let mut source = open_source(&dir.path().join("src"), source_id);
    source
        .commit(vec![event("plain_commit", json!({"n": 1}))], None)
        .unwrap();
    source.close().unwrap();

    // a non-empty target is invalid input (refusing to merge)
    let target = dir.path().join("t2");
    std::fs::create_dir_all(&target).unwrap();
    std::fs::write(target.join("junk"), "junk").unwrap();
    let err = Session::import(&dir.path().join("src"), &target)
        .err()
        .expect("a non-empty target is rejected");
    assert!(matches!(err, SessionError::InvalidInput(_)), "got {err:?}");
    assert!(target.join("junk").exists(), "the target is untouched");

    // a missing target is fine
    let ok = Session::import(&dir.path().join("src"), &dir.path().join("t3")).unwrap();
    assert_eq!(ok.session_id(), source_id);
    ok.close().unwrap();
}
