//! T9 hook-seam integration tests (session half): decision enforcement at the
//! turn boundary and the tool-intent gate, the fail-closed/degrade fault
//! policy (canonical `module_fault` facts, respawn + rebind), and the
//! canonical `turn_denied`/`Denied` outcome paths.
//!
//! A missing guest is a hard failure: build it with `cargo xtask build-guest`
//! from the workspace root first.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use kanbei_capabilities::TrustClass;
use kanbei_core::envelope::Envelope;
use kanbei_core::id::Id128;
use kanbei_log::for_each_frame;
use kanbei_modules::package::ModuleOrigin;
use kanbei_modules::PackageManifest;
use kanbei_scheduler::{
    CognitionProvider, RunId, StepCommand, StepContext, StepError, Trigger, TriggerKind,
};
use kanbei_session::{Session, SessionConfig};
use kanbei_tools::OutcomeClassification;
use kanbei_vm::{GuestError, Vm, VmConfig};
use serde_json::json;

// --- helpers ---------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kb-hooks-{tag}-{}-{}",
            std::process::id(),
            Id128::generate()
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

/// Fuel generous enough for the activation shim but small enough that an
/// intentional infinite loop in a hook exhausts it (the modules trap recipe).
fn trap_engine() -> VmConfig {
    VmConfig {
        fuel_per_call: 20_000_000,
        epoch_deadline: u64::MAX,
        ..Default::default()
    }
}

fn require_guest() {
    match Vm::load(trap_engine()) {
        Ok(_) => {}
        Err(GuestError::NotBuilt) => {
            panic!("guest wasm not built: run `cargo xtask build-guest` from the workspace root")
        }
        Err(e) => panic!("Vm::load failed: {e}"),
    }
}

fn manifest(id: Id128, source: &str) -> PackageManifest {
    manifest_origin(id, source, ModuleOrigin::UserConfig)
}

fn manifest_origin(id: Id128, source: &str, origin: ModuleOrigin) -> PackageManifest {
    PackageManifest {
        schema: kanbei_modules::PACKAGE_SCHEMA,
        module_id: id,
        origin,
        trust_class: TrustClass::User,
        scope: kanbei_services::ScopePath(vec!["root".into()]),
        deps: Vec::new(),
        capabilities: Vec::new(),
        source: source.to_string(),
        state_schema: None,
        state_key: None,
    }
}

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

fn facts<'a>(envs: &'a [Envelope], kind: &str) -> Vec<&'a Envelope> {
    envs.iter().filter(|e| e.kind == kind).collect()
}

fn open_with(source: &str) -> (TempDir, Session) {
    require_guest();
    let dir = TempDir::new("session");
    let m = manifest(Id128::generate(), source);
    let session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        engine: Some(trap_engine()),
        config_layers: vec![m],
        ..Default::default()
    })
    .unwrap();
    (dir, session)
}

fn start_run(session: &mut Session) -> (RunId, Trigger) {
    session.observe_trigger(Trigger {
        kind: TriggerKind::NewCausalEvent,
        referent: None,
    });
    let run = session.accept_wake().unwrap().unwrap();
    session.run_start(run.run_id).unwrap();
    (run.run_id, run.trigger)
}

/// A provider that plays a fixed script, recording how many steps it ran.
struct Scripted {
    commands: std::collections::VecDeque<StepCommand>,
    steps: u32,
}

impl Scripted {
    fn new(commands: Vec<StepCommand>) -> Self {
        Self {
            commands: commands.into(),
            steps: 0,
        }
    }
}

