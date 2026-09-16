//! T12 context ABI (decision 32): the kernel hands the built-in shell a
//! read-only render context (transcript, status, size, focus, selection,
//! viewport) alongside the module's reducer state, and the shell composes the
//! Maki frame from it. Guest-wasm tests need the guest (require_guest; build
//! with `cargo xtask build-guest`).

mod common;

use kanbei_capabilities::TrustClass;
use kanbei_core::id::Id128;
use kanbei_modules::package::{ModuleOrigin, PackageManifest};
use kanbei_scopes::contrib::ContributionKind;
use kanbei_session::Session;

use common::{input_row, open, require_guest};

/// The visible text of the last rendered frame: the tree owns the whole
/// surface, so every row counts.
fn body(session: &Session) -> String {
    let frame = session.ui().unwrap().last_frame().unwrap().clone();
    (0..frame.rows())
        .map(|r| frame.row_text(r))
        .collect::<Vec<_>>()
        .join("|")
}

/// A fixture module that echoes the render context into its tree, so the
/// context the kernel hands the guest is observable from the frame.
fn context_module() -> PackageManifest {
    PackageManifest {
        schema: kanbei_modules::PACKAGE_SCHEMA,
        module_id: Id128::generate(),
        origin: ModuleOrigin::UserConfig,
        trust_class: TrustClass::Builtin,
        scope: kanbei_services::ScopePath(vec!["root".into()]),
        deps: Vec::new(),
        capabilities: Vec::new(),
        source: r#"
function kb_on_activate(ctx)
  ctx.contribution_publish('{"kind":"ui","name":"ctx_ui","component":"ctx_comp","slot":"main"}')
end
function kb_hot(d)
  if d.entry == "ui_reduce" then
    return { state = d.state, intents = {} }
  elseif d.entry == "ui_render" then
    local c = d.context or {}
    local size = c.size or {}
    local turns = 0
    if type(c.transcript) == "table" and type(c.transcript.turns) == "table" then
      turns = #c.transcript.turns
    end
    return { root = { id = "root", kind = "stack", children = {
      { id = "ctx_status", kind = "text", spans = { { text = "status=" .. tostring(c.status) } } },
      { id = "ctx_size", kind = "text", spans = { { text = "size=" .. tostring(size.cols) .. "x" .. tostring(size.rows) } } },
      { id = "ctx_turns", kind = "text", spans = { { text = "turns=" .. tostring(turns) } } },
      { id = "ctx_focus", kind = "text", spans = { { text = "focus=" .. tostring(c.focus) } } },
      { id = "ctx_viewport", kind = "text", spans = { { text = "viewport=" .. tostring(c.viewport_top) } } },
      { id = "ctx_input", kind = "input", content = "" },
    } } }
  end
  error("unknown entry: " .. tostring(d.entry))
end
"#
        .to_string(),
        state_schema: None,
        state_key: None,
    }
}

/// The context carries the kernel status, the surface size, and the
/// transcript; a commit changes the transcript and recomposes the mount.
#[test]
fn render_context_reaches_the_module() {
    let (dir, mut session) = open("ui-context");
    require_guest();
    session.activate_ui(context_module()).unwrap();
    session.ui_render_frame().unwrap();
    let text = body(&session);
    assert!(text.contains("status=idle"), "kernel status in context: {text}");
    assert!(text.contains("size=80x24"), "surface size in context: {text}");
    assert!(text.contains("turns=0"), "empty transcript in context: {text}");

    // A canonical commit changes the transcript; the module sees it on the
    // next composition without a manual refresh.
    session.append_user_message("hello context").unwrap();
    session.ui_render_frame().unwrap();
    let text = body(&session);
    assert!(text.contains("turns=1"), "committed turn in context: {text}");
    std::fs::remove_dir_all(&dir).ok();
}

/// The built-in shell composes turn rows from the transcript context.
#[test]
fn builtin_shell_renders_transcript_rows() {
    let (dir, mut session) = open("ui-shell-turns");
    require_guest();
    session.activate_builtin_ui().unwrap();
    session.append_user_message("hello shell").unwrap();
    session.ui_render_frame().unwrap();
    let text = body(&session);
    assert!(text.contains("❯ hello shell"), "user turn row: {text}");
    assert!(text.contains("… working"), "live working indicator: {text}");
    std::fs::remove_dir_all(&dir).ok();
}

