//! The deterministic crash-test child (contract in kanbei-testkit's lib):
//! commits `KANBEI_CRASH_EVENTS` events with `KANBEI_CRASH_OBJECTS` object
//! payloads each, acks each commit on stdout, and aborts (SIGABRT, no
//! destructors) at the configured fault point once armed — after the
//! `KANBEI_CRASH_AFTER_ACKS`-th commit returns. `KANBEI_CRASH_POINT=none`
//! (default) never fires and the child completes normally.

use std::io::Write;
use std::path::PathBuf;
use std::process::exit;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kanbei_core::digest::Digest;
use kanbei_log::Profile;
use kanbei_session::{FaultInjector, FaultPoint, NewEvent, Session, SessionConfig};
use kanbei_testkit::{
    ENV_AFTER_ACKS, ENV_DIR, ENV_EVENTS, ENV_OBJECTS, ENV_POINT, ENV_PROFILE, ENV_STATE_EVERY,
    parse_fault_point,
};
use serde_json::json;

struct AbortInjector {
    point: Option<FaultPoint>,
    armed: AtomicBool,
}

impl FaultInjector for AbortInjector {
    fn inject(&self, point: FaultPoint) {
        if self.armed.load(Ordering::SeqCst) && self.point == Some(point) {
            std::process::abort();
        }
    }
}

fn env_u64(key: &str, default: u64) -> u64 {
    std::env::var(key)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(default)
}

fn ack(line: String) {
    let mut out = std::io::stdout();
    let _ = writeln!(out, "{line}");
    let _ = out.flush();
}

fn main() {
    let dir = match std::env::var(ENV_DIR) {
        Ok(d) => d,
        Err(_) => {
            eprintln!("crash-child: {ENV_DIR} is required");
            exit(2);
        }
    };
    let point = std::env::var(ENV_POINT).ok().and_then(|s| parse_fault_point(&s));
    let after_acks = env_u64(ENV_AFTER_ACKS, 4);
    let events = env_u64(ENV_EVENTS, 8);
    let objects = env_u64(ENV_OBJECTS, 2);
    let profile = Profile::from(&std::env::var(ENV_PROFILE).unwrap_or_else(|_| "fast".into()));
    let state_every = env_u64(ENV_STATE_EVERY, 1);

    let injector = Arc::new(AbortInjector { point, armed: AtomicBool::new(false) });
    let arm_handle = Arc::clone(&injector);
    let mut session = match Session::open(SessionConfig {
        dir: PathBuf::from(&dir),
        stream: "crash".into(),
        profile,
        fault: Some(injector),
        ..Default::default()
    }) {
        Ok(s) => s,
        Err(e) => {
            ack(format!("commit_error=open: {e}"));
            exit(2);
        }
    };

    if after_acks == 0 {
        arm_handle.armed.store(true, Ordering::SeqCst);
    }
    for i in 1..=events {
        let ev = NewEvent {
            kind: "test_event".into(),
            payload_schema: 1,
            payload: json!({"i": i}),
            objects: (0..objects).map(|j| format!("object-{i}-{j}").into_bytes()).collect(),
            refs: vec![],
        };
        let state_head = if state_every > 0 && i % state_every == 0 {
            Some(Digest::new(format!("state-{i}").as_bytes()))
        } else {
            None
        };
        match session.commit(vec![ev], state_head) {
            Ok(rec) => {
                if rec.last_seq == after_acks {
                    arm_handle.armed.store(true, Ordering::SeqCst);
                }
                ack(format!("acked={}", rec.last_seq));
            }
            Err(e) => {
                ack(format!("commit_error={e}"));
                exit(2);
            }
        }
    }
    ack("done".into());
    exit(0);
}