impl CognitionProvider for Scripted {
    fn step(
        &mut self,
        _context: &StepContext,
        _trigger: &Trigger,
        _last: Option<&kanbei_scheduler::StepResult>,
    ) -> Result<StepCommand, StepError> {
        self.steps += 1;
        Ok(self
            .commands
            .pop_front()
            .unwrap_or(StepCommand::Finish(kanbei_scheduler::TerminalOutcome::Progress)))
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// --- fixtures --------------------------------------------------------------

const TRAP_TURN: &str = r#"
kb_name = "trap_turn"
function kb_on_activate(ctx) end
function kb_hot(x) return "hot" end
function kb_on_turn_start(context)
  local n = 0
  while true do n = n + 1 end
end
"#;

const TRAP_TOOL: &str = r#"
kb_name = "trap_tool"
function kb_on_activate(ctx) end
function kb_hot(x) return "hot" end
function kb_on_tool_intent(context)
  local n = 0
  while true do n = n + 1 end
end
"#;

const DENY_TURN: &str = r#"
kb_name = "deny_turn"
function kb_on_activate(ctx) end
function kb_hot(x) return "hot" end
function kb_on_turn_start(context)
  return { decision = "deny", reason = "no thinking today" }
end
"#;

const DENY_TOOL: &str = r#"
kb_name = "deny_tool"
function kb_on_activate(ctx) end
function kb_hot(x) return "hot" end
function kb_on_tool_intent(context)
  return { decision = "deny", reason = "rm is not allowed" }
end
"#;

const ANNOTATE_TURN: &str = r#"
kb_name = "annotate_turn"
function kb_on_activate(ctx) end
function kb_hot(x) return "hot" end
function kb_on_turn_start(context)
  return { decision = "continue", annotations = { { key = "risk", value = "low" } } }
end
"#;

const WEDGE_TOOL: &str = r#"
kb_name = "wedge_tool"
function kb_on_activate(ctx) end
function kb_hot(x) return "hot" end
function kb_on_tool_intent(context)
  return kb_host_call(1, '{"key":"planner"}')
end
"#;

/// Traps only the first time; the state marker survives respawn, so the fresh
/// generation's hook returns `continue`.
const RETRY_TOOL: &str = r#"
kb_name = "retry_tool"
function kb_on_activate(ctx) end
function kb_hot(x) return "hot" end
function kb_on_tool_intent(context)
  local r = kb_host_call(1, '{"key":"retry_tool.seen"}')
  if string.find(r, '"value":null') then
    kb_host_call(2, '{"key":"retry_tool.seen","schema":1,"value":true}')
    local n = 0
    while true do n = n + 1 end
  end
  return { decision = "continue" }
end
"#;

const FAIL_OPEN_KEYS: [&str; 6] = [
    "module_id",
    "package_digest",
    "generation",
    "hook",
    "fault_class",
    "count",
];

// --- tests -----------------------------------------------------------------

/// A trapping `on_turn_start` hook degrades to CONTINUE: the run completes and
/// exactly one canonical `module_fault` fact (ids/digests/counts only) is
/// committed.
#[test]
fn turn_start_trap_degrades_to_continue() {
    let (dir, mut session) = open_with(TRAP_TURN);
    let (run_id, trigger) = start_run(&mut session);
    let mut provider = Scripted::new(vec![StepCommand::Finish(
        kanbei_scheduler::TerminalOutcome::Progress,
    )]);
    let outcome = session
        .cognition_loop(run_id, trigger.clone(), &mut provider, |s| {
            s.project_context(run_id, &trigger)
        })
        .unwrap();
    assert_eq!(outcome, kanbei_scheduler::TerminalOutcome::Progress);
    assert_eq!(provider.steps, 1, "continue still runs the provider step");
    session.close().unwrap();

    let envs = envelopes(&dir.path().join("log.zst"));
    let faults = facts(&envs, "module_fault");
    assert_eq!(faults.len(), 1, "one transition fact");
    let payload = &faults[0].payload;
    let keys: Vec<&str> = payload.as_object().unwrap().keys().map(String::as_str).collect();
    assert_eq!(keys.len(), FAIL_OPEN_KEYS.len());
    for k in FAIL_OPEN_KEYS {
        assert!(payload.get(k).is_some(), "missing {k} in {payload}");
    }
    assert_eq!(payload["fault_class"], "trap");
    assert_eq!(payload["hook"], "on_turn_start");
    assert_eq!(payload["count"], 1);
    assert_eq!(payload["generation"], 1);
    assert!(payload["module_id"].as_str().is_some_and(|s| !s.is_empty()));
    assert!(payload["package_digest"].as_str().unwrap().starts_with("blake3:"));
    // No guest text leaks into the fact.
    assert!(payload.get("reason").is_none() && payload.get("text").is_none());
}

/// A trapping `on_tool_intent` hook fails CLOSED: the tool is DENIED by
/// default, one `module_fault` is committed, and the run still completes.
#[test]
fn tool_intent_trap_fails_closed() {
    let (dir, mut session) = open_with(TRAP_TOOL);
    let (run_id, trigger) = start_run(&mut session);
    let principal = kanbei_capabilities::Principal {
        session: session.session_id(),
        generation: 0,
        run: Some(0),
    };
    let mut provider = Scripted::new(vec![
        StepCommand::ToolIntent {
            tool: "fs.read".into(),
            arguments: json!({ "path": "does-not-matter" }),
        },
        StepCommand::Finish(kanbei_scheduler::TerminalOutcome::Progress),
    ]);
    let outcome = session
        .cognition_loop(run_id, trigger.clone(), &mut provider, |s| {
            s.project_context(run_id, &trigger)
        })
        .unwrap();
    let _ = principal;
    assert_eq!(outcome, kanbei_scheduler::TerminalOutcome::Progress);
    session.close().unwrap();

    let envs = envelopes(&dir.path().join("log.zst"));
    let outcomes = facts(&envs, "tool_outcome");
    assert_eq!(outcomes.len(), 1);
    match &outcomes[0].payload["classification"] {
        serde_json::Value::Object(o) => {
            assert!(o.contains_key("Denied"), "expected Denied, got {o:?}")
        }
        other => panic!("unexpected classification shape {other:?}"),
    }
    assert!(outcomes[0].payload["hook_denied"]["module_id"]
        .as_str()
        .is_some_and(|s| !s.is_empty()));
    assert!(outcomes[0].payload["hook_denied"]["package_digest"]
        .as_str()
        .unwrap()
        .starts_with("blake3:"));
    assert_eq!(outcomes[0].payload["hook_denied"]["hook"], "on_tool_intent");
    assert!(outcomes[0].payload["hook_denied"]["decision_digest"]
        .as_str()
        .unwrap()
        .starts_with("blake3:"));
    assert_eq!(facts(&envs, "module_fault").len(), 1);
}

/// A denying `on_turn_start` hook is terminal: `turn_denied` is committed and
/// the run ends Blocked with NO provider step (no model call).
#[test]
fn turn_start_deny_blocks_without_a_model_call() {
    let (dir, mut session) = open_with(DENY_TURN);
    let (run_id, trigger) = start_run(&mut session);
    let mut provider = Scripted::new(vec![StepCommand::Finish(
        kanbei_scheduler::TerminalOutcome::Progress,
    )]);
    let outcome = session
        .cognition_loop(run_id, trigger.clone(), &mut provider, |s| {
            s.project_context(run_id, &trigger)
        })
        .unwrap();
    assert_eq!(outcome, kanbei_scheduler::TerminalOutcome::Blocked);
    assert_eq!(provider.steps, 0, "a denied turn never reaches the provider");
    session.close().unwrap();

    let envs = envelopes(&dir.path().join("log.zst"));
    let denied = facts(&envs, "turn_denied");
    assert_eq!(denied.len(), 1);
    assert_eq!(denied[0].payload["hook"], "on_turn_start");
    assert_eq!(denied[0].payload["run"], run_id.to_string());
    assert!(denied[0].payload["package_digest"]
        .as_str()
        .unwrap()
        .starts_with("blake3:"));
    assert!(denied[0].payload["decision_digest"]
        .as_str()
        .unwrap()
        .starts_with("blake3:"));
    // No raw guest reason in the canonical payload.
    assert!(denied[0].payload.get("reason").is_none());
    // The run ended Blocked (its reason in run_outcome, not turn_denied).
    assert_eq!(facts(&envs, "run_outcome").len(), 1);
}

/// Annotations from an `on_turn_start` hook are committed before the model
/// step sees the turn.
#[test]
fn turn_start_annotations_are_committed() {
    let (dir, mut session) = open_with(ANNOTATE_TURN);
    let (run_id, trigger) = start_run(&mut session);
    let mut provider = Scripted::new(vec![StepCommand::Finish(
        kanbei_scheduler::TerminalOutcome::Progress,
    )]);
    session
        .cognition_loop(run_id, trigger.clone(), &mut provider, |s| {
            s.project_context(run_id, &trigger)
        })
        .unwrap();
    session.close().unwrap();

    let envs = envelopes(&dir.path().join("log.zst"));
    let anns = facts(&envs, "hook_annotation");
    assert_eq!(anns.len(), 1);
    assert_eq!(anns[0].payload["key"], "risk");
    assert_eq!(anns[0].payload["value"], "low");
    assert_eq!(anns[0].payload["run"], run_id.to_string());
}

/// A denying `on_tool_intent` hook short-circuits the broker: a tool that
/// WOULD park behind the approval gate is denied outright and never executes.
#[test]
fn denying_tool_hook_never_consults_the_broker() {
    require_guest();
    let dir = TempDir::new("deny-tool");
    let session_id = Id128::generate();
    let broker = gated_broker(session_id);
    let session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        fs_root: dir.path().to_path_buf(),
        engine: Some(trap_engine()),
        broker,
        session_id: Some(session_id),
        config_layers: vec![manifest(Id128::generate(), DENY_TOOL)],
        ..Default::default()
    })
    .unwrap();
    let mut session = session;
    let (run_id, _trigger) = start_run(&mut session);
    let principal = kanbei_capabilities::Principal {
        session: session_id,
        generation: 0,
        run: Some(0),
    };
    let outcome = session
        .tool_call(
            run_id,
            principal,
            "fs.write",
            json!({ "path": "secret.txt", "content": "s" }),
        )
        .unwrap();
    session.close().unwrap();

