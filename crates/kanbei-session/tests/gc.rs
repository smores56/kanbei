//! Integration tests for kanbei-session M8 wave 2: automatic canonical-object
//! GC — root capture over the session log + live roots, writer pins, the
//! quarantine + grace sweep, the canonical `gc.run` record, the open-time
//! automatic pass (crash safety across reopen), memory-store GC, and
//! post-GC export honesty. Guest-wasm tests skip when the guest is not built
//! (see m2.rs).

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use kanbei_core::digest::Digest;
use kanbei_core::envelope::Envelope;
use kanbei_core::queue::DurabilityQueue;
use kanbei_gc::GcConfig;
use kanbei_log::for_each_frame;
use kanbei_memory::{
    Claim, ClaimProvenance, IdempotencyKey, MEMORY_CLAIM_SCHEMA, MEMORY_ROOT_SCHEMA,
    MEMORY_TRANSITION_SCHEMA, MemoryRootActor, MemoryScope, MemoryTransition, RootManifest,
    TransitionKind, TransitionOutcome,
};
use kanbei_modules::{ModuleOrigin, PackageManifest};
use kanbei_objects::ObjectStore;
use kanbei_session::{NewEvent, Session, SessionConfig};
use kanbei_vm::{GuestError, Vm, VmConfig};
use serde_json::json;

// --- helpers ---------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kb-session-gc-{tag}-{}-{}",
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

fn open_session(dir: &Path) -> Session {
    Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        ..Default::default()
    })
    .unwrap()
}

fn sweep_cfg() -> GcConfig {
    GcConfig {
        grace: Duration::ZERO,
        sweep: true,
    }
}

/// A second ObjectStore handle over the session store dir (the module
/// manager's handle pattern) — installs bytes the session never committed.
fn stray_store(dir: &Path) -> (ObjectStore, Arc<DurabilityQueue>) {
    let queue = Arc::new(DurabilityQueue::start("kb-session-gc-stray"));
    let store = ObjectStore::open(&dir.join("objects"), Arc::clone(&queue)).unwrap();
    (store, queue)
}

