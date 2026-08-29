//! The deterministic crash-test child (contract in kanbei-testkit's lib):
//! commits `KANBEI_CRASH_EVENTS` events with `KANBEI_CRASH_OBJECTS` object
//! payloads each, acks each commit on stdout, and aborts (SIGABRT, no
//! destructors) at the configured fault point once armed — after the
//! `KANBEI_CRASH_AFTER_ACKS`-th commit returns. `KANBEI_CRASH_POINT=none`
//! (default) never fires and the child completes normally.
//!
//! `KANBEI_CRASH_MODE=m2` (default "m1" keeps the M1 protocol byte-identical)
//! runs the M2 flow instead: the session opens with a config module publishing
//! `svc.greet`, `AFTER_ACKS` plain events are committed disarmed, then the
//! injector arms and the M2 seams run — `effect_dispatch` when
//! `KANBEI_CRASH_M2_FLOW` contains "dispatch" (the config generation is the
//! caller; the host rejects the self-call by contract, so the pre-dispatch
//! point fires and the post-dispatch point cannot), then two
//! `module_state_cas` head updates. Config-activation points arm before open
//! (they fire inside open's `activate_config`); dispatch/head points arm
//! after the `AFTER_ACKS`-th commit.

use std::io::Write;
use std::path::PathBuf;
use std::process::exit;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kanbei_capabilities::TrustClass;
use kanbei_core::digest::Digest;
use kanbei_core::id::Id128;
use kanbei_log::Profile;
use kanbei_modules::{ModuleOrigin, PackageManifest};
use kanbei_services::{ScopePath, ServiceKey};
use kanbei_session::{FaultInjector, FaultPoint, NewEvent, Session, SessionConfig};
use kanbei_testkit::{
    ENV_AFTER_ACKS, ENV_DIR, ENV_EVENTS, ENV_M2_FLOW, ENV_MODE, ENV_OBJECTS, ENV_POINT, ENV_PROFILE,
    ENV_STATE_EVERY, parse_fault_point,
};
use kanbei_vm::VmConfig;
use serde_json::json;

/// The M2 config module: publishes `svc.greet` v1 (the m2.rs shape; host op 6
/// payload is the canonical ServiceKey + contract version + deps JSON).
const CONFIG_SOURCE: &str = r#"
function kb_on_activate(ctx)
  ctx.service_publish('{"scope":[],"name":"greet"}', 1, '[]')
end
function kb_hot(x) return { from = "greet", got = x } end
"#;

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
    let mode = std::env::var(ENV_MODE).unwrap_or_else(|_| "m1".into());

    match mode.as_str() {
        "m1" => run_m1(dir, point, after_acks, events, objects, profile, state_every),
        "m2" => run_m2(dir, point, after_acks, events, profile),
        other => {
            eprintln!("crash-child: unknown {ENV_MODE}={other:?}");
            exit(2);
        }
    }
}

/// The M1 protocol: open, commit `events` with `objects` object payloads each,
/// arm after the `after_acks`-th ack, abort at `point` if configured.
fn run_m1(
    dir: String,
    point: Option<FaultPoint>,
    after_acks: u64,
    events: u64,
    objects: u64,
    profile: Profile,
    state_every: u64,
) {
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

/// The M2 flow: open with a config module publishing `svc.greet`, commit
/// `AFTER_ACKS` plain events disarmed, arm, then run the dispatch (when the
/// flow asks) and two head CAS updates. `point=none` completes with "done".
fn run_m2(dir: String, point: Option<FaultPoint>, after_acks: u64, events: u64, profile: Profile) {
    let flow = std::env::var(ENV_M2_FLOW).unwrap_or_else(|_| "head".into());
    let injector = Arc::new(AbortInjector { point, armed: AtomicBool::new(false) });
    let arm_handle = Arc::clone(&injector);
    // Config points fire inside open's activate_config — arm before open.
    // Dispatch/head points fire after the AFTER_ACKS commits — arm after.
    let arm_after_open = matches!(
        point,
        Some(
            FaultPoint::BeforeEffectDispatch
                | FaultPoint::AfterEffectDispatch
                | FaultPoint::BeforeHeadUpdate
                | FaultPoint::AfterHeadUpdate
        )
    );
    if !arm_after_open {
        arm_handle.armed.store(true, Ordering::SeqCst);
    }

    let manifest = PackageManifest {
        schema: 1,
        module_id: Id128::generate(),
        origin: ModuleOrigin::Builtin,
        trust_class: TrustClass::Builtin,
        scope: ScopePath(vec![]),
        deps: vec![],
        capabilities: vec![],
        source: CONFIG_SOURCE.into(),
        state_schema: None,
    };
    let mut session = match Session::open(SessionConfig {
        dir: PathBuf::from(&dir),
        stream: "crash-m2".into(),
        profile,
        fault: Some(injector),
        config: Some(manifest),
        // No fuel/epoch limits: the flow must not trap before the crash point.
        engine: Some(VmConfig {
            fuel_per_call: u64::MAX,
            epoch_deadline: u64::MAX,
            ..Default::default()
        }),
        ..Default::default()
    }) {
        Ok(s) => s,
        Err(e) => {
            ack(format!("commit_error=open: {e}"));
            exit(2);
        }
    };
    // The config generation id (open() runs activate_config internally and
    // swallows the ConfigActivation; the manager snapshot is the same id).
    let Some(generation) = session
        .modules()
        .and_then(|m| m.snapshot().first().map(|(_, g, _)| *g))
    else {
        ack("commit_error=modules disabled after open".into());
        exit(2);
    };

    // AFTER_ACKS plain commits (M1 event shape) with the injector disarmed.
    for i in 1..=events {
        let ev = NewEvent {
            kind: "test_event".into(),
            payload_schema: 1,
            payload: json!({"i": i}),
            objects: vec![format!("m2-object-{i}").into_bytes()],
            refs: vec![],
        };
        match session.commit(vec![ev], Some(Digest::new(format!("m2-state-{i}").as_bytes()))) {
            Ok(rec) => {
                if rec.last_seq == after_acks && arm_after_open {
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

    // Dispatch flow: the config generation is the caller by contract — the
    // host rejects the self-call (re-entrant instance lock), so the
    // BeforeEffectDispatch point fires and AfterEffectDispatch cannot.
    if flow.contains("dispatch") {
        let key = ServiceKey {
            scope: ScopePath(vec![]),
            name: "greet".into(),
        };
        match session.effect_dispatch(&key, "{}", generation) {
            Ok(_) => ack("dispatch=ok".into()),
            Err(e) => ack(format!("dispatch_error={e}")),
        }
    }

    // Two module-state head CAS updates (Before/AfterHeadUpdate fire here).
    for bytes in [b"1".as_slice(), b"2".as_slice()] {
        match session.module_state_cas("counter", 1, bytes.to_vec(), generation) {
            Ok(head) => ack(format!("cas={}", head.seq)),
            Err(e) => {
                ack(format!("cas_error={e}"));
                exit(2);
            }
        }
    }
    ack("done".into());
    exit(0);
}
