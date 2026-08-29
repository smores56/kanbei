//! Integration tests for kanbei-session: the M1 synchronous commit path —
//! genesis pinning, commit + recovery roundtrips, object flow, payload
//! classification, ref verification, manifest pinning, fault points, flush,
//! torn-tail recovery, and seq continuity.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use kanbei_core::digest::Digest;
use kanbei_core::envelope::{Envelope, ENVELOPE_SCHEMA};
use kanbei_log::for_each_frame;
use kanbei_session::{
    FaultInjector, FaultPoint, NewEvent, Session, SessionConfig, SessionError,
};
use serde_json::json;

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kb-session-test-{tag}-{}-{}",
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

fn open(dir: &Path) -> Session {
    Session::open(SessionConfig { dir: dir.to_path_buf(), ..Default::default() }).unwrap()
}

fn event(kind: &str, payload: serde_json::Value) -> NewEvent {
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

/// Fault injector that records every point it sees, in order.
struct Recorder(Arc<Mutex<Vec<FaultPoint>>>);

impl Recorder {
    fn new() -> (Self, Arc<Mutex<Vec<FaultPoint>>>) {
        let points = Arc::new(Mutex::new(Vec::new()));
        (Self(Arc::clone(&points)), points)
    }
}

impl FaultInjector for Recorder {
    fn inject(&self, point: FaultPoint) {
        self.0.lock().unwrap().push(point);
    }
}

#[test]
fn genesis_pins_bootstrap() {
    let dir = TempDir::new("genesis");
    let session = open(dir.path());
    assert!(session.current_snapshot().is_some(), "fresh session pins genesis");
    let objects = session.store().scan().unwrap();
    assert_eq!(objects.len(), 1, "objects dir holds exactly the bootstrap manifest");
    session.close().unwrap();
}

#[test]
fn commit_recovery_roundtrip() {
    let dir = TempDir::new("roundtrip");
    let objects: Vec<Vec<u8>> =
        vec![b"first object bytes".to_vec(), b"second object bytes".to_vec()];
    {
        let mut session = open(dir.path());
        let receipt = session
            .commit(
                vec![
                    event("pure", json!({"n": 1})),
                    NewEvent {
                        kind: "with_obj_1".into(),
                        payload_schema: 1,
                        payload: json!({"n": 2}),
                        objects: vec![objects[0].clone()],
                        refs: Vec::new(),
                    },
                    NewEvent {
                        kind: "with_obj_2".into(),
                        payload_schema: 1,
                        payload: json!({"n": 3}),
                        objects: vec![objects[1].clone()],
                        refs: Vec::new(),
                    },
                ],
                None,
            )
            .unwrap();
        assert_eq!((receipt.first_seq, receipt.last_seq, receipt.count), (1, 3, 3));
        session.close().unwrap();
    }
    // the log holds exactly 3 events
    let recovered = kanbei_log::recover(&dir.path().join("log.zst")).unwrap();
    assert_eq!(recovered.events, 3);
    // reopen: seq continues, objects survive, manifest state is not resumed
    let session = open(dir.path());
    assert_eq!(session.next_seq(), 4);
    assert_eq!(session.current_snapshot(), None, "M1 resumes without manifest state");
    for bytes in &objects {
        let digest = Digest::new(bytes);
        assert_eq!(session.store().get(&digest).unwrap().as_slice(), bytes.as_slice());
    }
    session.close().unwrap();
}

#[test]
fn commit_object_flow() {
    let dir = TempDir::new("object-flow");
    let mut session = open(dir.path());
    let bytes = b"payload-bytes".to_vec();
    let digest = Digest::new(&bytes);
    let receipt = session
        .commit(
            vec![NewEvent {
                kind: "with_obj".into(),
                payload_schema: 1,
                payload: json!({"text": "hi"}),
                objects: vec![bytes.clone()],
                refs: Vec::new(),
            }],
            None,
        )
        .unwrap();
    assert_eq!(receipt.objects, vec![digest]);
    // the envelope's refs carry the installed digest
    let env = &envelopes(session.log_path())[0];
    assert_eq!(env.refs, vec![digest]);
    // the store roundtrips the bytes
    assert_eq!(session.store().get(&digest).unwrap().as_slice(), bytes.as_slice());
    session.close().unwrap();
}

#[test]
fn payload_classification() {
    let dir = TempDir::new("classification");
    let mut session = open(dir.path());
    let big = json!({"data": "x".repeat(2048)});
    let big_serialized = serde_json::to_string(&big).unwrap();
    assert!(big_serialized.len() > 1024, "test payload must exceed the default inline_max");
    let big_digest = Digest::new(big_serialized.as_bytes());
    let small = json!({"text": "hi"});
    session
        .commit(
            vec![
                NewEvent {
                    kind: "big".into(),
                    payload_schema: 1,
                    payload: big,
                    objects: Vec::new(),
                    refs: Vec::new(),
                },
                NewEvent {
                    kind: "small".into(),
                    payload_schema: 1,
                    payload: small.clone(),
                    objects: Vec::new(),
                    refs: Vec::new(),
                },
            ],
            None,
        )
        .unwrap();
    let envs = envelopes(session.log_path());
    // oversized payload becomes an object reference
    assert_eq!(envs[0].payload, json!({ "$object": big_digest.to_string() }));
    assert!(envs[0].refs.contains(&big_digest));
    // small payload stays verbatim
    assert_eq!(envs[1].payload, small);
    assert!(envs[1].refs.is_empty());
    // the promoted payload is retrievable as an object
    assert_eq!(
        session.store().get(&big_digest).unwrap().as_slice(),
        big_serialized.as_bytes()
    );
    session.close().unwrap();
}

#[test]
fn explicit_refs_verified() {
    let dir = TempDir::new("missing-ref");
    let mut session = open(dir.path());
    let missing = Digest::new(b"does-not-exist");
    let err = session
        .commit(
            vec![NewEvent {
                kind: "bad".into(),
                payload_schema: 1,
                payload: json!(null),
                objects: Vec::new(),
                refs: vec![missing],
            }],
            None,
        )
        .unwrap_err();
    assert!(matches!(err, SessionError::MissingObject { digest } if digest == missing));
    // nothing committed: the log has no events and seq did not advance
    let recovered = kanbei_log::recover(session.log_path()).unwrap();
    assert_eq!(recovered.events, 0);
    assert_eq!(session.next_seq(), 1);
    session.close().unwrap();
}

#[test]
fn state_change_pins_manifest() {
    let dir = TempDir::new("state-pin");
    let mut session = open(dir.path());
    let state1 = Digest::new(b"state-1");
    let receipt1 = session.commit(vec![event("change", json!({"s": 1}))], Some(state1)).unwrap();
    let post = receipt1.post_snapshot.expect("state change pins a manifest");
    assert_eq!(session.current_snapshot(), Some(post));
    assert!(session.store().exists(&post), "pinned manifest is stored");
    assert!(session.store().get(&post).is_ok(), "pinned manifest reads back");
    // a pure commit references the pinned manifest and pins nothing new
    let receipt2 = session.commit(vec![event("pure", json!({"s": 2}))], None).unwrap();
    assert_eq!(receipt2.pre_snapshot, Some(post));
    assert_eq!(receipt2.post_snapshot, None);
    assert_eq!(session.current_snapshot(), Some(post), "pure commit leaves the manifest unchanged");
    session.close().unwrap();
}

#[test]
fn snapshot_ref_on_envelopes() {
    let dir = TempDir::new("snapshot-ref");
    let mut session = open(dir.path());
    let genesis = session.current_snapshot().expect("fresh session pins genesis");
    session.commit(vec![event("first", json!({"n": 1}))], None).unwrap();
    let envs = envelopes(session.log_path());
    assert_eq!(envs.len(), 1);
    assert_eq!(envs[0].snapshot, Some(genesis));
    assert_eq!(envs[0].env, ENVELOPE_SCHEMA);
    assert!(envs[0].validate().is_ok());
    session.close().unwrap();
}

#[test]
fn fault_points_recorded() {
    let dir = TempDir::new("fault-points");
    let (recorder, points) = Recorder::new();
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        fault: Some(Arc::new(recorder)),
        ..Default::default()
    })
    .unwrap();
    // 2 explicit objects + 1 promoted payload = 3 installs
    let big = json!({"data": "y".repeat(2048)});
    session
        .commit(
            vec![NewEvent {
                kind: "faulted".into(),
                payload_schema: 1,
                payload: big,
                objects: vec![b"obj-a".to_vec(), b"obj-b".to_vec()],
                refs: Vec::new(),
            }],
            None,
        )
        .unwrap();
    let points = points.lock().unwrap();
    assert_eq!(
        *points,
        vec![
            FaultPoint::BeforeObjectInstall,
            FaultPoint::AfterObjectInstall, // obj-a
            FaultPoint::AfterObjectInstall, // obj-b
            FaultPoint::AfterObjectInstall, // promoted payload
            FaultPoint::BeforeFrameAppend,
            FaultPoint::AfterFrameAppend,
        ]
    );
    session.close().unwrap();
}

