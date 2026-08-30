//! Integration tests for kanbei-session M9 wave 5a: the independent-session
//! fork — a NEW session (fresh SessionId, explicit source reference,
//! fork-floor grants) created from a committed checkpoint's snapshot closure.
//! Covers the canonical `forked` fact, memory-root preservation (seeded log
//! replay, post-checkpoint transitions truncated out), the read-only fork
//! floor, config activation, source non-interference (no quiesce, no events),
//! validation errors, determinism, and workspace restore. Guest-wasm tests
//! skip when the guest is not built (see m2.rs).
//!
//! Run `cargo build -p kanbei-guest --target wasm32-wasip1 --release` from
//! the workspace root first; the module-dependent test prints `skip:` and
//! passes without it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use kanbei_capabilities::{Capability, Principal};
use kanbei_core::digest::Digest;
use kanbei_core::envelope::Envelope;
use kanbei_core::id::Id128;
use kanbei_core::queue::DurabilityQueue;
use kanbei_log::for_each_frame;
use kanbei_memory::{
    Claim, ClaimProvenance, IdempotencyKey, MEMORY_CLAIM_SCHEMA, MEMORY_ROOT_SCHEMA,
    MEMORY_TRANSITION_SCHEMA, MemoryFollowPolicy, MemoryRootActor, MemoryScope, MemoryTransition,
    ProjectEntry, ProjectRegistry, PROJECT_ENTRY_SCHEMA, RootManifest, TransitionKind,
    TransitionOutcome,
};
use kanbei_modules::{ModuleOrigin, PackageManifest};
use kanbei_objects::ObjectStore;
use kanbei_policy::builtins::StoreAllPolicy;
use kanbei_services::ScopePath;
use kanbei_session::{CheckpointRef, ForkOptions, NewEvent, Session, SessionConfig, SessionError};
use kanbei_vm::{GuestError, Vm, VmConfig};
use kanbei_workspace::SnapshotOptions;
use serde_json::{Value, json};

// --- helpers ---------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kb-session-fork-{tag}-{}-{}",
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

/// Module tests need the guest wasm; without it they skip with a note.
fn require_guest() -> bool {
    match Vm::load(no_epoch()) {
        Ok(_) => true,
        Err(GuestError::NotBuilt) => {
            eprintln!(
                "skip: guest wasm not built (run `cargo build -p kanbei-guest \
                 --target wasm32-wasip1 --release`)"
            );
            false
        }
        Err(e) => panic!("Vm::load failed: {e}"),
    }
}

/// Non-fuel, non-epoch-bounded engine config for tests whose modules must not
/// trap.
fn no_epoch() -> VmConfig {
    VmConfig {
        fuel_per_call: u64::MAX,
        epoch_deadline: u64::MAX,
        ..Default::default()
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
    let queue = Arc::new(DurabilityQueue::start("kb-fork-seed"));
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
        decision_digest: Digest::new(format!("fork-seed-decision-{text}").as_bytes()),
        idempotency_key: IdempotencyKey {
            session: session_id,
            event: 1,
            decision: Digest::new(format!("fork-seed-decision-{text}").as_bytes()),
        },
    };
    match actor.propose(transition, &[d], &[]).unwrap() {
        TransitionOutcome::Committed { .. } => {}
        other => panic!("seed propose: expected Committed, got {other:?}"),
    }
    actor.flush().unwrap();
    manifest.digest()
}

/// Registers `project_id` in the registry and seeds one project-scope claim
/// root. Returns the root digest.
fn seed_project_claim(
    memory_root: &Path,
    session_id: Id128,
    project_id: Id128,
    text: &str,
) -> Digest {
    let mut registry = ProjectRegistry::open(&memory_root.join("projects.jsonl")).unwrap();
    registry
        .register(ProjectEntry {
            schema: PROJECT_ENTRY_SCHEMA,
            project_id,
            name: "default".into(),
            dir: MemoryScope::Project(project_id).dir_name(),
            created_session: session_id,
            created_event: 1,
        })
        .unwrap();
    seed_claim(
        memory_root,
        MemoryScope::Project(project_id),
        session_id,
        text,
    )
}

// --- tests -----------------------------------------------------------------