/// `activate_builtin_ui` activates the builtin shell as a NON-config module:
/// it contributes its mount/theme and commits its canonical composition event,
/// but must NOT pollute the config identity (`config_layers`/`config_digest`)
/// with a non-config module.
#[test]
fn builtin_ui_activation_leaves_config_identity_untouched() {
    let (dir, mut session) = open("ui-config-identity");
    require_guest();
    let layers_before = session.config_layer_digests();
    let digest_before = session.config_digest();
    session.activate_builtin_ui().unwrap();

    assert_eq!(
        session.config_layer_digests(),
        layers_before,
        "the builtin UI is not a config layer"
    );
    assert_eq!(session.config_digest(), digest_before, "config digest untouched");
    // Its contributions and canonical event are preserved.
    assert!(session.ui().is_some(), "UI host bound");
    assert!(
        session
            .composition()
            .contributions
            .iter()
            .any(|c| matches!(c.kind, ContributionKind::UiMount(_))),
        "ui mount contribution in the composition"
    );
    assert!(
        session
            .composition()
            .contributions
            .iter()
            .any(|c| matches!(c.kind, ContributionKind::Theme(_))),
        "theme contribution in the composition"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// The shell renders when the transcript is empty (header + composer prompt).
#[test]
fn builtin_shell_renders_empty_transcript() {
    let (dir, mut session) = open("ui-shell-empty");
    require_guest();
    session.activate_builtin_ui().unwrap();
    session.ui_render_frame().unwrap();
    let frame = session.ui().unwrap().last_frame().unwrap().clone();
    assert!(
        frame.row_text(0).contains("kanbei · idle"),
        "shell header: {:?}",
        frame.row_text(0)
    );
    assert_eq!(input_row(&frame), "❯", "composer prompt with no draft");
    std::fs::remove_dir_all(&dir).ok();
}

/// Session-local collapse overrides are applied kernel-side when the context
/// transcript is built (decision 9/32): a settled turn collapses to a header
/// and re-opens when the override is set, without re-entering the projection.
#[test]
fn collapse_overrides_are_kernel_side() {
    let (dir, mut session) = open("ui-shell-collapse");
    require_guest();
    session.activate_builtin_ui().unwrap();
    session.append_user_message("collapse me").unwrap();
    // Settle the active turn (no terminal outcome recorded → Interrupted).
    session.finalize_transcript_turn();
    session.ui_render_frame().unwrap();
    let collapsed = body(&session);
    assert!(collapsed.contains("▸ ["), "settled turn collapses: {collapsed}");

    session.toggle_transcript_collapse(0);
    session.ui_render_frame().unwrap();
    let expanded = body(&session);
    assert!(expanded.contains("▾ ["), "override re-opens the turn: {expanded}");
    std::fs::remove_dir_all(&dir).ok();
}

/// A context-observing module with a chosen ORIGIN: trusted origins see the
/// transcript, untrusted ones must not (decision 32 amendment).
fn origin_context_module(
    name: &str,
    component: &str,
    slot: &str,
    origin: ModuleOrigin,
) -> PackageManifest {
    PackageManifest {
        schema: kanbei_modules::PACKAGE_SCHEMA,
        module_id: Id128::generate(),
        origin,
        trust_class: TrustClass::Builtin,
        scope: kanbei_services::ScopePath(vec!["root".into()]),
        deps: Vec::new(),
        capabilities: Vec::new(),
        source: format!(
            r#"
function kb_on_activate(ctx)
  ctx.contribution_publish('{{"kind":"ui","name":"{name}","component":"{component}","slot":"{slot}"}}')
end
function kb_hot(d)
  if d.entry == "ui_reduce" then
    return {{ state = d.state, intents = {{}} }}
  elseif d.entry == "ui_render" then
    local c = d.context or {{}}
    local turns = 0
    if type(c.transcript) == "table" and type(c.transcript.turns) == "table" then
      turns = #c.transcript.turns
    end
    return {{ root = {{ id = "root", kind = "stack", children = {{
      {{ id = "{name}_status", kind = "text", spans = {{ {{ text = "{name}_status=" .. tostring(c.status) }} }} }},
      {{ id = "{name}_turns", kind = "text", spans = {{ {{ text = "{name}_turns=" .. tostring(turns) }} }} }},
      {{ id = "{name}_input", kind = "input", content = "" }},
    }} }} }}
  end
  error("unknown entry: " .. tostring(d.entry))
end
"#
        ),
        state_schema: None,
        state_key: None,
    }
}

/// Decision 32 amendment: the transcript carries prompts/model output, so only
/// trusted origins receive `context.transcript`; an untrusted mount keeps the
/// status/size/navigation but sees an empty transcript.
#[test]
fn render_context_transcript_is_trust_gated() {
    let (dir, mut session) = open("ui-context-trust");
    require_guest();
    session
        .activate_ui(origin_context_module(
            "trusted",
            "trusted_comp",
            "main",
            ModuleOrigin::UserConfig,
        ))
        .unwrap();
    session
        .activate_ui(origin_context_module(
            "untrusted",
            "untrusted_comp",
            "status",
            ModuleOrigin::WorkspaceConfig,
        ))
        .unwrap();
    session.append_user_message("secret prompt").unwrap();
    session.ui_render_frame().unwrap();
    let text = body(&session);
    assert!(
        text.contains("trusted_turns=1"),
        "a trusted mount sees the transcript: {text}"
    );
    assert!(
        text.contains("untrusted_turns=0"),
        "an untrusted mount gets an empty transcript: {text}"
    );
    assert!(
        text.contains("untrusted_status=idle"),
        "an untrusted mount keeps status/navigation: {text}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// A module whose reducer emits `toggle_collapse{0}` on the char `x` (only
/// when `emit`), and whose render echoes the turn-0 open state.
fn collapse_module(name: &str, component: &str, slot: &str, emit: bool) -> PackageManifest {
    PackageManifest {
        schema: kanbei_modules::PACKAGE_SCHEMA,
        module_id: Id128::generate(),
        origin: ModuleOrigin::UserConfig,
        trust_class: TrustClass::Builtin,
        scope: kanbei_services::ScopePath(vec!["root".into()]),
        deps: Vec::new(),
        capabilities: Vec::new(),
        source: format!(
            r#"
local EMIT = {emit}
function kb_on_activate(ctx)
  ctx.contribution_publish('{{"kind":"ui","name":"{name}","component":"{component}","slot":"{slot}"}}')
end
function kb_hot(d)
  if d.entry == "ui_reduce" then
    local intents = {{}}
    local e = d.event or {{}}
    if EMIT and e.kind == "char" and e.text == "x" then
      table.insert(intents, {{ kind = "toggle_collapse", turn = 0 }})
    end
    return {{ state = d.state, intents = intents }}
  elseif d.entry == "ui_render" then
    local c = d.context or {{}}
    local turns = {{}}
    if type(c.transcript) == "table" and type(c.transcript.turns) == "table" then
      turns = c.transcript.turns
    end
    local open = "nil"
    if type(turns[1]) == "table" then open = tostring(turns[1].open == true) end
    return {{ root = {{ id = "root", kind = "stack", children = {{
      {{ id = "{name}_open", kind = "text", spans = {{ {{ text = "{name}_open=" .. open }} }} }},
      {{ id = "{name}_input", kind = "input", content = "" }},
    }} }} }}
  end
  error("unknown entry: " .. tostring(d.entry))
end
"#
        ),
        state_schema: None,
        state_key: None,
    }
}

/// Decision 32 amendment: a module's `toggle_collapse` intent is scoped to the
/// emitting mount and is presentation-only — it never toggles another mount's
/// or the kernel's overrides.
#[test]
fn collapse_intent_is_mount_scoped() {
    let (dir, mut session) = open("ui-collapse-scope");
    require_guest();
    session
        .activate_ui(collapse_module("a", "comp_a", "main", true))
        .unwrap();
    session
        .activate_ui(collapse_module("b", "comp_b", "status", false))
        .unwrap();
    session.append_user_message("hi").unwrap();
    // Settle turn 0 so it is collapsed by default.
    session.finalize_transcript_turn();
    session.ui_render_frame().unwrap();
    let before = body(&session);
    assert!(before.contains("a_open=false"), "settled turn collapsed: {before}");

    // The char reaches both reducers; only mount `a` emits the toggle.
    session.ui_handle_input(b"x").unwrap();
    session.ui_render_frame().unwrap();
    let after = body(&session);
    assert!(
        after.contains("a_open=true"),
        "the emitter toggles its own collapse: {after}"
    );
    assert!(
        after.contains("b_open=false"),
        "another mount is unaffected: {after}"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Decision 32 amendment: a context component change (here `focus`) dirties
/// the mount that renders it, so the module does not show stale context.
#[test]
fn context_change_dirties_the_mount() {
    let (dir, mut session) = open("ui-context-dirty");
    require_guest();
    session.activate_ui(context_module()).unwrap();
    session.ui_render_frame().unwrap();
    let before = session.ui().unwrap().mounts[0].render_calls;
    // Tab focuses the module's input node: context.focus changes.
    session.ui_handle_input(b"\t").unwrap();
    session.ui_render_frame().unwrap();
    let after = session.ui().unwrap().mounts[0].render_calls;
    assert_eq!(after, before + 1, "a context change recomposes the mount");
    std::fs::remove_dir_all(&dir).ok();
}

/// Item 3: Enter on a focused NON-button submits (only a focused button
/// activates), so the composer keeps working once something is focused.
#[test]
fn enter_on_focused_input_submits() {
    use kanbei_transcript::CollapseOverrides;
    let (dir, mut session) = open("ui-enter-submit");
    require_guest();
    session.activate_builtin_ui().unwrap();
    session.ui_render_frame().unwrap();
    session.ui_handle_input(b"hello").unwrap();
    session.ui_mut().unwrap().focus.focused = Some("input".into());
    let out = session.ui_handle_input(b"\r").unwrap();
    assert!(out.submitted, "Enter on the focused input must submit");
    assert_eq!(
        session.transcript_view(&CollapseOverrides::new()).turns.len(),
        1
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Item 9: a paste split across reads keeps paste mode (the decoder is not
/// flushed per read), so its newline stays draft content and does not submit.
#[test]
fn paste_split_across_reads_keeps_paste_mode() {
    use kanbei_transcript::CollapseOverrides;
    let (dir, mut session) = open("ui-paste-split");
    require_guest();
    session.activate_builtin_ui().unwrap();
    session.ui_render_frame().unwrap();
    let out = session.ui_handle_input(b"\x1b[200~a").unwrap();
    assert!(!out.submitted, "paste start is not a submit");
    let out = session.ui_handle_input(b"\r\nb\x1b[201~").unwrap();
    assert!(
        !out.submitted,
        "a newline inside a paste must not submit: {out:?}"
    );
    assert_eq!(
        session.transcript_view(&CollapseOverrides::new()).turns.len(),
        0,
        "a paste must not open a turn"
    );
    std::fs::remove_dir_all(&dir).ok();
}

/// Item 8: backspace deletes a whole UTF-8 char, never a byte slice.
#[test]
fn backspace_is_char_safe() {
    let (dir, mut session) = open("ui-backspace");
    require_guest();
    session.activate_builtin_ui().unwrap();
    session.ui_render_frame().unwrap();
    session.ui_handle_input("aé".as_bytes()).unwrap();
    session.ui_handle_input(b"\x7f").unwrap();
    session.ui_render_frame().unwrap();
    let frame = session.ui().unwrap().last_frame().unwrap().clone();
    assert_eq!(input_row(&frame), "❯ a", "backspace removed the whole char");
    std::fs::remove_dir_all(&dir).ok();
}

/// Item 10: a long transcript is windowed so the shell never exceeds the
/// kernel's tree-node bound (which would degrade the mount to a placeholder).
#[test]
fn long_transcript_is_windowed() {
    let (dir, mut session) = open("ui-long-transcript");
    require_guest();
    session.activate_builtin_ui().unwrap();
    // >1366 turns × 3 nodes reach MAX_TREE_NODES; the window cap must hold.
    for i in 0..1500 {
        session.append_user_message(&format!("m{i}")).unwrap();
    }
    session.ui_render_frame().unwrap();
    assert!(
        !session.ui().unwrap().degraded,
        "a long transcript must not degrade the shell"
    );
    let text = body(&session);
    assert!(text.contains("m1499"), "the newest turn renders: {text}");
    std::fs::remove_dir_all(&dir).ok();
}
