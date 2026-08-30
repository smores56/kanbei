//! M8 wave 3 gate: multi-module UI composition. Two+ module generations
//! mount UIs into one composite frame with deterministic slot ordering;
//! input fans out to every mount's reducer with per-mount capability
//! isolation; focus navigates across mounts; a fault degrades only the
//! faulting mount; composition stays atomic with the existing fallback
//! classes; mid-session deactivation unbinds cleanly. Skips when the guest
//! wasm is not built (require_guest pattern).

use kanbei_capabilities::{
    Broker, Capability, Grant, GrantScope, PolicyTemplate, Principal, TrustClass,
};
use kanbei_core::digest::Digest;
use kanbei_core::id::Id128;
use kanbei_scopes::contrib::ContributionKind;
use kanbei_session::{Session, SessionConfig};

mod common;
use common::{
    engine, has_user_message, input_row, open, plain_module, require_guest, tempdir, ui_module,
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

/// The visible body text of the last rendered frame (excludes the kernel
/// status bar and the input line).
fn body(session: &Session) -> String {
    let frame = session.ui().unwrap().last_frame().unwrap().clone();
    (1..frame.rows - 2)
        .map(|r| frame.row_text(r))
        .collect::<Vec<_>>()
        .join("|")
}

/// Both mounts' trees appear in the composite frame, in slot order.
#[test]
fn two_mount_composition() {
    let (dir, mut session) = open("compose");
    if !require_guest() {
        return;
    }
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
    if !require_guest() {
        return;
    }
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
    assert_eq!(input_row(&session.ui().unwrap().last_frame().unwrap()), "> a");
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
    if !require_guest() {
        return;
    }
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
    if !require_guest() {
        return;
    }
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
    if !require_guest() {
        return;
    }
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
    assert_eq!(input_row(&session.ui().unwrap().last_frame().unwrap()), "> a");
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
    if !require_guest() {
        return;
    }
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
    assert_eq!(input_row(&session.ui().unwrap().last_frame().unwrap()), "> y");
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
    if !require_guest() {
        return;
    }
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