#[test]
fn fork_creates_independent_session_with_canonical_fact() {
    let dir = TempDir::new("fact");
    let source_id = Id128::generate();
    let lifetime_root = seed_claim(
        &dir.path().join("memory"),
        MemoryScope::Lifetime,
        source_id,
        "fork seed",
    );
    let mut source = open_source(dir.path(), source_id);
    let cp = source.create_checkpoint(Some("fork point".into())).unwrap();

    let fork_dir = dir.path().join("fork");
    let receipt = source.fork(&cp, fork_options(&fork_dir)).unwrap();

    // a NEW identity, never the source's
    assert_ne!(receipt.session_id, source_id);
    assert_eq!(receipt.session_id, receipt.session.session_id());
    assert_eq!(receipt.checkpoint_seq, cp.seq);
    assert_eq!(receipt.branch, receipt.session.branch());
    assert_eq!(
        receipt.follow,
        MemoryFollowPolicy::PinnedAt {
            lifetime_root,
            project_root: None,
        }
    );

    // the fork log holds exactly the canonical forked fact
    let envs = envelopes(&fork_dir.join("log.zst"));
    assert_eq!(envs.len(), 1, "fork log holds exactly the forked fact");
    let fork_env = &envs[0];
    assert_eq!(fork_env.kind, "forked");
    assert_eq!(fork_env.payload_schema, 1);
    assert_eq!(fork_env.seq, 1);
    // the explicit source reference + frontier
    assert_eq!(fork_env.payload["source_session"], source_id.to_string());
    assert_eq!(fork_env.payload["checkpoint_seq"], cp.seq);
    assert_eq!(fork_env.payload["frontier_seq"], cp.seq);
    // refs = [checkpoint snapshot, memory roots, config if any]
    let snap: Digest = fork_env.payload["checkpoint_snapshot"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(fork_env.refs, vec![snap, lifetime_root]);
    assert_eq!(fork_env.payload["config"], Value::Null);
    // the follow policy is recorded externally tagged
    assert_eq!(
        fork_env.payload["follow"]["PinnedAt"]["lifetime_root"],
        lifetime_root.to_string()
    );
    assert_eq!(
        fork_env.payload["follow"]["PinnedAt"]["project_root"],
        Value::Null
    );
    // the attenuated fork-floor grants are the canonical fact's grant record
    let grants: Vec<String> = fork_env.payload["grants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| g.as_str().unwrap().to_string())
        .collect();
    assert_eq!(grants.len(), 6, "read-only floor + memory.propose grants");
    let broker_grants: Vec<String> = receipt
        .session
        .broker()
        .grants
        .iter()
        .map(|g| g.grant_digest.to_string())
        .collect();
    assert_eq!(broker_grants, grants);

    receipt.session.close().unwrap();
    source.close().unwrap();
}

#[test]
fn fork_preserves_checkpoint_memory_roots() {
    let dir = TempDir::new("memory");
    let source_id = Id128::generate();
    let project_id = Id128::generate();
    let memory_root = dir.path().join("memory");
    let lifetime_root = seed_claim(&memory_root, MemoryScope::Lifetime, source_id, "lifetime");
    let project_root = seed_project_claim(&memory_root, source_id, project_id, "project");
    let mut source = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        memory_root: Some(memory_root.clone()),
        session_id: Some(source_id),
        project: Some(project_id),
        ..Default::default()
    })
    .unwrap();
    let cp = source.create_checkpoint(None).unwrap();
    source.close().unwrap();

    // Advance the source memory past the checkpoint: a later claim makes the
    // source's head newer than the pinned root — the fork must NOT inherit it.
    let newer = seed_claim(&memory_root, MemoryScope::Lifetime, source_id, "post-checkpoint");
    assert_ne!(newer, lifetime_root);
    let source = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        memory_root: Some(memory_root.clone()),
        session_id: Some(source_id),
        project: Some(project_id),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(source.memory_lifetime().head(), Some(newer));

    let fork_dir = dir.path().join("fork");
    let receipt = source.fork(&cp, fork_options(&fork_dir)).unwrap();

    // the forked actors replay the seeded (truncated) logs: head == the
    // checkpoint roots, and the post-checkpoint root never leaks in
    assert_eq!(
        receipt.session.memory_lifetime().head(),
        Some(lifetime_root)
    );
    assert!(receipt
        .session
        .memory_lifetime()
        .contains_root(&lifetime_root));
    assert!(!receipt
        .session
        .memory_lifetime()
        .contains_root(&newer));
    let project_actor = receipt
        .session
        .memory_project()
        .expect("the project binding is carried over");
    assert_eq!(project_actor.head(), Some(project_root));
    assert!(project_actor.contains_root(&project_root));
    assert_eq!(
        receipt.session.project_entry().unwrap().project_id,
        project_id
    );
    assert_eq!(
        receipt.follow,
        MemoryFollowPolicy::PinnedAt {
            lifetime_root,
            project_root: Some(project_root),
        }
    );
    let envs = envelopes(&fork_dir.join("log.zst"));
    let fork_env = envs
        .iter()
        .find(|e| e.kind == "forked")
        .expect("forked fact on the fork log");
    // the project binding commits first (open's canonical project_bound), the
    // forked fact is the fork's genesis record after it
    assert_eq!(envs[0].kind, "project_bound");
    assert_eq!(fork_env.seq, 2);
    assert_eq!(
        fork_env.payload["follow"]["PinnedAt"]["lifetime_root"],
        lifetime_root.to_string()
    );
    assert_eq!(
        fork_env.payload["follow"]["PinnedAt"]["project_root"],
        project_root.to_string()
    );
    assert_eq!(fork_env.refs.len(), 3, "snapshot + lifetime + project roots");

    receipt.session.close().unwrap();
    source.close().unwrap();
}

