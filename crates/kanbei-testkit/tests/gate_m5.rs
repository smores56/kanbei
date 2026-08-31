//! M5 gate: semantic workbench. Exercises the kernel terminal/fallback
//! boundary (kanbei-ui), the built-in UI as an immutable module generation
//! through the standard contribution contract, and the three R-27 fault
//! classes. Skips are documented where the guest wasm is not built.

use std::path::PathBuf;

use kanbei_capabilities::{Capability, Grant, GrantScope, PolicyTemplate, Principal, TrustClass};
use kanbei_core::digest::Digest;
use kanbei_core::id::Id128;
use kanbei_modules::package::{ModuleOrigin, PackageManifest};
use kanbei_session::{FaultPoint, Session, SessionConfig};
use kanbei_testkit::{child_acked, spawn_m5_crash_child, verify_m5_recovery};
use kanbei_ui::terminal::{TestTerminal, TermiosTerminal, is_raw_mode, openpty};

/// NO_EPOCH engine: fuel/epoch are session-safety bounds; the workbench
/// module needs fuel beyond the default 1M per call (M2 recipe).
fn engine() -> kanbei_vm::VmConfig {
    kanbei_vm::VmConfig {
        fuel_per_call: u64::MAX,
        epoch_deadline: u64::MAX / 2,
        ..Default::default()
    }
}

/// Module tests need the guest wasm; without it they skip with a note (the
/// crash matrix skips too — its points fire inside the module lifecycle, so
/// a wasm-less run would fail every point instead).
fn require_guest() -> bool {
    match kanbei_vm::Vm::load(engine()) {
        Ok(_) => true,
        Err(kanbei_vm::GuestError::NotBuilt) => {
            eprintln!(
                "skip: guest wasm not built (run `cargo build -p kanbei-guest \
                 --target wasm32-wasip1 --release`)"
            );
            false
        }
        Err(e) => panic!("guest vm load: {e}"),
    }
}

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kanbei-m5-{tag}-{}-{}",
        std::process::id(),
        Id128::generate()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn input_row(frame: &kanbei_ui::TerminalFrame) -> String {
    frame.row_text(frame.rows - 1)
}

fn open(tag: &str) -> (PathBuf, Session) {
    let dir = tempdir(tag);
    let session = Session::open(SessionConfig {
        dir: dir.clone(),
        stream: format!("m5-{tag}"),
        engine: Some(engine()),
        ..Default::default()
    })
    .unwrap();
    (dir, session)
}

/// The M5 UI e2e: activate the built-in UI module generation, type through
/// the kernel boundary, submit → canonical user_message, render a frame with
/// the text visible.
#[test]
fn builtin_ui_end_to_end() {
    let (dir, mut session) = open("e2e");
    if session.modules().is_none() {
        eprintln!("skip: guest wasm not built (kanbei-vm NotBuilt)");
        return;
    }
    let epoch = session.activate_builtin_ui().unwrap();
    assert!(epoch > 0);

    // The UI mount published through the standard contribution contract is
    // in the composition.
    let mount = session
        .composition()
        .contributions
        .iter()
        .find(|c| matches!(c.kind, kanbei_scopes::contrib::ContributionKind::UiMount(_)))
        .expect("ui mount contribution in composition");
    let ui = session.ui().expect("ui host bound");
    assert_eq!(ui.component, kanbei_ui::BUILTIN_UI_COMPONENT);
    let _ = mount;

    // Type "hello": each char reduces in the module.
    for ch in b"hello" {
        let outcome = session.ui_handle_input(&[*ch]).unwrap();
        assert!(!outcome.degraded, "reduce degraded: {:?}", session.ui().unwrap().last_error);
    }
    session.ui_render_frame().unwrap();
    let frame = session.ui().unwrap().last_frame().unwrap().clone();
    assert_eq!(input_row(&frame), "> hello");

    // Enter submits: canonical user_message + responder trigger.
    let outcome = session.ui_handle_input(b"\n").unwrap();
    assert_eq!(outcome.intents_applied, 1, "submit intent applied");
    let has_user_message = kanbei_testkit::collect_envelopes(&dir)
        .unwrap()
        .iter()
        .any(|e| {
            e.kind == "user_message"
                && e.payload.get("text").and_then(|t| t.as_str()) == Some("hello")
        });
    assert!(has_user_message, "canonical user_message committed");

    // The module cleared its draft; the refresh fact lands in the log view.
    session.ui_render_frame().unwrap();
    let frame = session.ui().unwrap().last_frame().unwrap().clone();
    assert_eq!(input_row(&frame), ">", "draft cleared after submit");
    session.ui_refresh("user message committed").unwrap();
    session.ui_render_frame().unwrap();
    let frame = session.ui().unwrap().last_frame().unwrap().clone();
    assert!(frame.row_text(1).contains("user message committed"), "log line rendered");
    std::fs::remove_dir_all(&dir).ok();
}

