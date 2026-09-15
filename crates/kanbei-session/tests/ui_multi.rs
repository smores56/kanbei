//! M8 wave 3 gate: multi-module UI composition. Two+ module generations
//! mount UIs into one composite frame with deterministic slot ordering;
//! input fans out to every mount's reducer with per-mount capability
//! isolation; focus navigates across mounts; a fault degrades only the
//! faulting mount; composition stays atomic with the existing fallback
//! classes; mid-session deactivation unbinds cleanly. Guest-wasm tests need
//! the guest; a missing guest is a hard failure (require_guest pattern; build
//! with `cargo xtask build-guest` from the workspace root).

use kanbei_capabilities::{
    Broker, Capability, Grant, GrantScope, PolicyTemplate, Principal, TrustClass,
};
use kanbei_core::digest::Digest;
use kanbei_core::id::Id128;
use kanbei_modules::package::{ModuleOrigin, PackageManifest};
use kanbei_scopes::contrib::ContributionKind;
use kanbei_session::{Session, SessionConfig};

mod common;
use common::{
    engine, frame_text, has_user_message, open, plain_module, require_guest, tempdir, ui_module,
};

/// A broker pre-granting `session:append` to the generation that will be
/// activated FIRST (generations are deterministic counters from 1, M2), with
/// an allow template. Mounts activated later carry no grant.
fn broker_with_append_grant(session_id: Id128, generation: u64) -> Broker {
    let mut broker = Broker::new();
    broker
        .add_template(PolicyTemplate {
            trust_class: TrustClass::Builtin,
            allow: vec![Capability::new("session".into(), vec!["append".into()])],
            deny: vec![],
            require_approval: vec![],
            version: 1,
            monotonic: true,
        })
        .unwrap();
    let policy_version = broker.policy_version();
    let mut grant = Grant {
        grant_digest: Digest::new(b"m8-grant"),
        principal: Principal {
            session: session_id,
            generation,
            run: None,
        },
        module_generation: generation,
        capability: Capability::new("session".into(), vec!["append".into()]),
        scope: GrantScope::Session,
        expiry: None,
        budget: None,
        purpose: Some("m8 multi-module ui".into()),
        policy_version,
    };
    grant.grant_digest = grant.derive_digest();
    broker.add_grant(grant).unwrap();
    broker
}

/// The visible body text of the last rendered frame: the tree owns the whole
/// surface, so every row counts.
fn body(session: &Session) -> String {
    frame_text(session.ui().unwrap().last_frame().unwrap())
}