    assert!(
        matches!(outcome.classification, OutcomeClassification::Denied(_)),
        "hook deny must classify Denied, got {:?}",
        outcome.classification
    );
    assert!(outcome.hook_denied.is_some());
    assert!(!dir.path().join("secret.txt").exists());
    let envs = envelopes(&dir.path().join("log.zst"));
    assert!(facts(&envs, "tool_intent").len() == 1, "intent still commits (B-05)");
    assert_eq!(facts(&envs, "tool_outcome").len(), 1);
}

/// A wedged hook is bounded by HOOK_WAIT: the decision returns promptly (no
/// multi-second stall) and falls back to the fail-closed default.
#[test]
fn wedged_hook_is_bounded_and_fails_closed() {
    let (dir, mut session) = open_with(WEDGE_TOOL);
    let (run_id, _trigger) = start_run(&mut session);
    // Hold the module's state lock so the hook's host call wedges, then
    // release it shortly after the hook's wait bound so the actor can drain.
    let state = session.modules().unwrap().state();
    let holder = std::thread::spawn(move || {
        let _guard = state.lock().unwrap();
        std::thread::sleep(Duration::from_millis(700));
    });
    std::thread::sleep(Duration::from_millis(50));
    let principal = kanbei_capabilities::Principal {
        session: session.session_id(),
        generation: 0,
        run: Some(0),
    };
    let started = Instant::now();
    let outcome = session
        .tool_call(run_id, principal, "fs.read", json!({ "path": "x" }))
        .unwrap();
    let elapsed = started.elapsed();
    holder.join().unwrap();
    session.close().unwrap();

    assert!(
        matches!(outcome.classification, OutcomeClassification::Denied(_)),
        "wedge must fall back to the fail-closed default, got {:?}",
        outcome.classification
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "the run must not stall: {elapsed:?}"
    );
    let envs = envelopes(&dir.path().join("log.zst"));
    let faults = facts(&envs, "module_fault");
    assert_eq!(faults.len(), 1);
    assert_eq!(faults[0].payload["fault_class"], "timeout");
}

