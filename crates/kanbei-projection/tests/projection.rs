//! Integration tests for kanbei-projection: the M1 audit reconstruction
//! report (S6 fixture shape), the disposable SQLite rebuild (S5 + R-23
//! watermarks, destructive idempotence), explicit corruption and
//! envelope-validation failures, upcast error reporting, and streaming
//! rebuild at 200k events.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use kanbei_core::queue::DurabilityQueue;
use kanbei_core::registry::{
    upcast_tool_result_v1_to_v2, upcast_user_message_v1_to_v2, upcast_user_message_v2_to_v3,
};
use kanbei_core::{Digest, Envelope, ENVELOPE_SCHEMA, Registry};
use kanbei_log::{AppendLog, Profile, RecoveryError};
use kanbei_objects::ObjectStore;
use kanbei_projection::{rebuild, reconstruct, PROJECTION_SCHEMA, RebuildError, TX_BATCH};
use rusqlite::Connection;
use serde_json::{json, Value};

fn tmp_dir(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("kb-projection-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&p);
    std::fs::create_dir_all(&p).unwrap();
    p
}

fn registry() -> Registry {
    let mut r = Registry::new();
    r.register("user_message", 1, upcast_user_message_v1_to_v2)
        .unwrap();
    r.register("tool_result", 1, upcast_tool_result_v1_to_v2)
        .unwrap();
    r
}

/// The S6 fixture stream: user_message v1, tool_result v1 x2 (one ref to an
/// installed object, one to a missing digest), future_kind schema 9.
/// Returns (log_path, store, queue, present, missing).
fn fixture_log(dir: &Path, stream: &str, tag: &str) -> (PathBuf, ObjectStore, Arc<DurabilityQueue>, Digest, Digest) {
    let queue = Arc::new(DurabilityQueue::start(&format!("kb-proj-{tag}-queue")));
    let log_path = dir.join("events.log.zst");
    let mut store = ObjectStore::open(&dir.join("objects"), Arc::clone(&queue)).unwrap();
    let present = store.install(b"object bytes behind the present ref").unwrap();
    let missing = Digest::from_hex(
        "blake3:deadbeef00000000000000000000000000000000000000000000000000000000",
    )
    .unwrap();

    let envs = vec![
        Envelope {
            env: ENVELOPE_SCHEMA,
            seq: 1,
            evt: "e1".into(),
            kind: "user_message".into(),
            payload_schema: 1,
            payload: json!({"text": "hello"}),
            refs: vec![],
            snapshot: Some(Digest::new(b"snap")),
        },
        Envelope {
            env: ENVELOPE_SCHEMA,
            seq: 2,
            evt: "e2".into(),
            kind: "tool_result".into(),
            payload_schema: 1,
            payload: json!({"tool": "read_file", "ok": true}),
            refs: vec![present],
            snapshot: None,
        },
        Envelope {
            env: ENVELOPE_SCHEMA,
            seq: 3,
            evt: "e3".into(),
            kind: "tool_result".into(),
            payload_schema: 1,
            payload: json!({"tool": "read_file", "ok": false}),
            refs: vec![missing],
            snapshot: None,
        },
        Envelope {
            env: ENVELOPE_SCHEMA,
            seq: 4,
            evt: "e4".into(),
            kind: "future_kind".into(),
            payload_schema: 9,
            payload: json!({"mystery": 42}),
            refs: vec![],
            snapshot: None,
        },
    ];
    let mut log = AppendLog::open(&log_path, stream, Arc::clone(&queue)).unwrap();
    log.append(&envs, Profile::Fast).unwrap();
    drop(log);
    (log_path, store, queue, present, missing)
}

fn shutdown(queue: Arc<DurabilityQueue>) {
    Arc::try_unwrap(queue)
        .unwrap_or_else(|_| panic!("durability queue still referenced"))
        .shutdown()
        .unwrap();
}

#[test]
fn public_constants() {
    assert_eq!(PROJECTION_SCHEMA, 1);
    assert_eq!(TX_BATCH, 1000);
}

#[test]
fn reconstruction_report_matches_s6_fixture() {
    let dir = tmp_dir("report");
    let (log_path, store, queue, _present, missing) = fixture_log(&dir, "demo", "report");

    let rep = reconstruct(&log_path, &registry(), &store).unwrap();
    assert_eq!(rep.events, 4);
    assert_eq!(rep.kinds.len(), 3);

    let um = &rep.kinds["user_message"];
    assert_eq!((um.schema, um.count, um.upcasted, um.opaque), (1, 1, 1, 0));
    assert!(um.opaque_reason.is_none());

    let tr = &rep.kinds["tool_result"];
    assert_eq!((tr.schema, tr.count, tr.upcasted, tr.opaque), (1, 2, 2, 0));

    let fk = &rep.kinds["future_kind"];
    assert_eq!((fk.schema, fk.count, fk.upcasted, fk.opaque), (9, 1, 0, 1));
    assert_eq!(
        fk.opaque_reason.as_deref(),
        Some("no upcaster for kind 'future_kind' schema 9")
    );

    assert_eq!(rep.missing_objects, vec![missing.to_string()]);
    assert!(rep.upcast_errors.is_empty());

    drop(store);
    shutdown(queue);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rebuild_writes_projection_rows_and_watermark() {
    let dir = tmp_dir("rebuild");
    let (log_path, store, queue, present, _missing) = fixture_log(&dir, "demo", "rebuild");
    let db_path = dir.join("projection.sqlite");

    let rep = rebuild(&log_path, &db_path, &registry(), &store).unwrap();
    assert_eq!(rep.events, 4);

    let conn = Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 4);

    let last_seq: i64 = conn
        .query_row(
            "SELECT last_seq FROM watermarks WHERE stream = 'demo'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(last_seq, 4);

    let refs: String = conn
        .query_row("SELECT refs FROM events WHERE seq = 2", [], |r| r.get(0))
        .unwrap();
    assert_eq!(refs, format!("[\"{present}\"]"));
    let refs_empty: String = conn
        .query_row("SELECT refs FROM events WHERE seq = 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(refs_empty, "[]");

    let snap: Option<String> = conn
        .query_row("SELECT snapshot FROM events WHERE seq = 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(snap, Some(Digest::new(b"snap").to_string()));
    let snap_null: Option<String> = conn
        .query_row("SELECT snapshot FROM events WHERE seq = 2", [], |r| r.get(0))
        .unwrap();
    assert_eq!(snap_null, None);

    let payload: String = conn
        .query_row("SELECT payload FROM events WHERE seq = 1", [], |r| r.get(0))
        .unwrap();
    assert_eq!(payload, "{\"text\":\"hello\"}");
    let kind: String = conn
        .query_row("SELECT kind FROM events WHERE seq = 4", [], |r| r.get(0))
        .unwrap();
    assert_eq!(kind, "future_kind");
    let schema: i64 = conn
        .query_row("SELECT payload_schema FROM events WHERE seq = 4", [], |r| r.get(0))
        .unwrap();
    assert_eq!(schema, 9);
    drop(conn);

    drop(store);
    shutdown(queue);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rebuild_discards_stale_projection_state() {
    let dir = tmp_dir("rebuild-twice");
    let (log_path, store, queue, _present, _missing) = fixture_log(&dir, "demo", "rebuild-twice");
    let db_path = dir.join("projection.sqlite");
    let reg = registry();

    // pre-existing stale projection: wrong schema, extra rows, stale watermark
    let stale = Connection::open(&db_path).unwrap();
    stale
        .execute_batch(
            "CREATE TABLE events (seq INTEGER PRIMARY KEY, junk TEXT);
             INSERT INTO events (seq, junk) VALUES (1, 'x'), (2, 'y'), (3, 'z');
             CREATE TABLE watermarks (stream TEXT PRIMARY KEY, last_seq INTEGER NOT NULL);
             INSERT INTO watermarks VALUES ('stale', 999);",
        )
        .unwrap();
    drop(stale);

    let first = rebuild(&log_path, &db_path, &reg, &store).unwrap();
    assert_eq!(first.events, 4);

    // second rebuild into the same file: schema dropped and recreated
    let second = rebuild(&log_path, &db_path, &reg, &store).unwrap();
    assert_eq!(second.events, 4);

    let conn = Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 4);
    let streams: i64 = conn
        .query_row("SELECT COUNT(*) FROM watermarks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(streams, 1);
    let last_seq: i64 = conn
        .query_row("SELECT last_seq FROM watermarks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(last_seq, 4);
    drop(conn);

    drop(store);
    shutdown(queue);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn tampered_middle_frame_is_explicit_corruption() {
    let dir = tmp_dir("tamper");
    let queue = Arc::new(DurabilityQueue::start("kb-proj-tamper-queue"));
    let log_path = dir.join("events.log.zst");
    let mut log = AppendLog::open(&log_path, "demo", Arc::clone(&queue)).unwrap();

    // three frames; remember each frame's byte range in the file
    let mut ranges: Vec<(u64, u64)> = Vec::new();
    for frame in 0..3 {
        let envs: Vec<Envelope> = (0..4)
            .map(|i| {
                let seq = (frame * 4 + i + 1) as u64;
                Envelope {
                    env: ENVELOPE_SCHEMA,
                    seq,
                    evt: format!("evt{seq}"),
                    kind: "user_message".into(),
                    payload_schema: 1,
                    payload: json!({"text": format!("frame {frame} event {i}")}),
                    refs: vec![],
                    snapshot: None,
                }
            })
            .collect();
        let plan = log.append(&envs, Profile::Fast).unwrap();
        let end = std::fs::metadata(&log_path).unwrap().len();
        ranges.push((end - plan.frame_len, plan.frame_len));
    }
    drop(log);

    // flip a byte inside the middle frame's compressed payload (past the
    // zstd header, so frame boundaries still scan) — the content checksum
    // makes the corruption explicit
    let (start, len) = ranges[1];
    let mut bytes = std::fs::read(&log_path).unwrap();
    let mid = (start + len / 2) as usize;
    assert!(mid > start as usize + 16, "flip must be past the zstd header");
    bytes[mid] ^= 0xFF;
    std::fs::write(&log_path, &bytes).unwrap();

    let store = ObjectStore::open(&dir.join("objects"), Arc::clone(&queue)).unwrap();
    let err = reconstruct(&log_path, &registry(), &store).unwrap_err();
    match err {
        RebuildError::Log(RecoveryError::Corruption { frame, .. }) => assert_eq!(frame, 1),
        other => panic!("expected Corruption at frame 1, got {other:?}"),
    }

    drop(store);
    shutdown(queue);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Hand-roll one M1 frame (kanbei-log format: metadata JSONL line + event
/// lines, zstd level 3 with checksum and pledged size, digest over the
/// canonical bytes) containing raw event lines, bypassing AppendLog's
/// envelope validation — needed to put an envelope that fails kernel
/// validation (env != 1) into the log.
fn write_raw_frame(path: &Path, stream: &str, first_seq: u64, lines: &[String]) {
    use kanbei_log::{hex, new_prev, Meta, SCHEMA};
    use std::io::Write;
    use zstd::stream::write::Encoder;

    let last_seq = first_seq + lines.len() as u64 - 1;
    let mut meta = Meta {
        stream: stream.into(),
        schema: SCHEMA,
        first_seq,
        last_seq,
        count: lines.len() as u64,
        prev: hex(&new_prev()),
        digest: String::new(),
        created_us: 0,
    };
    let canonical = {
        let meta_no_digest = serde_json::json!({
            "stream": meta.stream,
            "schema": meta.schema,
            "first_seq": meta.first_seq,
            "last_seq": meta.last_seq,
            "count": meta.count,
            "prev": meta.prev,
            "created_us": meta.created_us,
        });
        let mut out = serde_json::to_vec(&meta_no_digest).unwrap();
        out.push(b'\n');
        for l in lines {
            out.extend_from_slice(l.as_bytes());
            out.push(b'\n');
        }
        out
    };
    meta.digest = hex(Digest::new(&canonical).as_bytes());
    let mut meta_json = serde_json::to_vec(&meta).unwrap();
    meta_json.push(b'\n');

    let mut enc = Encoder::new(Vec::new(), 3).unwrap();
    enc.include_checksum(true).unwrap();
    enc.set_pledged_src_size(Some(
        (meta_json.len() + lines.iter().map(|l| l.len() + 1).sum::<usize>()) as u64,
    ))
    .unwrap();
    enc.write_all(&meta_json).unwrap();
    for l in lines {
        enc.write_all(l.as_bytes()).unwrap();
        enc.write_all(b"\n").unwrap();
    }
    let frame = enc.finish().unwrap();
    std::fs::write(path, &frame).unwrap();
}

#[test]
fn invalid_envelope_is_invalid_input_naming_seq() {
    let dir = tmp_dir("bad-env");
    let log_path = dir.join("events.log.zst");
    // seq 1, env 99: the frame chain and digests verify, then the envelope
    // fails kernel validation
    write_raw_frame(
        &log_path,
        "demo",
        1,
        &[r#"{"env":99,"seq":1,"evt":"e1","kind":"user_message","schema":1,"payload":{"text":"hi"},"refs":[],"snapshot":null}"#.to_string()],
    );

    let queue = Arc::new(DurabilityQueue::start("kb-proj-bad-env-queue"));
    let store = ObjectStore::open(&dir.join("objects"), Arc::clone(&queue)).unwrap();
    let err = reconstruct(&log_path, &registry(), &store).unwrap_err();
    match err {
        RebuildError::InvalidInput(msg) => {
            assert!(msg.contains("seq 1"), "message must name the seq: {msg}")
        }
        other => panic!("expected InvalidInput, got {other:?}"),
    }

    drop(store);
    shutdown(queue);
    let _ = std::fs::remove_dir_all(&dir);
}

fn upcast_boom(_: &Value) -> Result<Value, String> {
    Err("boom".into())
}

#[test]
fn reconstruction_mixed_schemas_upcast_through_chain() {
    let dir = tmp_dir("chain");
    let queue = Arc::new(DurabilityQueue::start("kb-proj-chain-queue"));
    let log_path = dir.join("events.log.zst");
    let mut reg = Registry::new();
    reg.register("user_message", 1, upcast_user_message_v1_to_v2)
        .unwrap();
    reg.register("user_message", 2, upcast_user_message_v2_to_v3)
        .unwrap();
    // v2 -> v3 is idempotent on v3 payloads, so v3 records stay upcasted
    reg.register("user_message", 3, upcast_user_message_v2_to_v3)
        .unwrap();
    let mut log = AppendLog::open(&log_path, "demo", Arc::clone(&queue)).unwrap();
    log.append(
        &[
            Envelope {
                env: ENVELOPE_SCHEMA,
                seq: 1,
                evt: "e1".into(),
                kind: "user_message".into(),
                payload_schema: 1,
                payload: json!({"text": "hello"}),
                refs: vec![],
                snapshot: None,
            },
            Envelope {
                env: ENVELOPE_SCHEMA,
                seq: 2,
                evt: "e2".into(),
                kind: "user_message".into(),
                payload_schema: 3,
                payload: json!({"text": "hi", "role": "user", "channel": "default"}),
                refs: vec![],
                snapshot: None,
            },
            Envelope {
                env: ENVELOPE_SCHEMA,
                seq: 3,
                evt: "e3".into(),
                kind: "future_kind".into(),
                payload_schema: 9,
                payload: json!({"mystery": 42}),
                refs: vec![],
                snapshot: None,
            },
        ],
        Profile::Fast,
    )
    .unwrap();
    drop(log);

    let store = ObjectStore::open(&dir.join("objects"), Arc::clone(&queue)).unwrap();
    let rep = reconstruct(&log_path, &reg, &store).unwrap();
    assert_eq!(rep.events, 3);
    assert_eq!(rep.kinds.len(), 2);

    // the v1 record upcasts v1 -> v2 -> v3 to the v3 shape, the v3 record
    // upcasts in place: 2/2 upcasted, nothing opaque
    let um = &rep.kinds["user_message"];
    assert_eq!((um.schema, um.count, um.upcasted, um.opaque), (3, 2, 2, 0));
    assert!(um.opaque_reason.is_none());

    let fk = &rep.kinds["future_kind"];
    assert_eq!((fk.schema, fk.count, fk.upcasted, fk.opaque), (9, 1, 0, 1));
    assert_eq!(
        fk.opaque_reason.as_deref(),
        Some("no upcaster for kind 'future_kind' schema 9")
    );

    assert!(rep.missing_objects.is_empty());
    assert!(rep.upcast_errors.is_empty());

    drop(store);
    shutdown(queue);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn upcast_error_is_reported_not_fatal() {
    let dir = tmp_dir("upcast-err");
    let queue = Arc::new(DurabilityQueue::start("kb-proj-upcast-err-queue"));
    let log_path = dir.join("events.log.zst");
    let mut reg = Registry::new();
    reg.register("boom_kind", 1, upcast_boom).unwrap();
    let mut log = AppendLog::open(&log_path, "demo", Arc::clone(&queue)).unwrap();
    log.append(
        &[Envelope {
            env: ENVELOPE_SCHEMA,
            seq: 1,
            evt: "e1".into(),
            kind: "boom_kind".into(),
            payload_schema: 1,
            payload: json!({}),
            refs: vec![],
            snapshot: None,
        }],
        Profile::Fast,
    )
    .unwrap();
    drop(log);

    let store = ObjectStore::open(&dir.join("objects"), Arc::clone(&queue)).unwrap();
    let rep = reconstruct(&log_path, &reg, &store).unwrap();
    assert!(
        rep.upcast_errors
            .iter()
            .any(|s| s.contains("boom_kind") && s.contains("boom")),
        "upcast_errors: {:?}",
        rep.upcast_errors
    );
    let st = &rep.kinds["boom_kind"];
    assert_eq!((st.count, st.upcasted, st.opaque), (1, 0, 1));
    assert_eq!(st.opaque_reason.as_deref(), Some("boom"));

    drop(store);
    shutdown(queue);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rebuild_streams_200k_events() {
    let dir = tmp_dir("stream-200k");
    let queue = Arc::new(DurabilityQueue::start("kb-proj-stream-200k-queue"));
    let log_path = dir.join("events.log.zst");
    let mut log = AppendLog::open(&log_path, "demo", Arc::clone(&queue)).unwrap();
    let mut seq = 1u64;
    while seq <= 200_000 {
        let batch: Vec<Envelope> = (0..64)
            .map(|i| {
                let s = seq + i;
                Envelope {
                    env: ENVELOPE_SCHEMA,
                    seq: s,
                    evt: format!("evt{s}"),
                    kind: "user_message".into(),
                    payload_schema: 1,
                    payload: json!({"text": format!("hello {s}")}),
                    refs: vec![],
                    snapshot: None,
                }
            })
            .collect();
        log.append(&batch, Profile::Fast).unwrap();
        seq += 64;
    }
    drop(log);

    let store = ObjectStore::open(&dir.join("objects"), Arc::clone(&queue)).unwrap();
    let db_path = dir.join("projection.sqlite");
    let rep = rebuild(&log_path, &db_path, &registry(), &store).unwrap();
    assert_eq!(rep.events, 200_000);

    let conn = Connection::open(&db_path).unwrap();
    let count: i64 = conn
        .query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 200_000);
    let last_seq: i64 = conn
        .query_row("SELECT last_seq FROM watermarks", [], |r| r.get(0))
        .unwrap();
    assert_eq!(last_seq, 200_000);
    drop(conn);

    drop(store);
    shutdown(queue);
    let _ = std::fs::remove_dir_all(&dir);
}