/// Both mounts' trees appear in the composite frame, in slot order.
#[test]
fn two_mount_composition() {
    let (dir, mut session) = open("compose");
    require_guest();
    session
        .activate_ui(ui_module("aux_ui", "aux_comp", "aux", TrustClass::Builtin, false))
        .unwrap();
    session
        .activate_ui(ui_module("stat_ui", "stat_comp", "status", TrustClass::Builtin, false))
        .unwrap();

    // deterministic bind order: (slot, scope path, name)
    let host = session.ui().unwrap();
    assert_eq!(host.mounts.len(), 2);
    assert_eq!(host.mounts[0].slot, "aux");
    assert_eq!(host.mounts[0].component, "aux_comp");
    assert_eq!(host.mounts[1].slot, "status");
    assert_eq!(host.mounts[1].component, "stat_comp");

    // the composition carries the slots
    let mounts: Vec<(String, Option<String>)> = session
        .composition()
        .contributions
        .iter()
        .filter_map(|c| match &c.kind {
            ContributionKind::UiMount(m) => Some((m.name.clone(), m.slot.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        mounts,
        vec![
            ("aux_ui".to_string(), Some("aux".to_string())),
            ("stat_ui".to_string(), Some("status".to_string())),
        ]
    );

    session.ui_render_frame().unwrap();
    let text = body(&session);
    assert!(text.contains("panel aux_ui"), "first mount renders: {text}");
    assert!(text.contains("panel stat_ui"), "second mount renders: {text}");
    assert!(
        text.find("panel aux_ui").unwrap() < text.find("panel stat_ui").unwrap(),
        "slot order is child order"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Input fans out to EVERY mount's reducer: a char with the first mount
/// focused is recorded by both mounts and drafted only by the focused one.
#[test]
fn fan_out_reducers() {
    let (dir, mut session) = open("fanout");
    require_guest();
    session
        .activate_ui(ui_module("aux_ui", "aux_comp", "aux", TrustClass::Builtin, false))
        .unwrap();
    session
        .activate_ui(ui_module("stat_ui", "stat_comp", "status", TrustClass::Builtin, false))
        .unwrap();
    session.ui_render_frame().unwrap();

    // focus the first mount's input: Tab lands on the ring's second node
    // (revalidate-to-first, then advance), Shift-Tab backs onto the input
    session.ui_handle_input(b"\t\x1b[Z").unwrap();
    assert_eq!(
        session.ui().unwrap().focus.focused.as_deref(),
        Some("0.aux_ui_input"),
        "composite id is unambiguous"
    );

    let outcome = session.ui_handle_input(b"a").unwrap();
    assert!(!outcome.degraded);
    session.ui_render_frame().unwrap();
    let text = body(&session);
    // both reducers received the char; only the focused (aux) mount drafted
    assert_eq!(text.matches("char:a").count(), 2, "both reducers saw the event: {text}");
    assert!(
        frame_text(&session.ui().unwrap().last_frame().unwrap()).contains("❯ a"),
        "the focused mount's composer holds the draft"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Per-mount capability isolation: a mount without a grant has its intent
/// denied (counted on its own mount) while the granted mount's identical
/// intent applies.
#[test]
fn per_mount_grants() {
    let session_id = Id128::generate();
    let broker = broker_with_append_grant(session_id, 1);
    let dir = tempdir("grants");
    let mut session = Session::open(SessionConfig {
        dir: dir.clone(),
        stream: "m8-grants".into(),
        broker,
        session_id: Some(session_id),
        engine: Some(engine()),
        ..Default::default()
    })
    .unwrap();
    require_guest();
    // generation 1: granted mount; generation 2: no grant.
    session
        .activate_ui(ui_module("granted", "granted_comp", "main", TrustClass::Builtin, false))
        .unwrap();
    session
        .activate_ui(ui_module("denied", "denied_comp", "status", TrustClass::Builtin, false))
        .unwrap();
    session.ui_render_frame().unwrap();

    // focus the denied mount's input (2 Tabs from None: revalidate-to-first,
    // then advance twice: granted_input -> granted_btn -> denied_input)
    session.ui_handle_input(b"\t\t").unwrap();
    assert!(
        session
            .ui()
            .unwrap()
            .focus
            .focused
            .as_deref()
            .is_some_and(|id| id.ends_with("denied_input"))
    );

    // "hi" + Enter with the denied mount focused: its submit intent is
    // denied (no grant) and nothing canonical is committed.
    let outcome = session.ui_handle_input(b"hi\n").unwrap();
    assert_eq!(outcome.intents_applied, 0, "denied intent must not apply");
    assert_eq!(outcome.denied, 1, "denial counted");
    session.flush().unwrap();
    assert!(!has_user_message(&dir, "hi"), "denied intent must not commit");
    let host = session.ui().unwrap();
    assert_eq!(host.mounts[0].denied_intents, 0, "granted mount untouched");
    assert_eq!(host.mounts[1].denied_intents, 1, "denial counted on its mount");

    // wrap focus back to the granted mount and submit: the identical intent
    // applies (the wrap restores the granted_btn; Shift-Tab backs onto the
    // input so Enter submits instead of activating the button)
    session.ui_handle_input(b"\t\t\x1b[Z").unwrap();
    let outcome = session.ui_handle_input(b"yo\n").unwrap();
    assert_eq!(outcome.intents_applied, 1, "granted intent applies");
    session.flush().unwrap();
    assert!(has_user_message(&dir, "yo"));
    let host = session.ui().unwrap();
    assert_eq!(host.mounts[0].denied_intents, 0);
    assert_eq!(host.mounts[1].denied_intents, 1);
    std::fs::remove_dir_all(&dir).ok();
}

/// Focus cycles across mount subtrees in slot order (Tab) and stays within
/// the focused mount's subtree (arrows); a cross-mount Tab restores the
/// entered mount's remembered focus.
#[test]
fn focus_cycles_mounts() {
    let (dir, mut session) = open("focus");
    require_guest();
    session
        .activate_ui(ui_module("aux_ui", "aux_comp", "aux", TrustClass::Builtin, false))
        .unwrap();
    session
        .activate_ui(ui_module("stat_ui", "stat_comp", "status", TrustClass::Builtin, false))
        .unwrap();
    session.ui_render_frame().unwrap();

    fn focused(session: &Session) -> Option<(usize, String)> {
        session.ui().unwrap().focus.focused.clone().map(|id| {
            let (index, original) = kanbei_ui::tree::SemanticTree::split_composite_id(&id).unwrap();
            (index, original.to_string())
        })
    }

    // ring: 0.aux_ui_input, 0.aux_ui_btn, 1.stat_ui_input, 1.stat_ui_btn.
    // The first Tab lands on the ring's second node (revalidate-to-first,
    // then advance — M5 kernel semantics); Shift-Tab backs onto the input.
    session.ui_handle_input(b"\t").unwrap();
    assert_eq!(focused(&session), Some((0, "aux_ui_btn".into())));
    session.ui_handle_input(b"\t").unwrap();
    assert_eq!(focused(&session), Some((1, "stat_ui_input".into())), "Tab crosses into mount 1");

    // arrows stay within the focused mount's subtree
    session.ui_handle_input(b"\x1b[B").unwrap();
    assert_eq!(focused(&session), Some((1, "stat_ui_btn".into())));
    session.ui_handle_input(b"\x1b[B").unwrap();
    assert_eq!(focused(&session), Some((1, "stat_ui_input".into())), "wraps within the mount");
    session.ui_handle_input(b"\x1b[A").unwrap();
    assert_eq!(focused(&session), Some((1, "stat_ui_btn".into())));

    // Tab wraps to mount 0 and restores its remembered focus (aux_ui_btn)
    session.ui_handle_input(b"\t").unwrap();
    assert_eq!(
        focused(&session),
        Some((0, "aux_ui_btn".into())),
        "wrap restores the entered mount's remembered focus"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Mid-session deactivation (M5 deferred): replacing a mount's generation
/// unbinds its mount; the remaining mount rebinds and keeps working.
#[test]
fn deactivation_unbinds_replaced_mount() {
    let (dir, mut session) = open("deactivate");
    require_guest();
    let main = ui_module("main_ui", "main_comp", "main", TrustClass::Builtin, false);
    let stat = ui_module("stat_ui", "stat_comp", "status", TrustClass::Builtin, false);
    session.activate_ui(main.clone()).unwrap();
    session.activate_ui(stat.clone()).unwrap();
    assert_eq!(session.ui().unwrap().mounts.len(), 2);

    // replace the status mount's generation with a plain module (mounts
    // nothing): its mount unbinds, the main mount remains.
    let mut plain = plain_module();
    plain.module_id = stat.module_id;
    session.replace_module(stat.module_id, plain).unwrap();

    let host = session.ui().unwrap();
    assert_eq!(host.mounts.len(), 1, "replaced mount unbinds");
    assert_eq!(host.mounts[0].component, "main_comp");
    assert_eq!(host.mounts[0].slot, "main");
    assert!(
        session
            .composition()
            .contributions
            .iter()
            .all(|c| !matches!(&c.kind, ContributionKind::UiMount(m) if m.name == "stat_ui")),
        "the removed mount leaves the composition"
    );

    // the remaining mount still renders and handles input (Tab first so the
    // fan-out event carries the mount's slot as the target hint)
    session.ui_render_frame().unwrap();
    assert!(body(&session).contains("panel main_ui"));
    session.ui_handle_input(b"\t").unwrap();
    session.ui_handle_input(b"a").unwrap();
    session.ui_render_frame().unwrap();
    assert!(
        frame_text(&session.ui().unwrap().last_frame().unwrap()).contains("❯ a"),
        "the remaining mount's composer drafted"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Fault isolation: one mount's generation trapping degrades only that
/// mount (placeholder subtree); the other mount still renders and applies
/// intents.
#[test]
fn fault_isolation() {
    let session_id = Id128::generate();
    let broker = broker_with_append_grant(session_id, 1);
    let dir = tempdir("fault-isolation");
    let mut session = Session::open(SessionConfig {
        dir: dir.clone(),
        stream: "m8-fault".into(),
        broker,
        session_id: Some(session_id),
        engine: Some(engine()),
        ..Default::default()
    })
    .unwrap();
    require_guest();
    session
        .activate_ui(ui_module("good", "good_comp", "main", TrustClass::Builtin, false))
        .unwrap();
    session
        .activate_ui(ui_module("bad", "bad_comp", "status", TrustClass::Builtin, true))
        .unwrap();
    session.ui_render_frame().unwrap();

    // focus the flaky mount's input (2 Tabs from None) and trap it
    session.ui_handle_input(b"\t\t").unwrap();
    assert!(
        session
            .ui()
            .unwrap()
            .focus
            .focused
            .as_deref()
            .is_some_and(|id| id.ends_with("bad_input")),
        "flaky input focused"
    );
    let outcome = session.ui_handle_input(b"x").unwrap();
    assert!(outcome.degraded, "the faulting mount degrades the summary");
    let host = session.ui().unwrap();
    assert!(host.mounts[1].degraded, "the faulting mount is degraded");
    assert!(!host.mounts[0].degraded, "the other mount keeps working");

    session.ui_render_frame().unwrap();
    let text = body(&session);
    assert!(text.contains("panel good"), "healthy mount still renders: {text}");
    assert!(text.contains("UI component faulted"), "placeholder for the faulted mount: {text}");

    // the healthy mount still drafts, submits, and applies intents (the wrap
    // restores the good_btn; Shift-Tab backs onto the input so Enter submits
    // instead of activating the button)
    session.ui_handle_input(b"\t\x1b[Z").unwrap();
    session.ui_handle_input(b"y").unwrap();
    session.ui_render_frame().unwrap();
    assert!(
        frame_text(&session.ui().unwrap().last_frame().unwrap()).contains("❯ y"),
        "the healthy mount's composer drafted"
    );
    let outcome = session.ui_handle_input(b"\n").unwrap();
    assert_eq!(outcome.intents_applied, 1, "healthy mount's intent applies");
    session.flush().unwrap();
    assert!(has_user_message(&dir, "y"));
    let host = session.ui().unwrap();
    assert!(!host.mounts[0].degraded, "healthy mount still healthy");
    assert!(host.mounts[1].degraded, "faulted mount stays degraded");
    std::fs::remove_dir_all(&dir).ok();
}

/// Atomic composition with multiple mounts: a conflicting activation fails
/// atomically, both bound mounts are retained, and the staleness banner
/// renders over the last-valid composite.
#[test]
fn atomic_fallback_two_mounts() {
    let (dir, mut session) = open("atomic");
    require_guest();
    session
        .activate_ui(ui_module("aux_ui", "aux_comp", "aux", TrustClass::Builtin, false))
        .unwrap();
    session
        .activate_ui(ui_module("stat_ui", "stat_comp", "status", TrustClass::Builtin, false))
        .unwrap();
    let epoch_before = session.composition().epoch;

    // re-mounting the same name conflicts (holder exists): the activation
    // fails, the composition is retained, and both mounts stay bound.
    let err = session
        .activate_ui(ui_module("aux_ui", "other_comp", "main", TrustClass::Builtin, false))
        .unwrap_err();
    assert!(
        err.to_string().contains("conflict") || err.to_string().contains("ui"),
        "{err}"
    );
    assert_eq!(session.composition().epoch, epoch_before, "last-valid retained");
    assert_eq!(session.ui().unwrap().mounts.len(), 2, "both mounts stay bound");
    assert!(session.ui().unwrap().staleness.is_some(), "staleness banner set");

    session.ui_render_frame().unwrap();
    let frame = session.ui().unwrap().last_frame().unwrap().clone();
    assert!(frame.row_text(0).starts_with("composition stale"), "banner rendered");
    assert!(body(&session).contains("panel stat_ui"), "last-valid composite still renders");
    std::fs::remove_dir_all(&dir).ok();
}

/// A single-mount module whose `ui_render` returns a valid tree once and then
/// an invalid one: exercises the fault → last-valid path. The module-level
/// counter persists across `call_generation` calls on one generation instance.
fn flaky_render_module() -> PackageManifest {
    let source = r#"
local renders = 0
function kb_on_activate(ctx)
  ctx.contribution_publish('{"kind":"ui","name":"cache_ui","component":"cache_comp","slot":"main"}')
end
function kb_hot(d)
  if d.entry == "ui_reduce" then
    return { state = d.state, intents = {} }
  elseif d.entry == "ui_render" then
    renders = renders + 1
    if renders > 1 then
      return { root = { id = "r", kind = "carousel" } }
    end
    return { root = { id = "root", kind = "stack", children = {
      { id = "title", kind = "text", spans = { { text = "last valid" } } },
    } } }
  end
  error("unknown entry")
end
"#;
    PackageManifest {
        schema: kanbei_modules::PACKAGE_SCHEMA,
        module_id: Id128::generate(),
        origin: ModuleOrigin::UserConfig,
        trust_class: TrustClass::Builtin,
        scope: kanbei_services::ScopePath(vec!["root".into()]),
        deps: Vec::new(),
        capabilities: Vec::new(),
        source: source.to_string(),
        state_schema: None,
        state_key: None,
    }
}

/// Decision 22 event-driven composition: a pure repaint re-renders the cached
/// composite natively and performs ZERO guest `ui_render` calls.
#[test]
fn repaint_does_not_reenter_guest() {
    let (dir, mut session) = open("cache-repaint");
    require_guest();
    session
        .activate_ui(ui_module("main_ui", "main_comp", "main", TrustClass::Builtin, false))
        .unwrap();
    session.ui_render_frame().unwrap();
    let first = session.ui().unwrap().mounts[0].render_calls;
    assert!(first >= 1, "the initial composition renders the guest");
    for _ in 0..5 {
        session.ui_render_frame().unwrap();
    }
    assert_eq!(
        session.ui().unwrap().mounts[0].render_calls,
        first,
        "repaint must not re-enter the guest"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Decision 22: a state change (a reduce) recomposes exactly once; the
/// following repaint is served from the cache.
#[test]
fn state_change_recomposes_once() {
    let (dir, mut session) = open("cache-once");
    require_guest();
    session
        .activate_ui(ui_module("main_ui", "main_comp", "main", TrustClass::Builtin, false))
        .unwrap();
    session.ui_render_frame().unwrap();
    let before = session.ui().unwrap().mounts[0].render_calls;
    session.ui_handle_input(b"a").unwrap();
    session.ui_render_frame().unwrap();
    let after = session.ui().unwrap().mounts[0].render_calls;
    assert_eq!(after, before + 1, "one guest render per state change");
    session.ui_render_frame().unwrap();
    assert_eq!(
        session.ui().unwrap().mounts[0].render_calls,
        after,
        "the repaint after the recomposition is cached"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Decision 22 fault policy: a recomposition fault keeps the previous
/// last-valid tree and is not retried on subsequent repaints.
#[test]
fn render_fault_keeps_last_valid_without_retry() {
    let (dir, mut session) = open("cache-fault");
    require_guest();
    session.activate_ui(flaky_render_module()).unwrap();
    session.ui_render_frame().unwrap();
    assert!(body(&session).contains("last valid"), "first tree is valid");
    let after_first = session.ui().unwrap().mounts[0].render_calls;

    // state change → recompose → the guest returns an invalid tree
    session.ui_handle_input(b"a").unwrap();
    session.ui_render_frame().unwrap();
    assert!(session.ui().unwrap().degraded, "fault degrades the mount");
    assert!(body(&session).contains("last valid"), "last-valid tree preserved");
    assert!(
        !body(&session).contains("UI component faulted"),
        "no placeholder replaces a valid tree"
    );
    let after_fault = session.ui().unwrap().mounts[0].render_calls;
    assert_eq!(after_fault, after_first + 1, "the fault is one render attempt");

    for _ in 0..5 {
        session.ui_render_frame().unwrap();
    }
    assert_eq!(
        session.ui().unwrap().mounts[0].render_calls,
        after_fault,
        "a faulted mount is not retried per frame"
    );
    assert!(body(&session).contains("last valid"), "still the last-valid tree");
    std::fs::remove_dir_all(&dir).ok();
}

/// A single-mount UI fixture whose tree carries a non-modal button, a
/// non-modal overlay button, and a higher-z modal `layer` holding an input +
/// button: the kernel confines the focus ring to the modal.
fn modal_module() -> PackageManifest {
    let source = r#"
function kb_on_activate(ctx)
  ctx.contribution_publish('{"kind":"ui","name":"modal_ui","component":"modal_comp","slot":"main"}')
end
function kb_hot(d)
  if d.entry == "ui_reduce" then
    return { state = d.state, intents = {} }
  elseif d.entry == "ui_render" then
    return { root = { id = "root", kind = "stack", children = {
      { id = "outside_btn", kind = "button", label = "outside" },
      { id = "overlay", kind = "layer", z = 1, modal = false, children = {
        { id = "under_btn", kind = "button", label = "under" },
      } },
      { id = "sheet", kind = "layer", z = 5, modal = true, children = {
        { id = "m_input", kind = "input", content = "x" },
        { id = "m_btn", kind = "button", label = "ok" },
      } },
    } } }
  end
  error("unknown entry: " .. tostring(d.entry))
end
"#;
    PackageManifest {
        schema: kanbei_modules::PACKAGE_SCHEMA,
        module_id: Id128::generate(),
        origin: ModuleOrigin::UserConfig,
        trust_class: TrustClass::Builtin,
        scope: kanbei_services::ScopePath(vec!["root".into()]),
        deps: Vec::new(),
        capabilities: Vec::new(),
        source: source.to_string(),
        state_schema: None,
        state_key: None,
    }
}

/// Modal containment is kernel-owned and driven by the render: focus enters
/// the topmost modal layer and Tab cycles only its focusables.
#[test]
fn modal_confines_focus_after_render() {
    require_guest();
    let (dir, mut session) = open("modal-focus");
    session.activate_ui(modal_module()).unwrap();
    session.ui_render_frame().unwrap();
    assert_eq!(
        session.ui().unwrap().focus.focused.as_deref(),
        Some("m_input"),
        "focus enters the topmost modal"
    );
    session.ui_handle_input(b"\t").unwrap();
    assert_eq!(session.ui().unwrap().focus.focused.as_deref(), Some("m_btn"));
    session.ui_handle_input(b"\t").unwrap();
    assert_eq!(
        session.ui().unwrap().focus.focused.as_deref(),
        Some("m_input"),
        "Tab wraps inside the modal, never reaching outside/under"
    );
    std::fs::remove_dir_all(&dir).ok();
}