/// Focus navigation + reserved keys never reach the module: the draft is
/// untouched by Tab/Ctrl-C/Ctrl-X Ctrl-S; Ctrl-X Ctrl-S enters safe mode
/// (canonical fact + fallback UI).
#[test]
fn focus_and_reserved_keys() {
    let (dir, mut session) = open("reserved");
    if session.modules().is_none() {
        eprintln!("skip: guest wasm not built");
        return;
    }
    session.activate_builtin_ui().unwrap();

    // Type "a", then navigation + reserved keys, then "b": only a and b land.
    session.ui_handle_input(b"a").unwrap();
    session.ui_handle_input(b"\x1b[A\x09\x03").unwrap(); // up, tab, ctrl-c
    session.ui_handle_input(b"b").unwrap();
    session.ui_render_frame().unwrap();
    let frame = session.ui().unwrap().last_frame().unwrap().clone();
    assert_eq!(input_row(&frame), "> ab", "nav/reserved keys must not reach the module");

    // Tab moves focus onto the input node (kernel focus model).
    session.ui_handle_input(b"\x09").unwrap();
    assert_eq!(
        session.ui().unwrap().focus.focused.as_deref(),
        Some("input"),
        "focus moves onto the focusable input"
    );

    // Safe-mode chord: canonical fact, fallback UI, input dropped.
    session.ui_handle_input(b"\x18s").unwrap();
    let has_safe_mode = kanbei_testkit::collect_envelopes(&dir)
        .unwrap()
        .iter()
        .any(|e| e.kind == "safe_mode_activated");
    assert!(has_safe_mode);
    assert!(session.ui().unwrap().safe_mode);
    session.ui_handle_input(b"c").unwrap();
    session.ui_render_frame().unwrap();
    let frame = session.ui().unwrap().last_frame().unwrap().clone();
    assert!(frame.row_text(0).contains("safe mode"), "fallback UI header");
    std::fs::remove_dir_all(&dir).ok();
}

/// Capability intersection (R-27): a broker denying session:append blocks
/// the builtin UI's submit intent (deny wins over the kernel's builtin
/// grant); the intent is dropped and no canonical fact is committed.
#[test]
fn capability_intersection_denies_ui_intents() {
    let dir = tempdir("caps");
    let session_id = Id128::generate();
    let mut broker = kanbei_capabilities::Broker::new();
    broker
        .add_template(PolicyTemplate {
            trust_class: TrustClass::Builtin,
            allow: vec![Capability::new("session".into(), vec!["cancel".into()])],
            deny: vec![Capability::new("session".into(), vec!["append".into()])],
            require_approval: vec![],
            version: 1,
            monotonic: true,
        })
        .unwrap();
    let mut grant = Grant {
        grant_digest: Digest::new(b"x"),
        principal: Principal {
            session: session_id,
            generation: 0,
            run: None,
        },
        module_generation: 0,
        capability: Capability::new("session".into(), vec!["cancel".into()]),
        scope: GrantScope::Session,
        expiry: None,
        budget: None,
        purpose: Some("m5".into()),
        policy_version: 1,
    };
    grant.grant_digest = grant.derive_digest();
    broker.add_grant(grant).unwrap();
    let mut session = Session::open(SessionConfig {
        dir: dir.clone(),
        stream: "m5-caps".into(),
        broker,
        session_id: Some(session_id),
        engine: Some(engine()),
        ..Default::default()
    })
    .unwrap();
    if session.modules().is_none() {
        eprintln!("skip: guest wasm not built");
        return;
    }
    session.activate_builtin_ui().unwrap();
    let outcome = session.ui_handle_input(b"secret\n").unwrap();
    assert_eq!(outcome.intents_applied, 0, "denied intent must not apply");
    assert_eq!(session.ui().unwrap().denied_intents, 1, "denial counted");
    let has_user_message = kanbei_testkit::collect_envelopes(&dir)
        .unwrap()
        .iter()
        .any(|e| e.kind == "user_message");
    assert!(!has_user_message, "denied intent must not commit a canonical fact");
    std::fs::remove_dir_all(&dir).ok();
}

