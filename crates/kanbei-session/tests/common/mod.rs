//! Shared helpers for the M8 multi-module UI tests (ui_multi.rs,
//! ui_latency.rs): guest wasm availability check, session opening, UI
//! fixture module manifests, and canonical envelope reading.

use std::path::PathBuf;

use kanbei_capabilities::TrustClass;
use kanbei_core::envelope::Envelope;
use kanbei_core::id::Id128;
use kanbei_log::for_each_frame;
use kanbei_modules::package::{ModuleOrigin, PackageManifest};
use kanbei_session::{Session, SessionConfig};
use kanbei_vm::{GuestError, Vm, VmConfig};

/// NO_EPOCH engine: the workbench modules need fuel beyond the default 1M
/// per call (M2 recipe; same as the M5 gate).
pub fn engine() -> VmConfig {
    VmConfig {
        fuel_per_call: u64::MAX,
        epoch_deadline: u64::MAX / 2,
        ..Default::default()
    }
}

/// Module tests need the guest wasm; without it they skip with a note.
pub fn require_guest() -> bool {
    match Vm::load(engine()) {
        Ok(_) => true,
        Err(GuestError::NotBuilt) => {
            eprintln!(
                "skip: guest wasm not built (run `cargo build -p kanbei-guest \
                 --target wasm32-wasip1 --release`)"
            );
            false
        }
        Err(e) => panic!("Vm::load failed: {e}"),
    }
}

pub fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kanbei-m8-{tag}-{}-{}",
        std::process::id(),
        Id128::generate()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

pub fn open(tag: &str) -> (PathBuf, Session) {
    let dir = tempdir(tag);
    let session = Session::open(SessionConfig {
        dir: dir.clone(),
        stream: format!("m8-{tag}"),
        engine: Some(engine()),
        ..Default::default()
    })
    .unwrap();
    (dir, session)
}

/// A UI fixture module (M8): publishes a root-scope UI mount in `slot`,
/// records every received event in a `seen` list (fan-out observability),
/// drafts text only when the event's `target` is its own slot, submits the
/// draft on Enter (target-gated), and renders a panel: title text, the seen
/// list, a focusable input, and a focusable button. `flaky` traps on the
/// char "x" (runtime component fault, R-27 class 2).
pub fn ui_module(
    name: &str,
    component: &str,
    slot: &str,
    trust: TrustClass,
    flaky: bool,
) -> PackageManifest {
    let source = r#"
local SLOT = "{SLOT}"
local FLAKY = {FLAKY}
function kb_on_activate(ctx)
  ctx.contribution_publish('{"kind":"ui","name":"{NAME}","component":"{COMPONENT}","slot":"{SLOT}"}')
end
local function fresh() return { draft = "", seen = {} } end
function kb_hot(d)
  if d.entry == "ui_reduce" then
    local s = d.state
    if type(s) ~= "table" then s = fresh() end
    local e = d.event or {}
    if FLAKY and e.kind == "char" and e.text == "x" then error("boom") end
    local mine = e.target == SLOT
    if e.kind == "char" and type(e.text) == "string" then
      if mine then s.draft = (s.draft or "") .. e.text end
      table.insert(s.seen, "char:" .. e.text)
    elseif e.kind == "backspace" then
      if mine then s.draft = string.sub(s.draft or "", 1, #(s.draft or "") - 1) end
    elseif e.kind == "enter" or (e.kind == "activate" and e.node == "{NAME}_input") then
      if mine and #(s.draft or "") > 0 then
        local text = s.draft
        s.draft = ""
        return { state = s, intents = { { kind = "submit_text", text = text } } }
      end
    elseif e.kind == "refresh" then
      table.insert(s.seen, "refresh:" .. tostring((e.facts or {}).last_outcome or ""))
    end
    return { state = s, intents = {} }
  elseif d.entry == "ui_render" then
    local s = d.state
    if type(s) ~= "table" then s = fresh() end
    local lines = {}
    for _, k in ipairs(s.seen or {}) do
      table.insert(lines, { id = "seen_" .. #lines, kind = "list_item", content = tostring(k) })
    end
    return { root = { id = "root", kind = "root", children = {
      { id = "title", kind = "text", content = "panel {NAME}" },
      { id = "events", kind = "list", children = lines },
      { id = "{NAME}_input", kind = "input", content = tostring(s.draft or ""), focusable = true },
      { id = "{NAME}_btn", kind = "button", content = "{NAME} button", focusable = true },
    } } }
  end
  error("unknown entry: " .. tostring(d.entry))
end
"#
    .replace("{NAME}", name)
    .replace("{COMPONENT}", component)
    .replace("{SLOT}", slot)
    .replace("{FLAKY}", if flaky { "true" } else { "false" });
    PackageManifest {
        schema: 1,
        module_id: Id128::generate(),
        origin: ModuleOrigin::UserConfig,
        trust_class: trust,
        scope: kanbei_services::ScopePath(vec!["root".into()]),
        deps: Vec::new(),
        capabilities: Vec::new(),
        source,
        state_schema: None,
    }
}

/// A plain module for generation replacement: mounts nothing (mid-session UI
/// deactivation, M8).
pub fn plain_module() -> PackageManifest {
    PackageManifest {
        schema: 1,
        module_id: Id128::generate(),
        origin: ModuleOrigin::UserConfig,
        trust_class: TrustClass::Builtin,
        scope: kanbei_services::ScopePath(vec!["root".into()]),
        deps: Vec::new(),
        capabilities: Vec::new(),
        source: r#"
function kb_on_activate(ctx) end
function kb_hot(d) error("plain module has no entries") end
"#
        .to_string(),
        state_schema: None,
    }
}

/// Whether the session log contains a `user_message` envelope whose text is
/// exactly `text`.
pub fn has_user_message(dir: &PathBuf, text: &str) -> bool {
    let mut found = false;
    for_each_frame(&dir.join("log.zst"), |frame| {
        for line in &frame.events {
            let Ok(env) = Envelope::from_line(line) else {
                continue;
            };
            if env.kind == "user_message"
                && env
                    .payload
                    .get("text")
                    .and_then(|t| t.as_str())
                    .is_some_and(|t| t == text)
            {
                found = true;
            }
        }
    })
    .unwrap();
    found
}

/// The visible text of one frame row (test helper).
pub fn input_row(frame: &kanbei_ui::TerminalFrame) -> String {
    frame.row_text(frame.rows - 1)
}
