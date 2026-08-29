//! AppendLog integration tests: roundtrip, byte parity with the frozen S3
//! spike, torn-tail truncation, mid-file corruption, chain verify, write
//! amplification, strict profile, and envelope validation on append.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use kanbei_core::envelope::Envelope;
use kanbei_core::queue::DurabilityQueue;
use kanbei_core::ENVELOPE_SCHEMA;
use kanbei_log::{for_each_frame, recover, scan_frames, AppendLog, Meta, Profile, RecoveryError};
use serde_json::json;

fn env(seq: u64) -> Envelope {
    Envelope {
        env: ENVELOPE_SCHEMA,
        seq,
        evt: format!("evt{seq}"),
        kind: "user_message".into(),
        payload_schema: 1,
        payload: json!({"text": format!("hello {seq}")}),
        refs: vec![],
        snapshot: None,
    }
}

fn tmp(tag: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("kb-log-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_file(&p);
    p
}

fn queue(name: &str) -> Arc<DurabilityQueue> {
    Arc::new(DurabilityQueue::start(name))
}

fn shutdown(q: Arc<DurabilityQueue>) {
    match Arc::try_unwrap(q) {
        Ok(q) => q.shutdown().unwrap(),
        Err(_) => panic!("durability queue still referenced at test shutdown"),
    }
}

/// The frozen encoding call (spike s3-appendlog, level 3, checksum, pledged
/// content size). zstd is deterministic: re-encoding reproduces the bytes.
fn encode(content: &[u8]) -> Vec<u8> {
    let mut enc = zstd::stream::write::Encoder::new(Vec::new(), 3).unwrap();
    enc.include_checksum(true).unwrap();
    enc.set_pledged_src_size(Some(content.len() as u64)).unwrap();
    enc.write_all(content).unwrap();
    enc.finish().unwrap()
}

fn decode_first_frame(path: &Path) -> (Meta, Vec<String>) {
    let (boundaries, _) = scan_frames(path).unwrap();
    assert_eq!(boundaries.len(), 1);
    let (start, len) = boundaries[0];
    let mut buf = vec![0u8; len as usize];
    let mut f = File::open(path).unwrap();
    f.seek(SeekFrom::Start(start)).unwrap();
    f.read_exact(&mut buf).unwrap();
    let content = zstd::stream::decode_all(&buf[..]).unwrap();
    let text = String::from_utf8(content).unwrap();
    let mut lines = text.lines();
    let meta: Meta = serde_json::from_str(lines.next().unwrap()).unwrap();
    let events: Vec<String> = lines.map(String::from).collect();
    (meta, events)
}

#[test]
fn roundtrip_100_events() {
    let path = tmp("roundtrip");
    let q = queue("kb-log-rt");
    let mut log = AppendLog::open(&path, "demo", Arc::clone(&q)).unwrap();
    let all: Vec<Envelope> = (1..=100).map(env).collect();
    let mut last = 0u64;
    for chunk in all.chunks(8) {
        let plan = log.append(chunk, Profile::Fast).unwrap();
        assert_eq!(plan.first_seq, last + 1);
        assert_eq!(plan.last_seq, last + chunk.len() as u64);
        assert_eq!(plan.count, chunk.len() as u64);
        assert!(plan.frame_len > 0);
        last = plan.last_seq;
    }
    assert_eq!(log.frames(), 13);
    assert_eq!(log.seq(), 101);
    drop(log);
    shutdown(q);

    let rec = recover(&path).unwrap();
    assert_eq!(rec.events, 100);
    assert_eq!(rec.frames, 13);
    assert!(!rec.truncated);
    assert_eq!(rec.last_seq, 100);

    // seq continuity of recovered events
    let mut expect = 1u64;
    for_each_frame(&path, |info| {
        assert_eq!(info.meta.first_seq, expect);
        for (i, line) in info.events.iter().enumerate() {
            let v: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(v["seq"].as_u64().unwrap(), expect + i as u64);
        }
        expect = info.meta.last_seq + 1;
    })
    .unwrap();
    assert_eq!(expect, 101);
}

#[test]
fn byte_parity_with_frozen_spike() {
    let path_a = tmp("parity-spike");
    let path_b = tmp("parity-kanbei");
    let q = queue("kb-log-parity");
    let stream = "demo";
    let batch: Vec<Envelope> = (1..=8).map(env).collect();
    let lines: Vec<String> = batch.iter().map(Envelope::to_line).collect();

    // spike side: same event LINE strings through the frozen LogWriter
    let mut spike = kb_s3_appendlog::LogWriter::open(&path_a, stream).unwrap();
    spike.append_frame(&lines, kb_s3_appendlog::Profile::Fast).unwrap();
    drop(spike);

    let mut log = AppendLog::open(&path_b, stream, Arc::clone(&q)).unwrap();
    log.append(&batch, Profile::Fast).unwrap();
    drop(log);

    // both files: one complete frame, no torn tail
    let (ba, ta) = scan_frames(&path_a).unwrap();
    let (bb, tb) = scan_frames(&path_b).unwrap();
    assert_eq!(ba.len(), 1);
    assert_eq!(bb.len(), 1);
    assert!(!ta);
    assert!(!tb);

    // decoded content differs only in the spec'd fields: kanbei's seq base is
    // 1 (envelope validate enforces seq >= 1) vs the spike's 0, so
    // first_seq/last_seq/digest shift; created_us is a timestamp
    let (meta_a, events_a) = decode_first_frame(&path_a);
    let (meta_b, events_b) = decode_first_frame(&path_b);
    assert_eq!(events_a, events_b, "event lines must be identical");
    assert_eq!(meta_a.stream, meta_b.stream);
    assert_eq!(meta_a.schema, meta_b.schema);
    assert_eq!(meta_a.count, meta_b.count);
    assert_eq!(meta_a.prev, meta_b.prev);
    assert_eq!(meta_a.first_seq + 1, meta_b.first_seq);
    assert_eq!(meta_a.last_seq + 1, meta_b.last_seq);
    assert_ne!(meta_a.digest, meta_b.digest);

    // both sides self-verify (spike via the spike's own recovery, kanbei via
    // ours — including the envelope-seq check)
    kb_s3_appendlog::recover(&path_a).unwrap();
    recover(&path_b).unwrap();

    // encoding parity: re-encoding the decoded content with the frozen call
    // reproduces the written frames byte-for-byte, so both writers are
    // byte-identical encoders of their content
    let frame_a = std::fs::read(&path_a).unwrap();
    let frame_b = std::fs::read(&path_b).unwrap();
    let content_a = zstd::stream::decode_all(&frame_a[..]).unwrap();
    let content_b = zstd::stream::decode_all(&frame_b[..]).unwrap();
    assert_eq!(encode(&content_a), frame_a, "spike frame must be byte-exact");
    assert_eq!(encode(&content_b), frame_b, "kanbei frame must be byte-exact");
    shutdown(q);
}

#[test]
fn torn_tail_truncates_and_resumes() {
    let path = tmp("torn");
    let q = queue("kb-log-torn");
    let mut log = AppendLog::open(&path, "demo", Arc::clone(&q)).unwrap();
    for chunk in (1..=100).map(env).collect::<Vec<_>>().chunks(8) {
        log.append(chunk, Profile::Fast).unwrap();
    }
    log.flush().unwrap();
    drop(log);

    let (boundaries, truncated) = scan_frames(&path).unwrap();
    assert!(!truncated);
    assert_eq!(boundaries.len(), 13);
    let (last_start, last_len) = *boundaries.last().unwrap();
    let cut = last_start + last_len / 2;
    let f = File::options().write(true).open(&path).unwrap();
    f.set_len(cut).unwrap();
    drop(f);

    let rec = recover(&path).unwrap();
    assert!(rec.truncated);
    assert_eq!(rec.events, 96);
    assert_eq!(rec.frames, 12);
    assert_eq!(rec.last_seq, 96);
    // exact torn-tail truncation: file ends at the last good offset
    assert_eq!(std::fs::metadata(&path).unwrap().len(), last_start);

    // reopen: seq and chain continue from the last complete frame
    let mut log = AppendLog::open(&path, "demo", Arc::clone(&q)).unwrap();
    assert_eq!(log.seq(), 97);
    let batch: Vec<Envelope> = (97..=100).map(env).collect();
    let plan = log.append(&batch, Profile::Fast).unwrap();
    assert_eq!(plan.first_seq, 97);
    drop(log);

    let rec = recover(&path).unwrap();
    assert!(!rec.truncated);
    assert_eq!(rec.events, 100);
    assert_eq!(rec.frames, 13);
    assert_eq!(rec.last_seq, 100);
    shutdown(q);
}

#[test]
fn mid_file_corruption_detected() {
    let path = tmp("corrupt");
    let q = queue("kb-log-corrupt");
    let mut log = AppendLog::open(&path, "demo", Arc::clone(&q)).unwrap();
    for chunk in (1..=100).map(env).collect::<Vec<_>>().chunks(8) {
        log.append(chunk, Profile::Fast).unwrap();
    }
    log.flush().unwrap();
    drop(log);

    let (boundaries, _) = scan_frames(&path).unwrap();
    assert_eq!(boundaries.len(), 13);
    let (target_start, _) = boundaries[6];
    let target = target_start + 10;
    let mut f = File::options().read(true).write(true).open(&path).unwrap();
    f.seek(SeekFrom::Start(target)).unwrap();
    let mut byte = [0u8; 1];
    f.read_exact(&mut byte).unwrap();
    f.seek(SeekFrom::Start(target)).unwrap();
    f.write_all(&[byte[0] ^ 0xff]).unwrap();
    drop(f);

    match recover(&path) {
        Err(RecoveryError::Corruption { frame, offset, reason }) => {
            assert_eq!(frame, 6);
            assert_eq!(offset, target_start);
            assert!(offset > 0);
            assert!(!reason.is_empty());
        }
        _ => panic!("expected corruption, got a clean recovery"),
    }
    shutdown(q);
}

#[test]
fn chain_verify_100_frames() {
    let path = tmp("chain");
    let q = queue("kb-log-chain");
    let mut log = AppendLog::open(&path, "demo", Arc::clone(&q)).unwrap();
    for chunk in (1..=800).map(env).collect::<Vec<_>>().chunks(8) {
        log.append(chunk, Profile::Fast).unwrap();
    }
    log.flush().unwrap();
    drop(log);

    let mut seen = 0u64;
    let mut events = 0u64;
    let rec = for_each_frame(&path, |info| {
        seen += 1;
        events += info.events.len() as u64;
        assert_eq!(info.meta.count, info.events.len() as u64);
    })
    .unwrap();
    assert_eq!(rec.frames, 100);
    assert_eq!(rec.events, 800);
    assert_eq!(rec.last_seq, 800);
    assert!(!rec.truncated);
    assert_eq!(seen, 100);
    assert_eq!(events, 800);

    // tamper one event line in frame 50: decode the frame, modify one event,
    // re-encode with the frozen call, and rebuild the file tail so the frames
    // after the tamper stay contiguous (the frame's own header may resolve to
    // a different length once its content changed)
    let (boundaries, _) = scan_frames(&path).unwrap();
    let (start, len) = boundaries[50];
    let whole = std::fs::read(&path).unwrap();
    let frame50 = &whole[start as usize..(start + len) as usize];
    let content = zstd::stream::decode_all(frame50).unwrap();
    let text = String::from_utf8(content).unwrap();
    let mut lines: Vec<String> = text.split('\n').map(String::from).collect();
    let mut v: serde_json::Value = serde_json::from_str(&lines[2]).unwrap();
    v["payload"]["text"] = json!("TAMPERED");
    lines[2] = serde_json::to_string(&v).unwrap();
    let tampered = format!("{}\n", lines[..9].join("\n"));
    let tampered_frame = encode(tampered.as_bytes());

    let mut rebuilt = Vec::new();
    rebuilt.extend_from_slice(&whole[..start as usize]);
    rebuilt.extend_from_slice(&tampered_frame);
    rebuilt.extend_from_slice(&whole[(start + len) as usize..]);
    let mut f = File::create(&path).unwrap();
    f.write_all(&rebuilt).unwrap();
    drop(f);

    match for_each_frame(&path, |_| {}) {
        Err(RecoveryError::Corruption { frame, offset, .. }) => {
            assert_eq!(frame, 50);
            assert_eq!(offset, start);
        }
        _ => panic!("expected corruption, got a clean verify"),
    }
    shutdown(q);
}

#[test]
fn write_amplification_below_raw() {
    let path = tmp("amp");
    let q = queue("kb-log-amp");
    let mut log = AppendLog::open(&path, "demo", Arc::clone(&q)).unwrap();
    let all: Vec<Envelope> = (1..=512).map(env).collect();
    let raw: usize = all.iter().map(|e| e.to_line().len() + 1).sum();
    for chunk in all.chunks(64) {
        log.append(chunk, Profile::Fast).unwrap();
    }
    log.flush().unwrap();
    drop(log);

    let file_size = std::fs::metadata(&path).unwrap().len();
    assert!(file_size < raw as u64, "file {file_size} bytes >= raw JSONL {raw} bytes");
    shutdown(q);
}

#[test]
fn strict_flushes_before_return() {
    let path = tmp("strict");
    let q = queue("kb-log-strict");
    let mut log = AppendLog::open(&path, "demo", Arc::clone(&q)).unwrap();
    for chunk in (1..=40).map(env).collect::<Vec<_>>().chunks(8) {
        let plan = log.append(chunk, Profile::Strict).unwrap();
        assert_eq!(plan.last_seq, chunk.last().unwrap().seq);
    }
    drop(log);
    shutdown(q);

    // every acked event readable after a fresh open
    let rec = recover(&path).unwrap();
    assert_eq!(rec.events, 40);
    assert_eq!(rec.frames, 5);
    assert_eq!(rec.last_seq, 40);
    assert!(!rec.truncated);
}

#[test]
fn append_rejects_bad_sequences_and_envelopes() {
    let path = tmp("validate");
    let q = queue("kb-log-validate");
    let mut log = AppendLog::open(&path, "demo", Arc::clone(&q)).unwrap();

    // empty batch
    let err = log.append(&[], Profile::Fast).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(log.seq(), 1);

    // first event must equal the next expected seq (1)
    let err = log.append(&[env(2)], Profile::Fast).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(log.seq(), 1);

    // discontinuity inside the batch
    let err = log.append(&[env(1), env(2), env(4)], Profile::Fast).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(log.seq(), 1);

    // envelope failing validate: seq 0
    let err = log.append(&[Envelope { seq: 0, ..env(1) }], Profile::Fast).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(log.seq(), 1);

    // envelope failing validate: empty kind
    let err = log.append(&[Envelope { kind: String::new(), ..env(1) }], Profile::Fast).unwrap_err();
    assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    assert_eq!(log.seq(), 1);

    // nothing was written by the rejected appends
    drop(log);
    shutdown(q);
    let rec = recover(&path).unwrap();
    assert_eq!(rec.events, 0);
    assert_eq!(rec.frames, 0);
    assert!(!rec.truncated);
}
