//! Decision 29: module-published keybindings round-trip through
//! `contribution_publish` → the registry and drive input dispatch in
//! `ui_handle_input`; reserved keys (Suspend/SafeModeChord/ModalEscape) still
//! win and cannot be rebound.

mod common;

use kanbei_capabilities::TrustClass;
use kanbei_core::id::Id128;
use kanbei_modules::package::{ModuleOrigin, PackageManifest};
use kanbei_session::Session;

use common::{open, require_guest};

/// A UI fixture that publishes bindings in `kb_on_activate` and records every
/// routed event in a `seen` list rendered into the frame, so dispatch is
/// observable from the rendered body.
fn binding_module() -> PackageManifest {
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
  ctx.contribution_publish('{"kind":"ui","name":"kb","component":"kb_comp","slot":"main"}')
  ctx.contribution_publish('{"kind":"keymap","bindings":[{"key":"g","context":"always","action":"cmd_g"},{"key":"ctrl-c","context":"always","action":"cmd_c"},{"key":"ctrl-z","context":"always","action":"cmd_z"}]}')
end
local function fresh() return { seen = {} } end
function kb_hot(d)
  if d.entry == "ui_reduce" then
    local s = d.state
    if type(s) ~= "table" then s = fresh() end
    local e = d.event or {}
    if e.kind == "command" then
      table.insert(s.seen, "command:" .. tostring(e.action))
    elseif e.kind == "char" then
      table.insert(s.seen, "char:" .. tostring(e.text))
    end
    return { state = s, intents = {} }
  elseif d.entry == "ui_render" then
    local s = d.state
    if type(s) ~= "table" then s = fresh() end
    local items = {}
    for _, k in ipairs(s.seen or {}) do
      table.insert(items, { id = "s" .. #items, label = tostring(k) })
    end
    return { root = { id = "root", kind = "stack", children = {
      { id = "title", kind = "text", spans = { { text = "kb" } } },
      { id = "events", kind = "list", items = items },
      { id = "kb_input", kind = "input", content = "" },
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

/// The visible body text of the last rendered frame.
fn body(session: &Session) -> String {
    let frame = session.ui().unwrap().last_frame().unwrap().clone();
    (0..frame.rows - 2)
        .map(|r| frame.row_text(r))
        .collect::<Vec<_>>()
        .join("|")
}

#[test]
fn keymap_dispatch_round_trips_and_respects_reserved() {
    let (dir, mut session) = open("keymap");
    require_guest();
    session.activate_ui(binding_module()).unwrap();
    session.ui_render_frame().unwrap();

    // A published binding routes its action id as a command intent (the
    // module sees `command:cmd_g`, not `char:g`).
    session.ui_handle_input(b"g").unwrap();
    session.ui_render_frame().unwrap();
    let text = body(&session);
    assert!(text.contains("command:cmd_g"), "binding drove dispatch: {text}");
    assert!(!text.contains("char:g"), "bound key is not raw-forwarded: {text}");

    // CancelRun is remappable: a binding claiming Ctrl-C is honored.
    session.ui_handle_input(b"\x03").unwrap();
    session.ui_render_frame().unwrap();
    let text = body(&session);
    assert!(text.contains("command:cmd_c"), "Ctrl-C binding honored: {text}");

    // Suspend (Ctrl-Z) is reserved: the binding claiming it is ignored and the
    // kernel takes the key.
    let outcome = session.ui_handle_input(b"\x1a").unwrap();
    assert!(outcome.suspend, "Ctrl-Z reserved for the kernel");
    session.ui_render_frame().unwrap();
    let text = body(&session);
    assert!(
        !text.contains("command:cmd_z"),
        "reserved Ctrl-Z cannot be rebound: {text}"
    );

    // An unbound key still falls through to raw forwarding.
    session.ui_handle_input(b"h").unwrap();
    session.ui_render_frame().unwrap();
    let text = body(&session);
    assert!(text.contains("char:h"), "unbound key forwards: {text}");

    std::fs::remove_dir_all(&dir).ok();
}

/// A UI fixture for dispatch tests: publishes a root-scope mount plus (when
/// `publish`) a `g -> cmd_g` binding and a `ctrl-c -> cancel_run` binding, and
/// records every reducer event in a `seen` list. `flaky` traps on the char "x"
/// so the mount can be degraded (R-27 class 2).
fn dispatch_module(name: &str, component: &str, slot: &str, publish: bool, flaky: bool) -> PackageManifest {
    PackageManifest {
        schema: kanbei_modules::PACKAGE_SCHEMA,
        module_id: Id128::generate(),
        origin: ModuleOrigin::UserConfig,
        trust_class: TrustClass::Builtin,
        scope: kanbei_services::ScopePath(vec!["root".into()]),
        deps: Vec::new(),
        capabilities: Vec::new(),
        source: r#"
local FLAKY = {FLAKY}
function kb_on_activate(ctx)
  ctx.contribution_publish('{"kind":"ui","name":"{NAME}","component":"{COMPONENT}","slot":"{SLOT}"}')
  if {PUBLISH} then
    ctx.contribution_publish('{"kind":"keymap","bindings":[{"key":"g","context":"always","action":"cmd_g"},{"key":"ctrl-c","context":"always","action":"cancel_run"}]}')
  end
end
local function fresh() return { seen = {} } end
function kb_hot(d)
  if d.entry == "ui_reduce" then
    local s = d.state
    if type(s) ~= "table" then s = fresh() end
    local e = d.event or {}
    if FLAKY and e.kind == "char" and e.text == "x" then error("boom") end
    if e.kind == "command" then
      table.insert(s.seen, "command:" .. tostring(e.action))
    elseif e.kind == "char" then
      table.insert(s.seen, "char:" .. tostring(e.text))
    end
    return { state = s, intents = {} }
  elseif d.entry == "ui_render" then
    local s = d.state
    if type(s) ~= "table" then s = fresh() end
    local items = {}
    for _, k in ipairs(s.seen or {}) do
      table.insert(items, { id = "s" .. #items, label = tostring(k) })
    end
    return { root = { id = "root", kind = "stack", children = {
      { id = "title", kind = "text", spans = { { text = "panel {NAME}" } } },
      { id = "events", kind = "list", items = items },
      { id = "{NAME}_input", kind = "input", content = "" },
    } } }
  end
  error("unknown entry: " .. tostring(d.entry))
end
"#
        .replace("{NAME}", name)
        .replace("{COMPONENT}", component)
        .replace("{SLOT}", slot)
        .replace("{PUBLISH}", if publish { "true" } else { "false" })
        .replace("{FLAKY}", if flaky { "true" } else { "false" })
        .to_string(),
        state_schema: None,
        state_key: None,
    }
}

/// Item 7: a winning binding's command is delivered ONLY to the focused
/// (target) mount; another mount's reducer must not act on its action id.
#[test]
fn command_delivers_only_to_the_target_mount() {
    let (dir, mut session) = open("keymap-target");
    require_guest();
    session
        .activate_ui(dispatch_module("a", "a_comp", "main", true, false))
        .unwrap();
    session
        .activate_ui(dispatch_module("b", "b_comp", "status", false, false))
        .unwrap();
    session.ui_render_frame().unwrap();

    // Focus the second mount (composite id prefix 1).
    session.ui_mut().unwrap().focus.focused = Some("1.b_input".to_string());
    session.ui_handle_input(b"g").unwrap();
    session.ui_render_frame().unwrap();
    let text = body(&session);
    assert_eq!(
        text.matches("command:cmd_g").count(),
        1,
        "only the target mount's reducer acts: {text}"
    );

    std::fs::remove_dir_all(&dir).ok();
}

/// Item 8: a binding owned by a degraded mount is skipped, so the key falls
/// through to forwarding instead of being swallowed by the dead reducer.
#[test]
fn degraded_mount_binding_falls_through_to_forwarding() {
    let (dir, mut session) = open("keymap-degraded");
    require_guest();
    session
        .activate_ui(dispatch_module("a", "a_comp", "main", true, false))
        .unwrap();
    session
        .activate_ui(dispatch_module("b", "b_comp", "status", true, true))
        .unwrap();
    session.ui_render_frame().unwrap();

    // Focus the flaky mount and trip it (no binding for "x", so it forwards).
    session.ui_mut().unwrap().focus.focused = Some("1.b_input".to_string());
    session.ui_handle_input(b"x").unwrap();
    assert!(session.ui().unwrap().degraded, "the flaky mount degraded");

    // "g" is bound, but its owner is degraded: it must forward, not dispatch.
    session.ui_handle_input(b"g").unwrap();
    session.ui_render_frame().unwrap();
    let text = body(&session);
    assert!(text.contains("char:g"), "degraded binding falls through: {text}");
    assert!(!text.contains("command:cmd_g"), "no command from a dead mount: {text}");

    std::fs::remove_dir_all(&dir).ok();
}

/// Item 10: the built-in `cancel_run` action id is wired to the session's
/// existing cancel path (kernel-handled, never delivered as a module command).
#[test]
fn cancel_run_binding_is_kernel_handled() {
    let (dir, mut session) = open("keymap-cancel");
    require_guest();
    session
        .activate_ui(dispatch_module("a", "a_comp", "main", true, false))
        .unwrap();
    session.ui_render_frame().unwrap();

    let outcome = session.ui_handle_input(b"\x03").unwrap();
    assert!(outcome.repaint, "the kernel cancel path repaints");
    session.ui_render_frame().unwrap();
    let text = body(&session);
    assert!(
        !text.contains("command:cancel_run"),
        "cancel_run is kernel-handled, not fan-out to a reducer: {text}"
    );

    std::fs::remove_dir_all(&dir).ok();
}