/// After a fault, the module is respawned on a fresh generation and rebound:
/// a hook that trapped once returns its real decision on the retry, and no
/// second fault fact is committed.
#[test]
fn faulted_hook_is_retried_on_a_fresh_generation() {
    let (dir, mut session) = open_with(RETRY_TOOL);
    let module_id = session.modules().unwrap().snapshot()[0].0;
    let (run_id, _trigger) = start_run(&mut session);
    let principal = kanbei_capabilities::Principal {
        session: session.session_id(),
        generation: 0,
        run: Some(0),
    };
    let first = session
        .tool_call(run_id, principal.clone(), "fs.read", json!({ "path": "a" }))
        .unwrap();
    assert!(matches!(first.classification, OutcomeClassification::Denied(_)));
    let generation_after = session
        .modules()
        .unwrap()
        .snapshot()
        .into_iter()
        .find(|(id, _, _)| *id == module_id)
        .unwrap()
        .1;
    assert!(generation_after > 1, "respawn must mint a fresh generation");

    let second = session
        .tool_call(run_id, principal, "fs.read", json!({ "path": "b" }))
        .unwrap();
    session.close().unwrap();

    assert!(
        !matches!(second.classification, OutcomeClassification::Denied(_)),
        "the retried hook must produce its real (continue) decision, got {:?}",
        second.classification
    );
    let envs = envelopes(&dir.path().join("log.zst"));
    assert_eq!(
        facts(&envs, "module_fault").len(),
        1,
        "the fresh generation does not fault"
    );
    assert_eq!(facts(&envs, "tool_intent").len(), 2);
}

