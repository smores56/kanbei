#![cfg(feature = "otel")]
#![allow(clippy::result_large_err)]

//! M8 wave 1 gate: the optional OTel-compatible telemetry — correlation
//! spans over the canonical run/commit/checkpoint/transition records and
//! storage gauges, all derived from canonical records + filesystem
//! observations (M7 principle). Runs only with `--features otel`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use kanbei_core::id::Id128;
use kanbei_scheduler::{RunUsage, TerminalOutcome, Trigger, TriggerKind};
use kanbei_session::{NewEvent, Session, SessionConfig};
use kanbei_telemetry::{AttrValue, FileSink, Telemetry};
use serde_json::json;

struct TempRoot(PathBuf);

impl TempRoot {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kb-session-telemetry-{tag}-{}-{}",
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

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn user_message(text: &str) -> NewEvent {
    NewEvent {
        kind: "user_message".into(),
        payload_schema: 1,
        payload: json!({"text": text}),
        objects: vec![],
        refs: vec![],
    }
}

/// Standard base64 with padding (mirror of the emitter's encoder; the
/// emitter's own correctness is pinned by RFC vectors in its unit tests).
fn b64(data: &[u8]) -> String {
    const ALPHA: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let n = ((chunk[0] as u32) << 16)
            | ((chunk.get(1).copied().unwrap_or(0) as u32) << 8)
            | chunk.get(2).copied().unwrap_or(0) as u32;
        out.push(ALPHA[((n >> 18) & 63) as usize] as char);
        out.push(ALPHA[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHA[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHA[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

/// One scripted run through the public API: wake acceptance, run_start, two
/// commits, run_outcome, a checkpoint, a branch, and the explicit storage
/// report — then the exported file must carry the correlation + gauges.
#[test]
fn otel_correlation_and_storage_reporting() {
    let root = TempRoot::new("correl");
    let export = root.path().join("telemetry.jsonl");
    let session_id = Id128::generate();
    let mut session = Session::open(SessionConfig {
        dir: root.path().to_path_buf(),
        stream: "otel-test".into(),
        session_id: Some(session_id),
        telemetry: Some(Telemetry::new(Arc::new(FileSink::new(&export)))),
        ..Default::default()
    })
    .unwrap();

    // One scripted cognition run (no provider — the run FSM + commit path
    // need none).
    session.observe_trigger(Trigger {
        kind: TriggerKind::NewCausalEvent,
        referent: None,
    });
    let run = session.accept_wake().unwrap().expect("wake accepted");
    let run_id = run.run_id;
    let started_secs = session.scheduler_usage(run_id).started_at_secs;
    session.run_start(run_id).unwrap();
    let r1 = session.commit(vec![user_message("hello")], None).unwrap();
    let r2 = session.commit(vec![user_message("world")], None).unwrap();

    let trip = session
        .run_outcome(
            run_id,
            TerminalOutcome::Progress,
            RunUsage {
                tokens: 120,
                tools: 2,
                children: 0,
                started_at_secs: started_secs,
            },
            &[],
        )
        .unwrap();
    assert!(trip.is_none(), "no breaker trip expected");

    let cp = session.create_checkpoint(Some("otel-gate".into())).unwrap();
    let branch = session.continue_from(&cp).unwrap();

    // Canonical ground truth (before close consumes the session).
    let committed_seq = session.next_seq() - 1;
    let objects_count = session.store().scan().unwrap().len();
    let log_bytes = std::fs::metadata(session.log_path()).unwrap().len();
    let transition_seq = branch.transition_seq;
    assert_eq!(committed_seq, transition_seq, "frontier == transition seq");
    let _ = r1;

    session.report_storage().unwrap();
    session.close().unwrap();

    // ---- exported payloads ----
    let exported = std::fs::read_to_string(&export).unwrap();
    let lines: Vec<&str> = exported.lines().collect();
    assert!(lines.len() >= 12, "exported {} payloads", lines.len());
    let spans: Vec<&str> = lines
        .iter()
        .filter(|l| l.contains("resourceSpans"))
        .copied()
        .collect();
    let metrics: Vec<&str> = lines
        .iter()
        .filter(|l| l.contains("resourceMetrics"))
        .copied()
        .collect();

    // ---- the run span: trace = session id, span = run id, SERVER ----
    let run_span = spans
        .iter()
        .find(|l| l.contains(r#""name":"run""#))
        .expect("run span exported");
    assert!(run_span.contains(&format!(
        r#""traceId":"{}""#,
        b64(session_id.as_bytes())
    )));
    assert!(run_span.contains(r#""kind":2"#));
    assert!(run_span.contains(&format!(
        r#""spanId":"{}""#,
        b64(&run_id.as_bytes()[..8])
    )));
    assert!(run_span.contains(&format!(
        r#""startTimeUnixNano":"{}""#,
        started_secs * 1_000_000_000
    )));
    assert!(run_span.contains(&format!(
        r#""key":"run_id","value":{{"stringValue":"{run_id}"}}"#
    )));
    assert!(run_span.contains(&format!(
        r#""key":"session_id","value":{{"stringValue":"{session_id}"}}"#
    )));
    assert!(run_span.contains(r#""key":"started_at_secs","value":{"intValue":""#));
    assert!(run_span.contains(r#""status":{"code":1,"message":"Progress"}"#));
    assert!(run_span.contains(r#""key":"tokens","value":{"intValue":"120"}"#));

    // ---- commit spans: parented to the run, attrs = canonical receipt ----
    let run_span_id = b64(&run_id.as_bytes()[..8]);
    let commit_spans: Vec<&str> = spans
        .iter()
        .filter(|l| l.contains(r#""name":"commit""#))
        .copied()
        .collect();
    assert!(commit_spans.len() >= 6, "commit spans per committed frame");
    let parented = commit_spans
        .iter()
        .filter(|c| c.contains(&format!(r#""parentSpanId":"{run_span_id}""#)))
        .count();
    assert!(
        parented >= 4,
        "run_start + message + run_outcome commits parented to the run span (got {parented})"
    );
    let last_commit = commit_spans
        .iter()
        .find(|l| {
            l.contains(&format!(
                r#""key":"last_seq","value":{{"intValue":"{}"}}"#,
                r2.last_seq
            ))
        })
        .expect("commit span of the last commit");
    assert!(last_commit.contains(&format!(
        r#""key":"first_seq","value":{{"intValue":"{}"}}"#,
        r2.first_seq
    )));
    assert!(last_commit.contains(&format!(
        r#""key":"count","value":{{"intValue":"{}"}}"#,
        r2.count
    )));
    assert!(last_commit.contains(&format!(
        r#""key":"frame_len","value":{{"intValue":"{}"}}"#,
        r2.frame_len
    )));
    assert!(last_commit.contains(r#""key":"objects","value":{"intValue":"0"}"#));

    // ---- checkpoint + continue_from spans ----
    let cp_span = spans
        .iter()
        .find(|l| l.contains(r#""name":"checkpoint""#))
        .expect("checkpoint span exported");
    assert!(cp_span.contains(&format!(r#""key":"seq","value":{{"intValue":"{}"}}"#, cp.seq)));
    // A fresh session pins no lifetime memory root (the canonical payload's
    // memory_root is null too) — the attr appears only when one exists.
    assert!(!cp_span.contains("memory_root"));
    let cf_span = spans
        .iter()
        .find(|l| l.contains(r#""name":"continue_from""#))
        .expect("continue_from span exported");
    assert!(cf_span.contains(&format!(
        r#""key":"branch","value":{{"stringValue":"{}"}}"#,
        branch.id
    )));
    assert!(cf_span.contains(&format!(
        r#""key":"transition_seq","value":{{"intValue":"{transition_seq}"}}"#
    )));

    // ---- storage gauges: present, numeric, canonical-valued ----
    for name in [
        "kanbei.objects.count",
        "kanbei.objects.bytes",
        "kanbei.log.bytes",
        "kanbei.log.seq",
        "kanbei.projection.bytes",
    ] {
        assert!(
            metrics
                .iter()
                .any(|l| l.contains(&format!(r#""name":"{name}""#))),
            "metric {name} exported"
        );
    }
    // The final report (last metrics payload) is the post-branch state.
    let final_log_seq = metrics
        .iter()
        .rev()
        .find(|l| l.contains(r#""name":"kanbei.log.seq""#))
        .expect("log.seq metric");
    assert!(final_log_seq.contains(&format!("\"asInt\":\"{committed_seq}\"")));
    assert!(final_log_seq.contains(&format!(
        r#""key":"session_id","value":{{"stringValue":"{session_id}"}}"#
    )));
    let final_count = metrics
        .iter()
        .rev()
        .find(|l| l.contains(r#""name":"kanbei.objects.count""#))
        .expect("objects.count metric");
    assert!(final_count.contains(&format!("\"asInt\":\"{objects_count}\"")));
    let final_log_bytes = metrics
        .iter()
        .rev()
        .find(|l| l.contains(r#""name":"kanbei.log.bytes""#))
        .expect("log.bytes metric");
    assert!(final_log_bytes.contains(&format!("\"asInt\":\"{log_bytes}\"")));
}