/// A flaky UI module for the fault-class-2 test: errors on reduce for the
/// char "x", works otherwise. Activated as a second root-scope mount so the
/// host binds it.
fn flaky_ui_manifest() -> PackageManifest {
    PackageManifest {
        schema: 1,
        module_id: Id128::generate(),
        origin: ModuleOrigin::UserConfig,
        trust_class: TrustClass::Builtin,
        scope: kanbei_services::ScopePath(vec!["root".into()]),
        deps: Vec::new(),
        capabilities: Vec::new(),
        source: r#"
function kb_on_activate(ctx)
  ctx.contribution_publish('{"kind":"ui","name":"flaky","component":"flaky_ui"}')
end
function kb_hot(d)
  if d.entry == "ui_reduce" then
    local e = d.event or {}
    -- trap fault (instance dies; M2 documented)
    if e.kind == "char" and e.text == "x" then error("boom") end
    local s = d.state
    if type(s) ~= "table" then s = { draft = "", bad = false } end
    if e.kind == "char" then
      if e.text == "z" then s.bad = true
      elseif e.text == "w" then s.bad = false
      else s.draft = (s.draft or "") .. e.text end
    end
    return { state = s, intents = {} }
  elseif d.entry == "ui_render" then
    local s = d.state or {}
    if s.bad then
      -- host-side render fault: unknown node kind
      return { root = { id = "r", kind = "carousel" } }
    end
    return { root = { id = "root", kind = "root", children = {
      { id = "input", kind = "input", content = tostring(s.draft or ""), focusable = true },
    } } }
  end
  error("unknown entry")
end
"#
        .to_string(),
        state_schema: None,
    }
}

/// Runtime component fault (R-27 class 2): a module that errors on reduce is
/// marked degraded and the kernel renders a placeholder; a later successful
/// event clears the flag.
#[test]
fn runtime_component_fault_degrades() {
    let (_dir, mut session) = open("fault");
    if session.modules().is_none() {
        eprintln!("skip: guest wasm not built");
        return;
    }
    // Activate the flaky module FIRST so the host binds it (snapshot order).
    session.activate_ui(flaky_ui_manifest()).unwrap();
    assert_eq!(session.ui().unwrap().component, "flaky_ui");

    // Host-side render fault (R-27 class 2): 'z' marks the tree bad; the
    // render returns an unknown node kind → the kernel marks the module
    // degraded and renders a placeholder.
    session.ui_handle_input(b"z").unwrap();
    assert!(!session.ui().unwrap().degraded, "reduce itself succeeded");
    session.ui_render_frame().unwrap();
    assert!(session.ui().unwrap().degraded, "invalid tree degrades the module");
    let frame = session.ui().unwrap().last_frame().unwrap().clone();
    let body: String = (1..frame.rows - 2)
        .map(|r| frame.row_text(r))
        .collect::<Vec<_>>()
        .join("|");
    assert!(body.contains("UI component faulted"), "placeholder rendered: {body}");

    // A later successful render clears the degradation (host-side fault:
    // the instance lives).
    session.ui_handle_input(b"w").unwrap();
    session.ui_render_frame().unwrap();
    assert!(!session.ui().unwrap().degraded, "successful render clears degradation");
    let frame = session.ui().unwrap().last_frame().unwrap().clone();
    assert_eq!(input_row(&frame), ">");

    // Trap fault: 'x' errors inside the guest → the wasm instance dies
    // (M2 documented); the module stays degraded with the placeholder.
    session.ui_handle_input(b"x").unwrap();
    assert!(session.ui().unwrap().degraded, "trap degrades the module");
    session.ui_render_frame().unwrap();
    session.ui_handle_input(b"y").unwrap();
    assert!(
        session.ui().unwrap().degraded,
        "a trapped instance stays degraded (recovery = generation replacement)"
    );
    std::fs::remove_dir_all(&_dir).ok();
}

/// Composition failure (R-27 class 1): a conflicting UI activation fails
/// atomically, the last-valid composition is retained, and the UI shows the
/// staleness banner.
#[test]
fn composition_failure_retains_last_valid() {
    let (dir, mut session) = open("stale");
    if session.modules().is_none() {
        eprintln!("skip: guest wasm not built");
        return;
    }
    session.activate_builtin_ui().unwrap();
    let epoch_before = session.composition().epoch;

    // Re-activating the same builtin conflicts on the ui mount (holder
    // exists): the activation fails, the composition is retained, and the
    // UI is marked stale.
    let err = session.activate_builtin_ui().unwrap_err();
    assert!(err.to_string().contains("conflict") || err.to_string().contains("ui"));
    assert_eq!(session.composition().epoch, epoch_before, "last-valid retained");
    assert!(session.ui().unwrap().staleness.is_some(), "staleness banner set");
    session.ui_render_frame().unwrap();
    let frame = session.ui().unwrap().last_frame().unwrap().clone();
    assert!(frame.row_text(0).starts_with("composition stale"), "banner rendered");
    std::fs::remove_dir_all(&dir).ok();
}

