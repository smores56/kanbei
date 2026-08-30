//! M1 milestone gate tests: the acceptance bullets and consistency tests that
//! M1 exercises (docs/architecture.md lines 629-663): 3 Canonical fact, 4
//! Snapshot, 5 Payload, 7 Recovery, 11 Causality, 12 Projection, 14 Evolution
//! (upcast fixture), plus the SQLite-deletion acceptance bullet.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use kanbei_core::digest::Digest;
use kanbei_core::queue::DurabilityQueue;
use kanbei_core::registry::{
    Registry, Report, upcast_tool_result_v1_to_v2, upcast_user_message_v1_to_v2,
};
use kanbei_log::{FrameInfo, Profile, for_each_frame, hex, new_prev};
use kanbei_objects::ObjectStore;
use kanbei_projection::{rebuild, reconstruct};
use kanbei_scopes::epoch::CompositionStore;
use kanbei_scopes::registry::ContributionRegistry;
use kanbei_services::ServiceRegistry;
use kanbei_session::{NewEvent, Session, SessionConfig};
use kanbei_snapshot::{ExecutionManifest, verify_closure};
use kanbei_testkit::collect_envelopes;
use serde_json::json;

fn fresh_session_dir(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "kanbei-gate-{name}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Best-effort cleanup at test end; never fail a test on cleanup errors.
struct DirGuard(PathBuf);
impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn open_session(dir: &Path, profile: Profile) -> Session {
    Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        stream: "gate".into(),
        profile,
        ..Default::default()
    })
    .unwrap()
}

fn test_event(i: u64, objects: u64) -> NewEvent {
    NewEvent {
        kind: "test_event".into(),
        payload_schema: 1,
        payload: json!({"i": i}),
        objects: (0..objects).map(|j| format!("object-{i}-{j}").into_bytes()).collect(),
        refs: vec![],
    }
}

fn open_store(dir: &Path) -> (ObjectStore, Arc<DurabilityQueue>) {
    let queue = Arc::new(DurabilityQueue::start("kb-gate-store"));
    let store = ObjectStore::open(&dir.join("objects"), Arc::clone(&queue)).unwrap();
    (store, queue)
}

fn shutdown_store(store: ObjectStore, queue: Arc<DurabilityQueue>) {
    drop(store);
    let q = Arc::try_unwrap(queue).unwrap_or_else(|_| panic!("store queue still shared"));
    q.shutdown().unwrap();
}

fn fixture_registry() -> Registry {
    let mut r = Registry::new();
    r.register("user_message", 1, upcast_user_message_v1_to_v2)
        .unwrap();
    r.register("tool_result", 1, upcast_tool_result_v1_to_v2)
        .unwrap();
    r
}

/// Report lacks PartialEq/Serialize (BTreeMap-based), so compare field by
/// field.
fn assert_report_eq(a: &Report, b: &Report) {
    assert_eq!(a.events, b.events);
    assert_eq!(a.missing_objects, b.missing_objects);
    assert_eq!(a.upcast_errors, b.upcast_errors);
    assert_eq!(a.kinds.len(), b.kinds.len());
    for (kind, sa) in &a.kinds {
        let sb = b
            .kinds
            .get(kind)
            .unwrap_or_else(|| panic!("kind {kind:?} missing in second report"));
        assert_eq!(sa.schema, sb.schema);
        assert_eq!(sa.count, sb.count);
        assert_eq!(sa.upcasted, sb.upcasted);
        assert_eq!(sa.opaque, sb.opaque);
        assert_eq!(sa.opaque_reason, sb.opaque_reason);
    }
}

fn walk(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    fn rec(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let entry = entry.unwrap();
            let p = entry.path();
            if p.is_dir() {
                out.push(format!("{}/", p.strip_prefix(base).unwrap().to_string_lossy()));
                rec(&p, base, out);
            } else {
                out.push(p.strip_prefix(base).unwrap().to_string_lossy().into_owned());
            }
        }
    }
    rec(dir, dir, &mut out);
    out.sort();
    out
}