#[test]
fn fork_broker_is_read_only_floor() {
    let dir = TempDir::new("broker");
    let source_id = Id128::generate();
    seed_claim(
        &dir.path().join("memory"),
        MemoryScope::Lifetime,
        source_id,
        "broker seed",
    );
    let mut source = open_source(dir.path(), source_id);
    let cp = source.create_checkpoint(None).unwrap();

    let fork_dir = dir.path().join("fork");
    let receipt = source.fork(&cp, fork_options(&fork_dir)).unwrap();
    let broker = receipt.session.broker();
    assert_eq!(broker.policy_version(), 1);
    let principal = Principal {
        session: receipt.session_id,
        generation: 0,
        run: None,
    };

    // the fork floor denies every write capability (no grant — default deny)
    for resource in ["state.write", "fs.write", "process.run", "tool.*", "service.call"] {
        let err = broker.check(
            &principal,
            &Capability::new(resource.into(), vec!["call".into()]),
            1,
        );
        assert!(err.is_err(), "fork floor must deny {resource}, got {err:?}");
    }
    // read-only grants resolve without the approval path
    let query = broker
        .check(
            &principal,
            &Capability::new("memory.query".into(), vec!["call".into()]),
            1,
        )
        .unwrap();
    assert!(!query.requires_approval);
    let read = broker
        .check(
            &principal,
            &Capability::new("fs.read".into(), vec!["call".into()]),
            1,
        )
        .unwrap();
    assert!(!read.requires_approval);
    // memory.propose is granted through the approval path only
    let propose = broker
        .check(
            &principal,
            &Capability::new("memory.propose".into(), vec!["call".into()]),
            1,
        )
        .unwrap();
    assert!(propose.requires_approval);

    receipt.session.close().unwrap();
    source.close().unwrap();
}

#[test]
fn fork_with_config_activates_same_digest() {
    if !require_guest() {
        return;
    }
    let dir = TempDir::new("config");
    let source_id = Id128::generate();
    let config = PackageManifest {
        schema: 1,
        module_id: Id128::generate(),
        origin: ModuleOrigin::UserConfig,
        trust_class: kanbei_capabilities::TrustClass::User,
        scope: ScopePath(vec![]),
        deps: vec![],
        capabilities: vec![],
        source: "function kb_on_activate(ctx) ctx.service_publish('{\"scope\":[],\"name\":\"fork-greeter\"}', 1, '[]') end\nfunction kb_hot(x) return x end".into(),
        state_schema: None,
    };
    // the package digest is the canonical content digest (install_package)
    let config_digest = Digest::new(&serde_json::to_vec(&config).unwrap());
    let mut source = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        memory_root: Some(dir.path().join("memory")),
        session_id: Some(source_id),
        config: Some(config),
        engine: Some(no_epoch()),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(source.config_digest(), Some(config_digest));
    let cp = source.create_checkpoint(None).unwrap();

    let fork_dir = dir.path().join("fork");
    let mut opts = fork_options(&fork_dir);
    opts.config.engine = Some(no_epoch());
    let receipt = source.fork(&cp, opts).unwrap();

    // the fork activated the SAME config package digest
    assert_eq!(receipt.session.config_digest(), Some(config_digest));
    let envs = envelopes(&fork_dir.join("log.zst"));
    // seq 1 = the fork's own config activation, seq 2 = the canonical fact
    assert_eq!(envs[0].kind, "composition_changed");
    assert_eq!(envs[1].kind, "forked");
    assert_eq!(envs[1].payload["config"], config_digest.to_string());
    assert!(
        envs[1].refs.contains(&config_digest),
        "the config package is a forked-fact ref"
    );
    assert_eq!(
        envs[1].refs[0],
        envs[1].payload["checkpoint_snapshot"]
            .as_str()
            .unwrap()
            .parse::<Digest>()
            .unwrap()
    );

    receipt.session.close().unwrap();
    source.close().unwrap();
}