/// Kernel render fault (R-27 class 3): a terminal write failure falls back to
/// the kernel fallback UI, and terminal restoration stays reliable.
#[test]
fn kernel_render_fault_falls_back() {
    let (_dir, mut session) = open("render-fault");
    if session.modules().is_none() {
        eprintln!("skip: guest wasm not built");
        return;
    }
    session.activate_builtin_ui().unwrap();
    session.ui_handle_input(b"x").unwrap();

    // A real pty: raw mode entered, then a failing terminal write → fallback.
    let (master, slave) = openpty().unwrap();
    let mut term = TermiosTerminal::open(slave).unwrap();
    {
        let _guard = kanbei_ui::TerminalGuard::new(&mut term).unwrap();
        assert!(is_raw_mode(&master).unwrap());
        let mut failing = TestTerminal::failing_after(0);
        let err = session.ui_present(&mut failing).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
        // Kernel fallback UI: the frame is the kernel-authored fallback
        // (the working-terminal present re-renders it; diff may be empty so
        // assert on the frame, not the written bytes).
        let mut ok_term = TestTerminal::new();
        session.ui_present(&mut ok_term).unwrap();
        let frame = session.ui().unwrap().last_frame().unwrap().clone();
        assert!(frame.row_text(0).contains("safe mode"), "fallback UI frame");
        assert!(session.ui().unwrap().safe_mode);
        let _ = ok_term;
    }
    assert!(!is_raw_mode(&master).unwrap(), "terminal restored after fallback");
    std::fs::remove_dir_all(&_dir).ok();
}

/// Terminal restoration/fallback remains reliable (acceptance bullet): raw
/// mode in, restore on drop, on explicit restore, and after a simulated
/// crash (guard dropped mid-raw).
#[test]
fn terminal_restoration_reliable() {
    let (master, slave) = openpty().unwrap();
    {
        let mut term = TermiosTerminal::open(slave).unwrap();
        {
            let _guard = kanbei_ui::TerminalGuard::new(&mut term).unwrap();
            assert!(is_raw_mode(&master).unwrap());
        }
        assert!(!term.is_raw());
        assert!(!is_raw_mode(&master).unwrap());
        // Explicit restore is idempotent.
        term.enter_raw().unwrap();
        assert!(is_raw_mode(&master).unwrap());
        term.restore().unwrap();
        term.restore().unwrap();
        assert!(!is_raw_mode(&master).unwrap());
    }
    // Drop of the terminal itself restores too.
    let slave = openpty().unwrap().1;
    let mut term = TermiosTerminal::open(slave).unwrap();
    term.enter_raw().unwrap();
    drop(term);
    assert!(!is_raw_mode(&master).unwrap());
}

/// Hot path (consistency 13): Luau/Wasm stays off cell rendering — the
/// renderer and diff are pure Rust with no module dependency. Structural
/// check: kanbei-ui's manifest must not depend on kanbei-vm/modules.
#[test]
fn hot_path_structure() {
    let manifest = std::fs::read_to_string(
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../kanbei-ui/Cargo.toml"),
    )
    .unwrap();
    assert!(
        !manifest.contains("kanbei-vm") && !manifest.contains("kanbei-modules"),
        "kanbei-ui must not depend on the wasm substrate"
    );
}

/// Crash injection at the UI boundary: every M5 point SIGABRTs the child and
/// recovery verifies (reopen, atomic composition intact, UI re-activates,
/// no canonical gestures).
#[test]
fn crash_matrix_m5() {
    if std::env::var("KANBEI_SKIP_CRASH").is_ok() {
        eprintln!("skip: KANBEI_SKIP_CRASH set");
        return;
    }
    // The M5 points fire inside the UI module lifecycle; without the guest
    // wasm the child never activates a module and every point FAILS instead
    // of skipping (every other m5 test guards on `modules().is_none()` — the
    // matrix must too, or the suite is red on a clean tree).
    if !require_guest() {
        return;
    }
    let points = [
        FaultPoint::BeforeUiReduce,
        FaultPoint::AfterUiReduce,
        FaultPoint::BeforeUiRender,
        FaultPoint::AfterUiRender,
    ];
    for after_acks in [0u64, 2] {
        for point in points {
            let dir = tempdir(&format!("crash-{point:?}-{after_acks}"));
            let mut child = spawn_m5_crash_child(&dir, point, after_acks);
            let status = child.wait().unwrap();
            assert!(
                !status.success(),
                "{point:?}/{after_acks}: crash child must abort, got {status:?}"
            );
            let acked = child_acked(&mut child);
            let checks = verify_m5_recovery(&dir, acked).unwrap_or_else(|e| {
                panic!("{point:?}/{after_acks}: {e}");
            });
            assert!(checks >= 5, "{point:?}/{after_acks}: {checks} checks");
            std::fs::remove_dir_all(&dir).ok();
        }
    }
}