/// Recompute a frame's digest exactly as kanbei-log does: blake3 over the
/// metadata JSON with the digest field dropped (serde_json object keys sort
/// identically on both sides) plus the event lines.
fn frame_digest(f: &FrameInfo) -> [u8; 32] {
    let meta_no_digest = serde_json::json!({
        "stream": f.meta.stream,
        "schema": f.meta.schema,
        "first_seq": f.meta.first_seq,
        "last_seq": f.meta.last_seq,
        "count": f.meta.count,
        "prev": f.meta.prev,
        "created_us": f.meta.created_us,
    });
    let mut canonical = serde_json::to_vec(&meta_no_digest).unwrap();
    canonical.push(b'\n');
    for e in &f.events {
        canonical.extend_from_slice(e.as_bytes());
        canonical.push(b'\n');
    }
    *blake3::hash(&canonical).as_bytes()
}

// ---------- acceptance: SQLite deletion followed by audit reconstruction ----------

#[test]
fn acceptance_consistency_12_sqlite_deletion_reconstruction() {
    let dir = fresh_session_dir("c12");
    let _guard = DirGuard(dir.clone());
    let mut s = open_session(&dir, Profile::Fast);
    for c in 0..10 {
        let mut events = Vec::new();
        for j in 0..3 {
            let i = (c * 3 + j) as u64 + 1;
            let mut ev = test_event(i, 0);
            if c % 2 == 0 {
                ev.objects = vec![
                    format!("obj-{i}-0").into_bytes(),
                    format!("obj-{i}-1").into_bytes(),
                ];
            }
            ev.kind = "user_message".into();
            ev.payload = json!({"text": format!("msg {i}")});
            events.push(ev);
        }
        // distinct state head per commit: every commit pins a fresh manifest
        s.commit(events, Some(Digest::new(format!("state-{c}").as_bytes())))
            .unwrap();
    }
    s.close().unwrap();

    let (store, queue) = open_store(&dir);
    let registry = fixture_registry();
    let log = dir.join("log.zst");
    let db = dir.join("proj.db");

    let report_a = rebuild(&log, &db, &registry, &store).unwrap();
    // SQLite is disposable (R-23): delete the db and its WAL siblings, then
    // rebuild from canonical truth
    for f in ["proj.db", "proj.db-wal", "proj.db-shm"] {
        let _ = std::fs::remove_file(dir.join(f));
    }
    let report_b = rebuild(&log, &db, &registry, &store).unwrap();
    assert_report_eq(&report_a, &report_b);

    let conn = rusqlite::Connection::open(&db).unwrap();
    let rows: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(rows, 30);
    let wm: i64 = conn
        .query_row("SELECT last_seq FROM watermarks WHERE stream = 'gate'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(wm, 30);
    shutdown_store(store, queue);
}

// ---------- acceptance: custom schemas/upcasters reconstruct or report precise partial availability ----------

#[test]
fn acceptance_consistency_14_upcast_fixture() {
    let dir = fresh_session_dir("c14");
    let _guard = DirGuard(dir.clone());
    let mut s = open_session(&dir, Profile::Fast);

    // user_message + an installed object (S6 fixture shape)
    let receipt = s
        .commit(
            vec![NewEvent {
                kind: "user_message".into(),
                payload_schema: 1,
                payload: json!({"text": "hello fixture"}),
                objects: vec![b"installed object bytes".to_vec()],
                refs: vec![],
            }],
            None,
        )
        .unwrap();
    let obj = receipt.objects[0];

    // tool_result x2 referencing the installed object + unknown future_kind
    s.commit(
        vec![
            NewEvent {
                kind: "tool_result".into(),
                payload_schema: 1,
                payload: json!({"tool": "read_file", "ok": true}),
                objects: vec![],
                refs: vec![obj],
            },
            NewEvent {
                kind: "tool_result".into(),
                payload_schema: 1,
                payload: json!({"tool": "write_file", "ok": false}),
                objects: vec![],
                refs: vec![obj],
            },
            NewEvent {
                kind: "future_kind".into(),
                payload_schema: 9,
                payload: json!({"mystery": 42}),
                objects: vec![],
                refs: vec![],
            },
        ],
        None,
    )
    .unwrap();
    s.close().unwrap();

    let (store, queue) = open_store(&dir);
    let log = dir.join("log.zst");

    let rep = reconstruct(&log, &fixture_registry(), &store).unwrap();
    assert_eq!(rep.events, 4);
    let um = rep.kinds.get("user_message").expect("user_message kind");
    assert_eq!((um.schema, um.count, um.upcasted, um.opaque), (1, 1, 1, 0));
    let tr = rep.kinds.get("tool_result").expect("tool_result kind");
    assert_eq!((tr.schema, tr.count, tr.upcasted, tr.opaque), (1, 2, 2, 0));
    let fk = rep.kinds.get("future_kind").expect("future_kind kind");
    assert_eq!((fk.schema, fk.count, fk.upcasted, fk.opaque), (9, 1, 0, 1));
    assert_eq!(
        fk.opaque_reason.as_deref(),
        Some("no upcaster for kind 'future_kind' schema 9")
    );
    assert!(rep.missing_objects.is_empty(), "missing: {:?}", rep.missing_objects);
    assert!(rep.upcast_errors.is_empty(), "errors: {:?}", rep.upcast_errors);

    // empty registry: typed interpretation degrades to all-opaque (R-06)
    let opaque = reconstruct(&log, &Registry::new(), &store).unwrap();
    assert_eq!(opaque.events, 4);
    for (kind, stat) in &opaque.kinds {
        assert_eq!(stat.upcasted, 0, "kind {kind} must be opaque");
        assert_eq!(stat.opaque, stat.count, "kind {kind} opaque count");
    }
    assert_eq!(opaque.kinds["future_kind"].opaque_reason.as_deref(), Some(
        "no upcaster for kind 'future_kind' schema 9"
    ));
    shutdown_store(store, queue);
}

// ---------- consistency 4: snapshot closure verifies ----------

#[test]
fn acceptance_consistency_4_snapshot_closure_verifies() {
    let dir = fresh_session_dir("c4");
    let _guard = DirGuard(dir.clone());
    let mut s = open_session(&dir, Profile::Fast);

    // 50 state-changing commits with the SAME state head: every post-manifest
    // is byte-identical and dedups to one object
    let state_v1 = Digest::new(b"state-v1");
    for i in 1..=50u64 {
        s.commit(vec![test_event(i, 2)], Some(state_v1)).unwrap();
    }
    // then 5 distinct state heads
    for i in 51..=55u64 {
        let head = Digest::new(format!("state-{i}").as_bytes());
        s.commit(vec![test_event(i, 2)], Some(head)).unwrap();
    }
    s.close().unwrap();

    let envelopes = collect_envelopes(&dir).unwrap();
    assert_eq!(envelopes.len(), 55);

    // closure = every envelope's refs ∪ snapshots (the first envelope's
    // snapshot is the genesis bootstrap digest, so it is covered)
    let mut closure: HashSet<Digest> = HashSet::new();
    for env in &envelopes {
        closure.extend(env.refs.iter().copied());
        if let Some(snap) = env.snapshot {
            closure.insert(snap);
        }
    }

    let (store, queue) = open_store(&dir);
    assert_eq!(verify_closure(&store, &closure).unwrap(), closure.len() as u64);
    // no crash here: the only unreferenced objects are the live final-state
    // manifest — R-08 materializes the post-manifest at the last commit, and
    // no successor envelope references it yet (legitimate, not crash garbage)
    // — plus the epoch-composition object the schema-2 manifests pin (R-01:
    // the composition digest is a manifest field, not an envelope ref)
    // — plus the tool-registry object the M6 wave 2 manifests pin (same
    // manifest-field, not-envelope-ref status)
    let on_disk = store.scan().unwrap();
    let mut orphans: Vec<Digest> =
        on_disk.iter().filter(|d| !closure.contains(d)).copied().collect();
    let final_head = Digest::new(b"state-55");
    let final_manifest = on_disk
        .iter()
        .filter_map(|d| {
            let m: ExecutionManifest = serde_json::from_slice(&store.get(d).unwrap()).ok()?;
            (m.state_head == Some(final_head)).then_some(*d)
        })
        .next()
        .expect("the live final manifest exists in the store");
    let mut expected = vec![
        final_manifest,
        CompositionStore::new(&ContributionRegistry::new(Arc::new(Mutex::new(
            ServiceRegistry::new(),
        ))))
        .current()
        .digest,
        Digest::new(&kanbei_tools::ToolRegistry::builtin().to_canonical_bytes()),
    ];
    expected.sort();
    orphans.sort();
    assert_eq!(orphans, expected, "only the live manifest + composition may be unreferenced");

    // manifest dedup: bootstrap + 1 (identical state-v1 manifests) + 5
    // (distinct) = 7 manifest objects
    let manifests = store
        .scan()
        .unwrap()
        .iter()
        .filter(|d| serde_json::from_slice::<ExecutionManifest>(&store.get(d).unwrap()).is_ok())
        .count();
    assert_eq!(manifests, 7);
    shutdown_store(store, queue);
}

// ---------- consistency 3: canonical fact ----------

#[test]
fn acceptance_consistency_3_canonical_fact() {
    let dir = fresh_session_dir("c3");
    let _guard = DirGuard(dir.clone());
    let mut s = open_session(&dir, Profile::Fast);
    for i in 1..=10u64 {
        let head = Some(Digest::new(format!("state-{i}").as_bytes()));
        s.commit(vec![test_event(i, 2)], head).unwrap();
    }
    s.close().unwrap();

    let (store, queue) = open_store(&dir);
    let rep = reconstruct(&dir.join("log.zst"), &Registry::new(), &store).unwrap();
    assert_eq!(rep.events, 10);
    let k = rep.kinds.get("test_event").expect("test_event kind");
    assert_eq!(k.count, 10);

    // canonical facts on disk: bootstrap + 10 state manifests + 20 objects
    // + the epoch-composition object (schema-2 manifests pin it, R-01)
    // + the tool-registry object (M6 wave 2 manifests pin it; content
    // addressing dedups it across the 10 commits)
    let scan = store.scan().unwrap();
    assert_eq!(scan.len(), 33);

    // snapshot chain: envelope k's pre-event snapshot is the post-manifest
    // pinned by commit k-1 (bootstrap for the first) — the schema-2
    // manifests, located by the state head they pin (R-08)
    let bootstrap = Digest::new(&ExecutionManifest::bootstrap().to_bytes());
    let manifests: Vec<(Digest, ExecutionManifest)> = scan
        .iter()
        .filter_map(|d| {
            serde_json::from_slice::<ExecutionManifest>(&store.get(d).unwrap())
                .ok()
                .map(|m| (*d, m))
        })
        .collect();
    let envelopes = collect_envelopes(&dir).unwrap();
    assert_eq!(envelopes.len(), 10);
    for (idx, env) in envelopes.iter().enumerate() {
        let k = idx as u64 + 1;
        let expected = if k == 1 {
            bootstrap
        } else {
            let head = Digest::new(format!("state-{}", k - 1).as_bytes());
            manifests
                .iter()
                .find(|(_, m)| m.state_head == Some(head))
                .map(|(d, _)| *d)
                .unwrap_or_else(|| panic!("no manifest pins state head {head}"))
        };
        assert_eq!(env.snapshot, Some(expected), "envelope seq {k}");
    }
    shutdown_store(store, queue);
}

// ---------- consistency 5: payload inline vs object ----------

#[test]
fn acceptance_consistency_5_payload_inline_vs_object() {
    let dir = fresh_session_dir("c5");
    let _guard = DirGuard(dir.clone());
    let mut s = open_session(&dir, Profile::Fast);
    s.commit(
        vec![NewEvent {
            kind: "test_event".into(),
            payload_schema: 1,
            payload: json!({"text": "hi"}),
            objects: vec![],
            refs: vec![],
        }],
        None,
    )
    .unwrap();
    // > 1 KB serialized payload (> inline_max 1024) → promoted to an object
    let big = json!({"text": "a".repeat(2000)});
    s.commit(
        vec![NewEvent {
            kind: "test_event".into(),
            payload_schema: 1,
            payload: big.clone(),
            objects: vec![],
            refs: vec![],
        }],
        None,
    )
    .unwrap();
    s.close().unwrap();

    let (store, queue) = open_store(&dir);
    let envelopes = collect_envelopes(&dir).unwrap();
    assert_eq!(envelopes.len(), 2);

    let small = &envelopes[0];
    assert_eq!(small.payload, json!({"text": "hi"}));
    assert!(small.refs.is_empty(), "small payload must stay inline");

    let large = &envelopes[1];
    assert_eq!(large.refs.len(), 1);
    let d = large.refs[0];
    assert_eq!(large.payload, json!({"$object": d.to_string()}));
    // the stored bytes are the serialized payload JSON, hash-verified on read
    assert_eq!(store.get(&d).unwrap(), serde_json::to_vec(&big).unwrap());
    assert!(store.get(&d).unwrap().len() > 1024);
    shutdown_store(store, queue);
}

// ---------- consistency 7: recovery without effects ----------

#[test]
fn acceptance_consistency_7_recovery_without_effects() {
    let dir = fresh_session_dir("c7");
    let _guard = DirGuard(dir.clone());
    let mut s = open_session(&dir, Profile::Fast);
    for i in 1..=12u64 {
        s.commit(vec![test_event(i, 2)], None).unwrap();
    }
    s.close().unwrap();

    let before = walk(&dir);
    let (store, queue) = open_store(&dir);
    let rep = reconstruct(&dir.join("log.zst"), &Registry::new(), &store).unwrap();
    let after = walk(&dir);
    // reconstruct is read-only (for_each_frame never truncates) and needs no
    // SQLite db — the file set must be unchanged
    assert_eq!(before, after, "reconstruct must not touch the session dir");
    assert_eq!(rep.events, 12);
    assert_eq!(rep.kinds["test_event"].count, 12);
    shutdown_store(store, queue);
}

// ---------- consistency 11: causality (explicit refs + frame chain) ----------

#[test]
fn consistency_11_causality_explicit_refs() {
    let dir = fresh_session_dir("c11");
    let _guard = DirGuard(dir.clone());
    let mut s = open_session(&dir, Profile::Fast);
    for i in 1..=8u64 {
        let head = Some(Digest::new(format!("state-{i}").as_bytes()));
        s.commit(vec![test_event(i, 2)], head).unwrap();
    }
    s.close().unwrap();

    // recover() succeeding IS the frame-chain check: it verifies prev
    // continuity, self digests, counts, and seq ranges on every frame
    let rec = kanbei_log::recover(&dir.join("log.zst")).unwrap();
    assert_eq!(rec.events, 8);
    assert!(!rec.truncated);

    // recompute the chain byte-for-byte: frame k's prev == digest of frame
    // k-1 (zeros for genesis), and each frame's digest == its own recompute
    let mut prev = new_prev();
    let mut frames = 0u64;
    for_each_frame(&dir.join("log.zst"), |f| {
        assert_eq!(f.meta.prev, hex(&prev), "frame {frames}: prev chain");
        let got = frame_digest(f);
        assert_eq!(f.meta.digest, hex(&got), "frame {frames}: self digest");
        prev = got;
        frames += 1;
    })
    .unwrap();
    assert_eq!(frames, rec.frames);

    // every ref and snapshot is a parseable `blake3:<64 hex>` digest that
    // points at an existing object (R-10: no dangling references)
    let (store, queue) = open_store(&dir);
    for env in collect_envelopes(&dir).unwrap() {
        for r in &env.refs {
            assert_eq!(Digest::from_str(&r.to_string()), Ok(*r), "seq {}", env.seq);
            assert!(store.exists(r), "seq {}: ref {r} missing", env.seq);
        }
        if let Some(snap) = env.snapshot {
            assert_eq!(Digest::from_str(&snap.to_string()), Ok(snap), "seq {}", env.seq);
            assert!(store.exists(&snap), "seq {}: snapshot {snap} missing", env.seq);
        }
    }
    shutdown_store(store, queue);
}