// --- review-fix pack: A/B/C/F/G regression tests ----------------------------

fn principal(session: &Session) -> kanbei_capabilities::Principal {
    kanbei_capabilities::Principal {
        session: session.session_id(),
        generation: 0,
        run: Some(0),
    }
}

fn open_origin(source: &str, origin: ModuleOrigin) -> (TempDir, Session) {
    require_guest();
    let dir = TempDir::new("origin");
    let m = manifest_origin(Id128::generate(), source, origin);
    let session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        engine: Some(trap_engine()),
        config_layers: vec![m],
        ..Default::default()
    })
    .unwrap();
    (dir, session)
}

/// A (HIGH): a deny-relevant `on_tool_intent` hook whose generation cannot be
/// respawned (its wait budget is exhausted on the fault) stays DEGRADED and
/// fail-closed — every subsequent intent is DENIED, never allowed, and no
/// per-decision fault storm is emitted.
#[test]
fn unrespawnable_intent_hook_stays_fail_closed_across_decisions() {
    let (dir, mut session) = open_with(WEDGE_TOOL);
    let (run_id, _trigger) = start_run(&mut session);
    let state = session.modules().unwrap().state();
    let holder = std::thread::spawn(move || {
        let _guard = state.lock().unwrap();
        std::thread::sleep(Duration::from_millis(700));
    });
    std::thread::sleep(Duration::from_millis(50));

    let mut outcomes = Vec::new();
    for i in 0..4 {
        let outcome = session
            .tool_call(
                run_id,
                principal(&session),
                "fs.read",
                json!({ "path": format!("p{i}") }),
            )
            .unwrap();
        outcomes.push(outcome);
    }
    holder.join().unwrap();
    session.close().unwrap();

    for (i, o) in outcomes.iter().enumerate() {
        assert!(
            matches!(o.classification, OutcomeClassification::Denied(_)),
            "intent {i} must stay denied, got {:?}",
            o.classification
        );
    }
    let envs = envelopes(&dir.path().join("log.zst"));
    let faults = facts(&envs, "module_fault");
    assert_eq!(faults.len(), 1, "no per-decision fault storm");
    assert_eq!(faults[0].payload["fault_class"], "timeout");
    // The retried decisions carry the constant kernel reason, no guest text.
    for o in facts(&envs, "tool_outcome") {
        assert_eq!(o.payload["classification"]["Denied"], "denied by a module hook");
    }
}