#[test]
fn fork_leaves_source_untouched() {
    let dir = TempDir::new("untouched");
    let source_id = Id128::generate();
    seed_claim(
        &dir.path().join("memory"),
        MemoryScope::Lifetime,
        source_id,
        "untouched seed",
    );
    let mut source = open_source(dir.path(), source_id);
    let cp = source.create_checkpoint(None).unwrap();
    // an active run: fork must NOT quiesce it (continue_from would)
    source.observe_trigger(kanbei_scheduler::Trigger {
        kind: kanbei_scheduler::TriggerKind::NewCausalEvent,
        referent: None,
    });
    let run = source.accept_wake().unwrap().expect("wake accepted");
    source.run_start(run.run_id).unwrap();
    let before = envelopes(&dir.path().join("log.zst"));

    let fork_dir = dir.path().join("fork");
    let receipt = source.fork(&cp, fork_options(&fork_dir)).unwrap();

    // no new events on the source log, and no quiesce of the active run
    let after = envelopes(&dir.path().join("log.zst"));
    assert_eq!(before.len(), after.len(), "fork must not append to the source log");
    assert!(
        after.iter().all(|e| e.kind != "run_outcome"),
        "fork must not quiesce the active run"
    );
    assert_eq!(after.last().unwrap().kind, "run_start");

    // the source run still completes normally afterwards
    source
        .run_outcome(
            run.run_id,
            kanbei_scheduler::TerminalOutcome::CompletedGoal,
            source.scheduler_usage(run.run_id),
            &[],
        )
        .unwrap();
    assert_eq!(
        envelopes(&dir.path().join("log.zst")).last().unwrap().kind,
        "run_outcome"
    );
    receipt.session.close().unwrap();
    source.close().unwrap();
}

#[test]
fn fork_rejects_invalid_checkpoints() {
    let dir = TempDir::new("reject");
    let source_id = Id128::generate();
    seed_claim(
        &dir.path().join("memory"),
        MemoryScope::Lifetime,
        source_id,
        "reject seed",
    );
    let mut source = open_source(dir.path(), source_id);
    let cp = source.create_checkpoint(None).unwrap();
    let before = envelopes(&dir.path().join("log.zst")).len();
    let fork_dir = dir.path().join("fork");

    // seq 0 and an uncommitted seq are not committed events
    let err = source
        .fork(
            &CheckpointRef {
                session_id: source_id,
                seq: 0,
            },
            fork_options(&fork_dir),
        )
        .err()
        .expect("seq 0 is not a committed event");
    assert!(matches!(err, SessionError::InvalidInput(_)));
    let err = source
        .fork(
            &CheckpointRef {
                session_id: source_id,
                seq: source.next_seq(),
            },
            fork_options(&fork_dir),
        )
        .err()
        .expect("an uncommitted seq is rejected");
    assert!(matches!(err, SessionError::InvalidInput(_)));

    // a checkpoint from another session is rejected
    let err = source
        .fork(
            &CheckpointRef {
                session_id: Id128::generate(),
                seq: cp.seq,
            },
            fork_options(&fork_dir),
        )
        .err()
        .expect("a cross-session checkpoint is rejected");
    assert!(matches!(err, SessionError::InvalidInput(_)));

    // a committed non-checkpoint event is rejected
    source
        .commit(vec![event("plain_commit", json!({"n": 1}))], None)
        .unwrap();
    let err = source
        .fork(
            &CheckpointRef {
                session_id: source_id,
                seq: source.next_seq() - 1,
            },
            fork_options(&fork_dir),
        )
        .err()
        .expect("a non-checkpoint event is rejected");
    assert!(matches!(err, SessionError::InvalidInput(_)));

    // no side effects: the source log grew only by the plain commit, and no
    // failed fork left a target dir behind
    assert_eq!(
        envelopes(&dir.path().join("log.zst")).len(),
        before + 1,
        "failed forks must not append to the source log"
    );
    assert!(!fork_dir.exists(), "failed forks must not leave an orphan dir");

    source.close().unwrap();
}