fn gc_run_envelope(dir: &Path) -> Envelope {
    let log_path = dir.join("log.zst");
    let mut found: Option<Envelope> = None;
    for_each_frame(&log_path, |info| {
        for line in &info.events {
            let Ok(env) = Envelope::from_line(line) else {
                continue;
            };
            if env.kind == "gc.run" {
                found = Some(env);
            }
        }
    })
    .unwrap();
    found.expect("a gc.run envelope is in the log")
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

fn no_epoch() -> VmConfig {
    VmConfig {
        fuel_per_call: u64::MAX,
        epoch_deadline: u64::MAX,
        ..Default::default()
    }
}

// --- session-store GC -------------------------------------------------------

/// Workspace snapshots must survive GC: the blobs behind a snapshot's
/// manifest are referenced only inside the manifest object, so the
/// collector has to walk that closure — anything else quarantines every
/// workspace blob on the first run and sweeps them after grace.
#[test]
fn workspace_snapshot_blobs_survive_gc() {
    let tmp = TempDir::new("gc-workspace");
    let dir = tmp.path().to_path_buf();
    let ws = dir.join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    std::fs::write(ws.join("a.txt"), b"alpha").unwrap();
    std::fs::write(ws.join("b.txt"), b"beta").unwrap();
    let mut session = Session::open(SessionConfig {
        dir: dir.clone(),
        fs_root: ws.clone(),
        ..Default::default()
    })
    .unwrap();
    let manifest = session
        .snapshot_workspace(kanbei_workspace::SnapshotOptions::default())
        .unwrap();
    let parsed: kanbei_workspace::Manifest = serde_json::from_slice(
        &session.store().get(&manifest).unwrap(),
    )
    .unwrap();
    let blobs: Vec<Digest> = parsed
        .entries
        .iter()
        .filter_map(|e| match e {
            kanbei_workspace::Entry::File { digest, .. } => Some(*digest),
            _ => None,
        })
        .collect();
    assert!(blobs.len() >= 2, "the snapshot grounded its file blobs");

    // a sweep with zero grace deletes anything the collector missed
    let report = session.run_gc(sweep_cfg()).unwrap();
    for digest in &blobs {
        assert!(
            session.store().exists(digest),
            "workspace blob {digest} was quarantined by GC (swept: {})",
            report.swept
        );
    }
    let restored = session.restore_workspace(&manifest).unwrap();
    assert_eq!(restored.entries_restored, parsed.entries.len() as u64);
    session.close().unwrap();
}

#[test]
fn orphan_swept_and_gc_run_recorded() {
    let tmp = TempDir::new("session-basic");
    let dir = tmp.path().to_path_buf();
    let mut session = open_session(&dir);
    let (mut stray, queue) = stray_store(&dir);
    let orphan = stray.install(b"orphaned by the session").unwrap();
    stray.flush().unwrap();
    drop(stray);
    drop(queue);

    let report = session.run_gc(sweep_cfg()).unwrap();
    assert_eq!(report.scanned, 2, "genesis manifest + orphan");
    assert_eq!(report.quarantined, 1);
    assert_eq!(report.swept, 1);
    assert_eq!(report.restored_or_cleaned, 0);

    // the orphan is gone from disk, the genesis manifest survives
    let store = session.store();
    assert!(!store.exists(&orphan));
    let genesis = session.current_snapshot().expect("genesis pinned at open");
    assert!(store.exists(&genesis));

    // the canonical record: a gc.run envelope with the report payload, whose
    // snapshot is the pinned pre-GC manifest
    let env = gc_run_envelope(&dir);
    assert!(env.snapshot.is_some(), "gc.run is snapshot-pinned");
    let recorded = &env.payload["report"];
    assert_eq!(recorded["run_id"].as_str(), Some(report.run_id.as_str()));
    assert_eq!(recorded["swept"].as_u64(), Some(1));
    assert_eq!(recorded["quarantined"].as_u64(), Some(1));

    // the session stays fully usable: commit + export verify clean
    session
        .commit(
            vec![NewEvent {
                kind: "user_message".into(),
                payload_schema: 1,
                payload: json!({"text": "after gc"}),
                objects: vec![b"post-gc object".to_vec()],
                refs: Vec::new(),
            }],
            Some(session.composition().digest),
        )
        .unwrap();
    let export_dir = tmp.path().join("export");
    let report = session.export_bundle(&export_dir).unwrap();
    assert!(report.verified, "post-GC export finds nothing missing");
    assert!(report.missing.is_empty());
    session.close().unwrap();
}

#[test]
fn genesis_current_snapshot_is_a_live_root() {
    let tmp = TempDir::new("session-genesis");
    let dir = tmp.path().to_path_buf();
    let mut session = open_session(&dir);
    let (mut stray, queue) = stray_store(&dir);
    let orphan = stray.install(b"orphan").unwrap();
    stray.flush().unwrap();
    drop(stray);
    drop(queue);

    // fresh session, zero commits: the genesis manifest exists ONLY as the
    // live current_snapshot (no envelope references it yet) — it must not
    // be collected
    let report = session.run_gc(sweep_cfg()).unwrap();
    assert_eq!(report.swept, 1, "only the stray orphan");
    let genesis = session.current_snapshot().unwrap();
    assert!(session.store().exists(&genesis));
    assert!(!session.store().exists(&orphan));
    session.close().unwrap();
}

#[test]
fn committed_objects_and_manifest_survive() {
    let tmp = TempDir::new("session-refs");
    let dir = tmp.path().to_path_buf();
    let mut session = open_session(&dir);
    let (mut stray, queue) = stray_store(&dir);
    let orphan = stray.install(b"orphan").unwrap();
    stray.flush().unwrap();
    drop(stray);
    drop(queue);
    let receipt = session
        .commit(
            vec![NewEvent {
                kind: "user_message".into(),
                payload_schema: 1,
                payload: json!({"text": "referenced"}),
                objects: vec![b"referenced object".to_vec()],
                refs: Vec::new(),
            }],
            Some(session.composition().digest),
        )
        .unwrap();
    let referenced = receipt.objects[0];

    let report = session.run_gc(sweep_cfg()).unwrap();
    assert_eq!(report.quarantined, 1, "only the stray orphan");
    assert_eq!(report.swept, 1);
    assert!(session.store().exists(&referenced));
    assert_eq!(
        session.store().get(&referenced).unwrap(),
        b"referenced object"
    );
    assert!(session
        .store()
        .exists(&receipt.post_snapshot.expect("state-changing commit pins")));
    session.close().unwrap();
}

#[test]
fn writer_pins_protect_inflight_installs() {
    let tmp = TempDir::new("session-pins");
    let dir = tmp.path().to_path_buf();
    let mut session = open_session(&dir);
    let (mut stray, queue) = stray_store(&dir);
    let in_flight = stray.install(b"about to be referenced").unwrap();
    stray.flush().unwrap();
    drop(stray);
    drop(queue);

    // an external writer pins before its commit references the object
    session.gc_pin(in_flight);
    let report = session.run_gc(sweep_cfg()).unwrap();
    assert_eq!(report.quarantined, 0, "pinned digests are never quarantined");
    assert!(session.store().exists(&in_flight));

    // the pin falls away: the next run collects the still-unreferenced object
    session.gc_unpin(in_flight);
    let report = session.run_gc(sweep_cfg()).unwrap();
    assert_eq!(report.quarantined, 1);
    assert_eq!(report.swept, 1);
    assert!(!session.store().exists(&in_flight));
    session.close().unwrap();
}

#[test]
fn quarantine_survives_reopen_and_auto_gc_sweeps() {
    let tmp = TempDir::new("session-reopen");
    let dir = tmp.path().to_path_buf();
    {
        let mut session = open_session(&dir);
        let (mut stray, queue) = stray_store(&dir);
        let orphan = stray.install(b"crash-window orphan").unwrap();
        stray.flush().unwrap();
        drop(stray);
        drop(queue);
        // quarantine pass only (like a configured open after an upgrade)
        let report = session
            .run_gc(GcConfig {
                grace: Duration::from_secs(3600),
                sweep: false,
            })
            .unwrap();
        assert_eq!(report.quarantined, 1);
        session.close().unwrap();
    }

    // reopen with an automatic sweep pass: the quarantined file is
    // re-analyzed (not double-counted, not re-quarantined) and swept once
    // its grace has elapsed; open must not fail
    let mut session = Session::open(SessionConfig {
        dir: dir.clone(),
        gc: Some(sweep_cfg()),
        ..Default::default()
    })
    .unwrap();
    // the orphan is gone from disk; the sweep left a clean store (every
    // remaining object is referenced — nothing was double-counted or
    // re-quarantined across the reopen)
    let store = session.store();
    let survivors = store.scan().unwrap();
    assert!(!survivors.is_empty(), "the genesis manifest survives");
    let gc_dir = dir.join("objects/.gc");
    assert!(!gc_dir.exists() || std::fs::read_dir(&gc_dir).unwrap().count() == 0);
    session
        .commit(
            vec![NewEvent {
                kind: "user_message".into(),
                payload_schema: 1,
                payload: json!({"text": "after reopen"}),
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            None,
        )
        .unwrap();
    session.close().unwrap();

    // reopen with a quarantine-only config: nothing new is quarantined (the
    // previous run already swept), the session stays usable
    let mut session = Session::open(SessionConfig {
        dir,
        gc: Some(GcConfig {
            grace: Duration::from_secs(3600),
            sweep: false,
        }),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(session.store().quarantined().unwrap(), Vec::<Digest>::new());
    session.close().unwrap();
}

// --- memory-store GC --------------------------------------------------------

/// Seeds one lifetime-scope root with a claim (the gate_m4/m6 seeding
/// pattern) and drops one stray object into the memory store. Returns the
/// claim digest and the memory root path.
fn seed_lifetime_claim(memory_root: &Path) -> Digest {
    let session_id = kanbei_core::id::Id128::generate();
    let claim = Claim {
        schema: MEMORY_CLAIM_SCHEMA,
        claim_id: kanbei_core::id::Id128::generate(),
        kind: "decision".into(),
        content: "gc keeps me".into(),
        owner: kanbei_capabilities::Principal {
            session: session_id,
            generation: 0,
            run: None,
        },
        visibility_scope: MemoryScope::Lifetime,
        provenance: ClaimProvenance::new_ordinary(session_id, 1),
        observed_at: Some(1_700_000_000),
        valid_from: None,
        sensitivity: "public".into(),
    };
    let mut actor = MemoryRootActor::open(memory_root, MemoryScope::Lifetime).unwrap();
    let queue = Arc::new(DurabilityQueue::start("kb-gc-mem-seed"));
    let mut store =
        ObjectStore::open(&memory_root.join("lifetime/objects"), Arc::clone(&queue)).unwrap();
    let claim_digest = store.install(&claim.to_canonical_bytes()).unwrap();
    // a stray object no transition ever referenced
    store.install(b"memory orphan").unwrap();
    store.flush().unwrap();
    drop(store);
    if let Ok(q) = Arc::try_unwrap(queue) {
        let _ = q.shutdown();
    }
    let manifest = RootManifest {
        schema: MEMORY_ROOT_SCHEMA,
        parent: None,
        scope: MemoryScope::Lifetime,
        added_claims: vec![claim_digest],
        added_edges: vec![],
        retracted: vec![],
        transition_id: kanbei_core::id::Id128::generate(),
    };
    let transition = MemoryTransition {
        schema: MEMORY_TRANSITION_SCHEMA,
        transition_id: manifest.transition_id,
        scope: MemoryScope::Lifetime,
        kind: TransitionKind::RootApproval,
        expected_old_root: None,
        accepted_new_root: manifest.digest(),
        origin_session: session_id,
        origin_event: 1,
        origin_kind: "memory_root_approved".into(),
        decision_principal: kanbei_capabilities::Principal {
            session: session_id,
            generation: 0,
            run: None,
        },
        decision_digest: Digest::new(b"gc-seed-decision"),
        idempotency_key: IdempotencyKey {
            session: session_id,
            event: 1,
            decision: Digest::new(b"gc-seed-decision"),
        },
    };
    match actor.propose(transition, &[claim_digest], &[]).unwrap() {
        TransitionOutcome::Committed { .. } => {}
        other => panic!("seed propose: expected Committed, got {other:?}"),
    }
    actor.flush().unwrap();
    claim_digest
}

#[test]
fn memory_gc_sweeps_orphans_and_keeps_claims() {
    let tmp = TempDir::new("session-memgc");
    let memory_root = tmp.path().join("memory");
    std::fs::create_dir_all(&memory_root).unwrap();
    let claim_digest = seed_lifetime_claim(&memory_root);

    let mut session = Session::open(SessionConfig {
        dir: tmp.path().join("session"),
        memory_root: Some(memory_root.clone()),
        ..Default::default()
    })
    .unwrap();

    // a stray memory object installed after open (second-handle pattern)
    let queue = Arc::new(DurabilityQueue::start("kb-gc-mem-stray"));
    let mut store =
        ObjectStore::open(&memory_root.join("lifetime/objects"), Arc::clone(&queue)).unwrap();
    let stray = store.install(b"memory orphan 2").unwrap();
    store.flush().unwrap();
    drop(store);
    drop(queue);

    let reports = session.run_memory_gc(sweep_cfg()).unwrap();
    assert_eq!(reports.len(), 1, "lifetime scope only (no project bound)");
    let (scope, report) = &reports[0];
    assert_eq!(*scope, MemoryScope::Lifetime);
    // the seeding step's stray + the post-open stray are both swept
    assert_eq!(report.swept, 2);
    assert_eq!(report.restored_or_cleaned, 0);

    // the claim, its manifest, and the head survive — the fold still
    // resolves the seeded claim
    let actor = session.memory_lifetime();
    assert!(!actor.store().exists(&stray));
    assert!(!actor.store().exists(&Digest::new(b"memory orphan")));
    assert!(actor.store().exists(&claim_digest));
    let fold = actor.fold(actor.head()).unwrap();
    assert_eq!(fold.claims.len(), 1);
    assert_eq!(fold.claims[0].0, claim_digest);
    session.close().unwrap();
}

// --- config/module pins (guest-gated) ---------------------------------------

#[test]
fn config_package_and_module_pin_survive_gc() {
    if !require_guest() {
        return;
    }
    let tmp = TempDir::new("session-config");
    let dir = tmp.path().to_path_buf();
    let config = PackageManifest {
        schema: 1,
        module_id: kanbei_core::id::Id128::generate(),
        origin: ModuleOrigin::UserConfig,
        trust_class: kanbei_capabilities::TrustClass::User,
        scope: kanbei_services::ScopePath(vec![]),
        deps: vec![],
        capabilities: vec![],
        source: "function kb_on_activate(ctx) ctx.service_publish('{\"scope\":[],\"name\":\"gc-greeter\"}', 1, '[]') end\nfunction kb_hot(x) return x end".into(),
        state_schema: None,
    };
    let package_digest = Digest::new(&serde_json::to_vec(&config).unwrap());
    let mut session = Session::open(SessionConfig {
        dir: dir.clone(),
        config: Some(config),
        engine: Some(no_epoch()),
        ..Default::default()
    })
    .unwrap();
    let (mut stray, queue) = stray_store(&dir);
    let orphan = stray.install(b"orphan").unwrap();
    stray.flush().unwrap();
    drop(stray);
    drop(queue);

    let report = session.run_gc(sweep_cfg()).unwrap();
    assert_eq!(report.swept, 1, "only the stray orphan");
    assert_eq!(report.restored_or_cleaned, 0);

    // the activated config package (live config_digest) survives, and the
    // package pin inside every snapshot manifest's closure survives with it
    assert!(session.store().exists(&package_digest));
    assert!(!session.store().exists(&orphan));
    session.close().unwrap();
}