/// B (HIGH): an untrusted (`WorkspaceConfig`) layer's hook is gated out and
/// has no effect, while a trusted (`UserConfig`) layer's hook binds.
#[test]
fn untrusted_origin_hook_is_not_bound_but_trusted_is() {
    let (untrusted_dir, mut untrusted) = open_origin(DENY_TOOL, ModuleOrigin::WorkspaceConfig);
    let (run_id, _trigger) = start_run(&mut untrusted);
    let allowed = untrusted
        .tool_call(run_id, principal(&untrusted), "fs.read", json!({ "path": "x" }))
        .unwrap();
    untrusted.close().unwrap();
    assert!(
        !matches!(allowed.classification, OutcomeClassification::Denied(_)),
        "a cloned workspace hook must not deny, got {:?}",
        allowed.classification
    );
    let untrusted_envs = envelopes(&untrusted_dir.path().join("log.zst"));
    assert!(facts(&untrusted_envs, "module_fault").is_empty());
    assert!(facts(&untrusted_envs, "tool_outcome")
        .iter()
        .all(|o| o.payload["classification"].get("Denied").is_none()));

    let (trusted_dir, mut trusted) = open_origin(DENY_TOOL, ModuleOrigin::UserConfig);
    let (run_id, _trigger) = start_run(&mut trusted);
    let denied = trusted
        .tool_call(run_id, principal(&trusted), "fs.read", json!({ "path": "x" }))
        .unwrap();
    trusted.close().unwrap();
    assert!(matches!(denied.classification, OutcomeClassification::Denied(_)));
    let _ = trusted_dir;
}

/// C (HIGH): a guest hook name/reason carrying multibyte, long text never
/// panics and never appears verbatim in a committed payload.
#[test]
fn multibyte_guest_hook_text_never_panics_or_reaches_payloads() {
    let name = format!("Ω{}", "é".repeat(200));
    let reason = "é".repeat(300);
    let source = format!(
        r#"
kb_name = "{name}"
function kb_on_activate(ctx) end
function kb_hot(x) return "hot" end
function kb_on_tool_intent(context)
  return {{ decision = "deny", reason = "{reason}", annotations = {{{{ key = "k", value = "{value}" }}}} }}
end
"#,
        value = "é".repeat(400)
    );
    let (dir, mut session) = open_with(&source);
    let (run_id, trigger) = start_run(&mut session);
    let mut provider = Scripted::new(vec![
        StepCommand::ToolIntent {
            tool: "fs.read".into(),
            arguments: json!({ "path": "x" }),
        },
        StepCommand::Finish(kanbei_scheduler::TerminalOutcome::Progress),
    ]);
    // The projection render truncates recent-event payloads at a byte bound;
    // a multibyte payload must not panic there.
    session
        .cognition_loop(run_id, trigger.clone(), &mut provider, |s| {
            s.project_context(run_id, &trigger)
        })
        .unwrap();
    session.close().unwrap();

    let envs = envelopes(&dir.path().join("log.zst"));
    assert!(facts(&envs, "tool_outcome")
        .iter()
        .all(|o| o.payload["classification"]["Denied"] == "denied by a module hook"));
    for env in &envs {
        let text = env.payload.to_string();
        assert!(!text.contains(&reason), "guest reason leaked into {}", env.kind);
        assert!(!text.contains(&name), "guest hook name leaked into {}", env.kind);
    }
}