#[test]
fn fork_twice_yields_independent_sessions() {
    let dir = TempDir::new("twice");
    let source_id = Id128::generate();
    let lifetime_root = seed_claim(
        &dir.path().join("memory"),
        MemoryScope::Lifetime,
        source_id,
        "twice seed",
    );
    let mut source = open_source(dir.path(), source_id);
    let cp = source.create_checkpoint(None).unwrap();

    let mut r1 = source
        .fork(&cp, fork_options(&dir.path().join("fork-a")))
        .unwrap();
    let mut r2 = source
        .fork(&cp, fork_options(&dir.path().join("fork-b")))
        .unwrap();
    assert_ne!(r1.session_id, r2.session_id);
    assert_ne!(r1.branch, r2.branch);
    for receipt in [&r1, &r2] {
        let envs = envelopes(receipt.session.log_path());
        assert_eq!(envs.len(), 1);
        assert_eq!(envs[0].kind, "forked");
        assert_eq!(envs[0].payload["source_session"], source_id.to_string());
        assert_eq!(envs[0].payload["checkpoint_seq"], cp.seq);
        assert_eq!(
            receipt.session.memory_lifetime().head(),
            Some(lifetime_root)
        );
    }
    // both forks are independently writable
    r1.session
        .commit(vec![event("fork_a_event", json!({"n": 1}))], None)
        .unwrap();
    r2.session
        .commit(vec![event("fork_b_event", json!({"n": 1}))], None)
        .unwrap();
    let a = envelopes(&dir.path().join("fork-a/log.zst"));
    let b = envelopes(&dir.path().join("fork-b/log.zst"));
    assert_eq!(a.len(), 2);
    assert_eq!(b.len(), 2);
    assert_eq!(a[1].kind, "fork_a_event");
    assert_eq!(b[1].kind, "fork_b_event");

    r1.session.close().unwrap();
    r2.session.close().unwrap();
    source.close().unwrap();
}

#[test]
fn fork_restores_workspace_snapshot() {
    let dir = TempDir::new("workspace");
    let fs_root = dir.path().join("tree");
    std::fs::create_dir_all(fs_root.join("sub")).unwrap();
    std::fs::write(fs_root.join("a.txt"), "hello").unwrap();
    std::fs::write(fs_root.join("sub/b.txt"), "world").unwrap();
    let mut source = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        memory_root: Some(dir.path().join("memory")),
        fs_root: fs_root.clone(),
        ..Default::default()
    })
    .unwrap();
    let manifest = source.snapshot_workspace(SnapshotOptions::default()).unwrap();
    let cp = source.create_checkpoint(None).unwrap();

    let fork_dir = dir.path().join("fork");
    let mut opts = fork_options(&fork_dir);
    opts.config.fs_root = fs_root.clone();
    let mut receipt = source.fork(&cp, opts).unwrap();

    // the workspace snapshot objects ride along with the checkpoint closure
    assert!(receipt.session.store().exists(&manifest));
    let fork_env = envelopes(&fork_dir.join("log.zst"));
    assert!(
        fork_env[0].refs.contains(&manifest),
        "the workspace snapshot manifest is a forked-fact ref (GC-rooted)"
    );

    // mutate the tree, then restore it from the forked session
    std::fs::write(fs_root.join("a.txt"), "mutated").unwrap();
    std::fs::remove_file(fs_root.join("sub/b.txt")).unwrap();
    let report = receipt.session.restore_workspace(&manifest).unwrap();
    assert_eq!(report.entries_restored, 2);
    assert_eq!(report.bytes, 10);
    assert_eq!(
        std::fs::read_to_string(fs_root.join("a.txt")).unwrap(),
        "hello"
    );
    assert_eq!(
        std::fs::read_to_string(fs_root.join("sub/b.txt")).unwrap(),
        "world"
    );
    // the restore committed a canonical event on the fork log
    let envs = envelopes(&fork_dir.join("log.zst"));
    assert_eq!(envs[0].kind, "forked");
    assert_eq!(envs[1].kind, "workspace_restore");
    assert_eq!(envs[1].refs, vec![manifest]);

    receipt.session.close().unwrap();
    source.close().unwrap();
}

#[test]
fn fork_without_memory_records_follow_head() {
    let dir = TempDir::new("followhead");
    let source_id = Id128::generate();
    let mut source = open_source(dir.path(), source_id);
    let cp = source.create_checkpoint(None).unwrap();

    let fork_dir = dir.path().join("fork");
    let receipt = source.fork(&cp, fork_options(&fork_dir)).unwrap();

    assert_eq!(receipt.follow, MemoryFollowPolicy::FollowHead);
    assert_eq!(receipt.session.memory_lifetime().head(), None);
    let envs = envelopes(&fork_dir.join("log.zst"));
    assert_eq!(envs[0].payload["follow"], json!("FollowHead"));
    assert_eq!(envs[0].refs.len(), 1, "only the checkpoint snapshot");

    receipt.session.close().unwrap();
    source.close().unwrap();
}
