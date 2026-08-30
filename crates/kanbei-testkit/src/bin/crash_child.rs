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
    ENV_AFTER_ACKS, ENV_DIR, ENV_EVENTS, ENV_M2_FLOW, ENV_M3_FLOW, ENV_MODE, ENV_OBJECTS,
    ENV_POINT, ENV_PROFILE, ENV_STATE_EVERY, parse_fault_point,
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

/// The memory-actor half of the injector: aborts at the transition/head
/// seams of a committed proposal (same armed-flag contract as the session
/// injector).
struct MemoryAbortInjector {
    point: Option<kanbei_memory::MemoryFaultPoint>,
    armed: AtomicBool,
}

impl kanbei_memory::MemoryFaultInjector for MemoryAbortInjector {
    fn inject(&self, point: kanbei_memory::MemoryFaultPoint) {
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
    let point = std::env::var(ENV_POINT)
        .ok()
        .and_then(|s| parse_fault_point(&s));
    let after_acks = env_u64(ENV_AFTER_ACKS, 4);
    let events = env_u64(ENV_EVENTS, 8);
    let objects = env_u64(ENV_OBJECTS, 2);
    let profile = Profile::from(&std::env::var(ENV_PROFILE).unwrap_or_else(|_| "fast".into()));
    let state_every = env_u64(ENV_STATE_EVERY, 1);
    let mode = std::env::var(ENV_MODE).unwrap_or_else(|_| "m1".into());

    match mode.as_str() {
        "m1" => run_m1(
            dir,
            point,
            after_acks,
            events,
            objects,
            profile,
            state_every,
        ),
        "m2" => run_m2(dir, point, after_acks, events, profile),
        "m3" => run_m3(dir, point, after_acks),
        "m4" => run_m4(dir, after_acks),
        "m5" => run_m5(dir, after_acks),
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
    let injector = Arc::new(AbortInjector {
        point,
        armed: AtomicBool::new(false),
    });
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
            objects: (0..objects)
                .map(|j| format!("object-{i}-{j}").into_bytes())
                .collect(),
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
    let injector = Arc::new(AbortInjector {
        point,
        armed: AtomicBool::new(false),
    });
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
        match session.commit(
            vec![ev],
            Some(Digest::new(format!("m2-state-{i}").as_bytes())),
        ) {
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

/// The M3 flow: open a session with a fake provider, arm after `after_acks`
/// plain commits, then run the agent spine — wake accept, run start, model
/// call, tool call, run outcome — aborting at `point` once armed.
fn run_m3(dir: String, point: Option<FaultPoint>, after_acks: u64) {
    use kanbei_capabilities::{Grant, GrantScope, Principal, TrustClass};
    use kanbei_provider::{CompletionResponse, FakeEngine, FinishReason, ProviderConfig, Usage};
    use kanbei_scheduler::{BreakerFloors, Budgets, TerminalOutcome, Trigger, TriggerKind};
    use std::sync::Arc;

    let flow = std::env::var(ENV_M3_FLOW).unwrap_or_else(|_| "spine".into());
    let injector = Arc::new(AbortInjector {
        point,
        armed: AtomicBool::new(false),
    });
    let arm_handle = Arc::clone(&injector);
    // Wake/run points arm after the AFTER_ACKS commits; the spine runs then.
    // Points that fire during open (none in M3) would arm before open.
    let arm_after_open = matches!(
        point,
        Some(
            FaultPoint::BeforeWakeAccept
                | FaultPoint::AfterWakeAccept
                | FaultPoint::BeforeRunStart
                | FaultPoint::AfterRunStart
                | FaultPoint::BeforeModelCall
                | FaultPoint::AfterModelCall
                | FaultPoint::BeforeToolIntentCommit
                | FaultPoint::AfterToolIntentCommit
                | FaultPoint::BeforeToolDispatch
                | FaultPoint::AfterToolDispatch
                | FaultPoint::BeforeToolOutcomeCommit
                | FaultPoint::AfterToolOutcomeCommit
                | FaultPoint::BeforeRunOutcome
                | FaultPoint::AfterRunOutcome
        )
    );

    // Grant everything to generation 0 (the spine principal) so tool calls
    // dispatch without parking in the approval queue.
    let session_id = Id128::generate();
    let mut broker = kanbei_capabilities::Broker::new();
    broker
        .add_template(kanbei_capabilities::PolicyTemplate {
            trust_class: TrustClass::Builtin,
            allow: vec![kanbei_capabilities::Capability::new(
                "fs.read".into(),
                vec!["call".into()],
            )],
            deny: vec![],
            require_approval: vec![],
            version: 1,
            monotonic: true,
        })
        .unwrap();
    let mut grant = Grant {
        grant_digest: kanbei_core::digest::Digest::new(b"placeholder"),
        principal: Principal {
            session: session_id,
            generation: 0,
            run: None,
        },
        module_generation: 0,
        capability: kanbei_capabilities::Capability::new("fs.read".into(), vec!["call".into()]),
        scope: GrantScope::Session,
        expiry: None,
        budget: None,
        purpose: Some("crash-test".into()),
        policy_version: 1,
    };
    grant.grant_digest = grant.derive_digest();
    broker.add_grant(grant).unwrap();

    let fake = FakeEngine::new(
        ProviderConfig {
            provider: "fake".into(),
            model: "test".into(),
            base_url: "http://localhost:0/v1".into(),
            key: kanbei_provider::KeySource::Env("KANBEI_TEST_KEY".into()),
            temperature: None,
            max_tokens: Some(10),
            timeout: std::time::Duration::from_secs(5),
        },
        vec![CompletionResponse {
            content: Some("hello".into()),
            tool_calls: vec![],
            finish_reason: FinishReason::Stop,
            usage: Usage {
                input_tokens: 3,
                output_tokens: 2,
            },
        }],
    );

    let mut session = match Session::open(SessionConfig {
        dir: PathBuf::from(&dir),
        stream: "crash-m3".into(),
        profile: Profile::Fast,
        fault: Some(injector),
        provider: Some(kanbei_provider::ProviderConfig {
            provider: "fake".into(),
            model: "test".into(),
            base_url: "http://localhost:0/v1".into(),
            key: kanbei_provider::KeySource::Env("KANBEI_TEST_KEY".into()),
            temperature: None,
            max_tokens: Some(10),
            timeout: std::time::Duration::from_secs(5),
        }),
        provider_engine: Some(Box::new(fake)),
        budgets: Budgets {
            deadline_secs: Some(60),
            ..Default::default()
        },
        breaker_floors: BreakerFloors::default(),
        broker,
        session_id: Some(session_id),
        ..Default::default()
    }) {
        Ok(s) => s,
        Err(e) => {
            ack(format!("commit_error=open: {e}"));
            exit(2);
        }
    };

    // AFTER_ACKS plain commits (M1 event shape) with the injector disarmed.
    for i in 1..=after_acks {
        let ev = NewEvent {
            kind: "test_event".into(),
            payload_schema: 1,
            payload: json!({"i": i}),
            objects: vec![],
            refs: vec![],
        };
        match session.commit(vec![ev], None) {
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
    if after_acks == 0 {
        arm_handle.armed.store(true, Ordering::SeqCst);
    }

    if flow == "wake" {
        // Wake acceptance only: exercises Before/AfterWakeAccept.
        session.observe_trigger(Trigger {
            kind: TriggerKind::NewCausalEvent,
            referent: None,
        });
        match session.accept_wake() {
            Ok(_) => ack(format!("acked={}", session.next_seq() - 1)),
            Err(e) => {
                ack(format!("wake_error={e}"));
                exit(2);
            }
        }
        ack("done".into());
        exit(0);
    }

    // Spine: wake → run start → model call → tool call → run outcome,
    // acking after every commit so the ack-coverage invariant tracks each
    // frame (each crash point fires inside its owning commit, leaving the
    // in-flight frame as the only unacked events).
    session.observe_trigger(Trigger {
        kind: TriggerKind::NewCausalEvent,
        referent: None,
    });
    let run = match session.accept_wake() {
        Ok(Some(r)) => r,
        Ok(None) => {
            ack("wake_error=denied".into());
            exit(2);
        }
        Err(e) => {
            ack(format!("wake_error={e}"));
            exit(2);
        }
    };
    ack(format!("acked={}", session.next_seq() - 1));
    if let Err(e) = session.run_start(run.run_id) {
        ack(format!("run_start_error={e}"));
        exit(2);
    }
    ack(format!("acked={}", session.next_seq() - 1));

    // Model call: intent + outcome commit in one frame.
    let messages = vec![kanbei_provider::Message {
        role: kanbei_provider::Role::User,
        content: "hello".into(),
        tool_call_id: None,
    }];
    match session.model_call(run.run_id, messages, vec![], "hello") {
        Ok(_) => ack(format!("acked={}", session.next_seq() - 1)),
        Err(e) => {
            ack(format!("model_error={e}"));
            exit(2);
        }
    }

    // Tool call: intent commit, then outcome commit (separate frames).
    let principal = Principal {
        session: session_id,
        generation: 0,
        run: Some(0),
    };
    let outcome = match session.tool_call(
        run.run_id,
        principal,
        "fs.read",
        json!({"path": "README.md"}),
    ) {
        Ok(o) => o,
        Err(e) => {
            ack(format!("tool_error={e}"));
            exit(2);
        }
    };
    ack(format!("acked={}", session.next_seq() - 1));
    if let Err(e) = session.commit_tool_outcome(&outcome) {
        ack(format!("outcome_error={e}"));
        exit(2);
    }
    ack(format!("acked={}", session.next_seq() - 1));

    // Run outcome.
    let usage = session.scheduler_usage(run.run_id);
    match session.run_outcome(run.run_id, TerminalOutcome::CompletedGoal, usage, &[]) {
        Ok(_) => ack(format!("acked={}", session.next_seq() - 1)),
        Err(e) => {
            ack(format!("run_outcome_error={e}"));
            exit(2);
        }
    }
    ack("done".into());
    exit(0);
}

/// The M4 flow: open a session with the memory substrate + a project
/// binding and an approval-requiring broker for memory.propose, arm both
/// injectors after `after_acks` plain commits, then drive the propose flow —
/// wake accept, run start, memory.propose intent, proposal, approval,
/// transition (the actor's commit path), backlink, tool outcome — aborting
/// at the configured point once armed. `point=none` completes with "done".
fn run_m4(dir: String, after_acks: u64) {
    use kanbei_capabilities::{Grant, GrantScope, PolicyTemplate, Principal, TrustClass};
    use kanbei_scheduler::{Budgets, Trigger, TriggerKind};
    use kanbei_testkit::parse_memory_fault_point;

    // The ENV_POINT string decides which injector owns the point: the four
    // memory strings wire into the memory injector (they collide with the
    // session's module-head point names by design — the mode decides), the
    // two memory-proposal points and anything else wire into the session
    // injector.
    let point_str = std::env::var(ENV_POINT).unwrap_or_else(|_| "none".into());
    let memory_point = parse_memory_fault_point(&point_str);
    let session_point = if memory_point.is_none() {
        parse_fault_point(&point_str)
    } else {
        None
    };
    let session_injector = Arc::new(AbortInjector {
        point: session_point,
        armed: AtomicBool::new(false),
    });
    let memory_injector = Arc::new(MemoryAbortInjector {
        point: memory_point,
        armed: AtomicBool::new(false),
    });
    let session_arm = Arc::clone(&session_injector);
    let memory_arm = Arc::clone(&memory_injector);

    // Broker: memory.propose is approval-gated for the session principal.
    let session_id = Id128::generate();
    let mut broker = kanbei_capabilities::Broker::new();
    broker
        .add_template(PolicyTemplate {
            trust_class: TrustClass::Builtin,
            allow: vec![kanbei_capabilities::Capability::new(
                "memory.propose".into(),
                vec!["call".into()],
            )],
            deny: vec![],
            require_approval: vec![kanbei_capabilities::Capability::new(
                "memory.propose".into(),
                vec!["call".into()],
            )],
            version: 1,
            monotonic: true,
        })
        .unwrap();
    let mut grant = Grant {
        grant_digest: Digest::new(b"placeholder"),
        principal: Principal {
            session: session_id,
            generation: 0,
            run: None,
        },
        module_generation: 0,
        capability: kanbei_capabilities::Capability::new(
            "memory.propose".into(),
            vec!["call".into()],
        ),
        scope: GrantScope::Session,
        expiry: None,
        budget: None,
        purpose: Some("crash-test".into()),
        policy_version: 1,
    };
    grant.grant_digest = grant.derive_digest();
    broker.add_grant(grant).unwrap();

    let project_id = Id128::generate();
    let mut session = match Session::open(SessionConfig {
        dir: PathBuf::from(&dir),
        stream: "crash-m4".into(),
        profile: Profile::Fast,
        fault: Some(session_injector),
        memory_root: Some(PathBuf::from(&dir).join("memory")),
        project: Some(project_id),
        memory_fault: Some(memory_injector),
        broker,
        session_id: Some(session_id),
        budgets: Budgets {
            deadline_secs: Some(60),
            ..Default::default()
        },
        ..Default::default()
    }) {
        Ok(s) => s,
        Err(e) => {
            ack(format!("commit_error=open: {e}"));
            exit(2);
        }
    };

    // AFTER_ACKS plain commits with both injectors disarmed; arm after the
    // after_acks-th commit returns (the memory flow runs after setup).
    for i in 1..=after_acks {
        let ev = NewEvent {
            kind: "test_event".into(),
            payload_schema: 1,
            payload: json!({"i": i}),
            objects: vec![],
            refs: vec![],
        };
        match session.commit(vec![ev], None) {
            Ok(rec) => {
                if i == after_acks {
                    session_arm.armed.store(true, Ordering::SeqCst);
                    memory_arm.armed.store(true, Ordering::SeqCst);
                }
                ack(format!("acked={}", rec.last_seq));
            }
            Err(e) => {
                ack(format!("commit_error={e}"));
                exit(2);
            }
        }
    }
    if after_acks == 0 {
        session_arm.armed.store(true, Ordering::SeqCst);
        memory_arm.armed.store(true, Ordering::SeqCst);
    }

    // Propose flow: wake → run start → memory.propose (intent, proposal,
    // approval, transition, backlink) → tool outcome, acking after every
    // commit so the ack-coverage invariant tracks each frame.
    session.observe_trigger(Trigger {
        kind: TriggerKind::NewCausalEvent,
        referent: None,
    });
    let run = match session.accept_wake() {
        Ok(Some(r)) => r,
        Ok(None) => {
            ack("wake_error=denied".into());
            exit(2);
        }
        Err(e) => {
            ack(format!("wake_error={e}"));
            exit(2);
        }
    };
    ack(format!("acked={}", session.next_seq() - 1));
    if let Err(e) = session.run_start(run.run_id) {
        ack(format!("run_start_error={e}"));
        exit(2);
    }
    ack(format!("acked={}", session.next_seq() - 1));

    let principal = Principal {
        session: session_id,
        generation: 0,
        run: Some(0),
    };
    let outcome = match session.tool_call(
        run.run_id,
        principal,
        "memory.propose",
        json!({"claim": {"kind": "decision", "content": "m4 crash claim"}}),
    ) {
        Ok(o) => o,
        Err(e) => {
            ack(format!("tool_error={e}"));
            exit(2);
        }
    };
    ack(format!("acked={}", session.next_seq() - 1));
    if let Err(e) = session.commit_tool_outcome(&outcome) {
        ack(format!("outcome_error={e}"));
        exit(2);
    }
    ack(format!("acked={}", session.next_seq() - 1));
    ack("done".into());
    exit(0);
}

/// The M5 flow: open a session (modules enabled), activate the built-in UI
/// (an atomic composition publish), feed input through the kernel boundary
/// (char reduce + enter → canonical `user_message`), render a frame —
/// aborting at the configured UI point once armed. `point=none` completes
/// with "done".
fn run_m5(dir: String, after_acks: u64) {
    use kanbei_scheduler::Budgets;
    use kanbei_vm::VmConfig;
    let session_injector = Arc::new(AbortInjector {
        point: parse_fault_point(&std::env::var(ENV_POINT).unwrap_or_else(|_| "none".into())),
        armed: AtomicBool::new(false),
    });
    let arm = Arc::clone(&session_injector);
    let mut session = match Session::open(SessionConfig {
        dir: PathBuf::from(&dir),
        stream: "crash-m5".into(),
        profile: Profile::Fast,
        fault: Some(session_injector),
        budgets: Budgets {
            deadline_secs: Some(60),
            ..Default::default()
        },
        engine: Some(VmConfig {
            fuel_per_call: u64::MAX,
            epoch_deadline: u64::MAX / 2,
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

    // AFTER_ACKS plain commits with the injector disarmed; arm after the
    // after_acks-th commit returns (the UI flow runs after setup).
    for i in 1..=after_acks {
        let ev = NewEvent {
            kind: "test_event".into(),
            payload_schema: 1,
            payload: json!({"i": i}),
            objects: vec![],
            refs: vec![],
        };
        match session.commit(vec![ev], None) {
            Ok(rec) => {
                if i == after_acks {
                    arm.armed.store(true, Ordering::SeqCst);
                }
                ack(format!("acked={}", rec.last_seq));
            }
            Err(e) => {
                ack(format!("commit_error={e}"));
                exit(2);
            }
        }
    }
    if after_acks == 0 {
        arm.armed.store(true, Ordering::SeqCst);
    }

    // Activate the built-in UI (composition_changed; ack after the commit).
    match session.activate_builtin_ui() {
        Ok(_epoch) => ack(format!("acked={}", session.next_seq() - 1)),
        Err(e) => {
            ack(format!("ui_error=activate: {e}"));
            exit(2);
        }
    }

    // Type + submit through the kernel boundary: reduces (crash points),
    // then a canonical user_message on enter (acked).
    if let Err(e) = session.ui_handle_input(b"hello\n") {
        ack(format!("ui_error=input: {e}"));
        exit(2);
    }
    ack(format!("acked={}", session.next_seq() - 1));

    // Render a frame (Before/AfterUiRender points).
    if let Err(e) = session.ui_render_frame() {
        ack(format!("ui_error=render: {e}"));
        exit(2);
    }
    ack("done".into());
    exit(0);
}