/// F (MEDIUM): after a hook fault respawns the module, the UI mount resolves
/// to the fresh generation (not the dead one) and the hook is retried.
#[test]
fn respawn_rebinds_ui_mount_and_retries_hook() {
    require_guest();
    let dir = TempDir::new("respawn-ui");
    let m = manifest(Id128::generate(), UI_AND_TRAP_ONCE);
    let module_id = m.module_id;
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        engine: Some(trap_engine()),
        ..Default::default()
    })
    .unwrap();
    session.activate_ui(m).unwrap();
    let mount_generation = session.ui().unwrap().mounts[0].generation;
    assert!(mount_generation >= 1);

    let (run_id, _trigger) = start_run(&mut session);
    let first = session
        .tool_call(
            run_id,
            principal(&session),
            "fs.read",
            json!({ "path": "a" }),
        )
        .unwrap();
    assert!(matches!(first.classification, OutcomeClassification::Denied(_)));
    let fresh = session
        .modules()
        .unwrap()
        .snapshot()
        .into_iter()
        .find(|(id, _, _)| *id == module_id)
        .unwrap()
        .1;
    assert!(fresh > mount_generation, "respawn mints a fresh generation");
    assert_eq!(
        session.ui().unwrap().mounts[0].generation,
        fresh,
        "the UI mount must rebind to the fresh generation, not stay dead"
    );

    let second = session
        .tool_call(
            run_id,
            principal(&session),
            "fs.read",
            json!({ "path": "b" }),
        )
        .unwrap();
    session.close().unwrap();
    assert!(
        !matches!(second.classification, OutcomeClassification::Denied(_)),
        "the retried hook produces its real decision, got {:?}",
        second.classification
    );
}

/// A module with BOTH a root-scope UI mount and a hook that traps only once
/// (the marker survives respawn).
const UI_AND_TRAP_ONCE: &str = r#"
kb_name = "ui_retry"
function kb_on_activate(ctx)
  ctx.contribution_publish('{"kind":"ui","name":"panel","component":"panel_c"}')
end
function kb_hot(x) return "hot" end
function kb_on_tool_intent(context)
  local r = kb_host_call(1, '{"key":"ui_retry.seen"}')
  if string.find(r, '"value":null') then
    kb_host_call(2, '{"key":"ui_retry.seen","schema":1,"value":true}')
    local n = 0
    while true do n = n + 1 end
  end
  return { decision = "continue" }
end
"#;

fn gated_broker(session_id: Id128) -> kanbei_capabilities::Broker {
    use kanbei_capabilities::{Broker, Capability, Grant, GrantScope, PolicyTemplate};
    let mut broker = Broker::new();
    broker
        .add_template(PolicyTemplate {
            trust_class: TrustClass::Builtin,
            allow: vec![Capability::new("fs.write".into(), vec!["call".into()])],
            deny: vec![],
            require_approval: vec![Capability::new("fs.write".into(), vec!["call".into()])],
            version: 1,
            monotonic: true,
        })
        .unwrap();
    let mut grant = Grant {
        grant_digest: kanbei_core::digest::Digest::new(b"placeholder"),
        principal: kanbei_capabilities::Principal {
            session: session_id,
            generation: 0,
            run: None,
        },
        module_generation: 0,
        capability: Capability::new("fs.write".into(), vec!["call".into()]),
        scope: GrantScope::Session,
        expiry: None,
        budget: None,
        purpose: Some("hook test".into()),
        policy_version: 1,
    };
    grant.grant_digest = grant.derive_digest();
    broker.add_grant(grant).unwrap();
    broker
}