#[test]
fn flush_durable() {
    let dir = TempDir::new("flush");
    let mut session = open(dir.path());
    session
        .commit(vec![event("a", json!({"n": 1})), event("b", json!({"n": 2}))], None)
        .unwrap();
    // barrier covers the frame fsync and all pending object dirsyncs
    session.flush().unwrap();
    let recovered = kanbei_log::recover(session.log_path()).unwrap();
    assert_eq!(recovered.events, 2);
    session.close().unwrap();
}

#[test]
fn torn_tail_across_restart() {
    let dir = TempDir::new("torn-tail");
    {
        let mut session = open(dir.path());
        // 7 events in one frame, then a final single-event frame to tear
        let batch: Vec<NewEvent> = (1..=7).map(|i| event("bulk", json!({"n": i}))).collect();
        session.commit(batch, None).unwrap();
        // incompressible payload so the final frame comfortably exceeds the tear
        let noise: String =
            (0..1500).map(|i| char::from(b'!' + ((i * 7) % 90) as u8)).collect();
        session.commit(vec![event("tail", json!({"noise": noise}))], None).unwrap();
        session.close().unwrap();
    }
    let log_path = dir.path().join("log.zst");
    let (boundaries, _) = kanbei_log::scan_frames(&log_path).unwrap();
    let final_frame_len = boundaries.last().unwrap().1;
    assert!(final_frame_len > 100, "final frame must exceed the tear size: {final_frame_len}");
    // tear mid-final-frame
    let len = std::fs::metadata(&log_path).unwrap().len();
    let f = std::fs::OpenOptions::new().write(true).open(&log_path).unwrap();
    f.set_len(len - 100).unwrap();
    drop(f);
    // recovery sees and truncates the tear
    let recovered = kanbei_log::recover(&log_path).unwrap();
    assert!(recovered.truncated);
    assert_eq!(recovered.events, 7);
    assert_eq!(recovered.last_seq, 7);
    // the session resumes at the last complete event and can append
    let mut session = open(dir.path());
    assert_eq!(session.next_seq(), 8);
    let receipt = session.commit(vec![event("after-tear", json!({"n": 8}))], None).unwrap();
    assert_eq!(receipt.first_seq, 8);
    assert_eq!(session.next_seq(), 9);
    session.close().unwrap();
}

#[test]
fn seq_continuity_enforced() {
    let dir = TempDir::new("seq-continuity");
    let mut session = open(dir.path());
    let first = session
        .commit(
            vec![
                event("a", json!({"n": 1})),
                event("b", json!({"n": 2})),
                event("c", json!({"n": 3})),
            ],
            None,
        )
        .unwrap();
    assert_eq!((first.first_seq, first.last_seq, first.count), (1, 3, 3));
    let second = session.commit(vec![event("d", json!({"n": 4})), event("e", json!({"n": 5}))], None).unwrap();
    assert_eq!((second.first_seq, second.last_seq, second.count), (4, 5, 2));
    let envs = envelopes(session.log_path());
    let seqs: Vec<u64> = envs.iter().map(|e| e.seq).collect();
    assert_eq!(seqs, vec![1, 2, 3, 4, 5]);
    for env in &envs {
        env.validate().unwrap();
    }
    session.close().unwrap();
}
