//! T12 context ABI (decision 32): the kernel hands the built-in shell a
//! read-only render context (transcript, status, size, focus, selection,
//! viewport) alongside the module's reducer state, and the shell composes the
//! Maki frame from it. Guest-wasm tests need the guest (require_guest; build
//! with `cargo xtask build-guest`).

mod common;

use kanbei_capabilities::TrustClass;
use kanbei_core::id::Id128;
use kanbei_modules::package::{ModuleOrigin, PackageManifest};
use kanbei_session::Session;

use common::{input_row, open, require_guest};

/// The visible body text of the last rendered frame (header row..status bar,
/// excluding the kernel status bar and input line).
fn body(session: &Session) -> String {
    let frame = session.ui().unwrap().last_frame().unwrap().clone();
    (0..frame.rows() - 2)
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
    assert_eq!(input_row(&frame), ">", "composer prompt with no draft");
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
