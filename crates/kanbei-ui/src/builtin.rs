//! The built-in workbench UI: one immutable Luau module generation authored
//! through the standard contribution contract (architecture.md M5). The
//! kernel hosts this source like any other module package; it publishes a UI
//! mount + theme overlay on activation and implements the `ui_reduce` /
//! `ui_render` entries of the `kb_hot` dispatcher.
//!
//! Module contract (guest requirements): top-level code is pure (runs twice);
//! `kb_hot` is the single callable entry; `kb_on_activate(ctx)` runs through
//! the kernel's activation shim.
//!
//! ABI (internal/unstable, M5):
//! - `{"entry":"ui_reduce","state":<json|null>,"event":{"kind":
//!   "char"|"backspace"|"enter"|"activate"|"refresh", ...}}` →
//!   `{"state":<json>,"intents":[{"kind":"submit_text","text":...}]}`
//! - `{"entry":"ui_render","state":<json>}` → the semantic tree wire shape
//!   (see `crate::tree`).

pub const BUILTIN_UI_NAME: &str = "workbench";
pub const BUILTIN_UI_COMPONENT: &str = "builtin_workbench";

pub const BUILTIN_UI_SOURCE: &str = r#"-- kanbei built-in workbench UI (M5).
-- Reducer state: { draft = <string>, last_outcome = <string>, log = { {seq, text}, ... } }.
-- Kernel facts arrive with the refresh event; no UI gestures are canonical.

local function empty_state()
  return { draft = "", last_outcome = "", log = {} }
end

function kb_on_activate(ctx)
  ctx.contribution_publish('{"kind":"ui","name":"workbench","component":"builtin_workbench"}')
  ctx.contribution_publish(
    '{"kind":"theme","name":"default","overlay":{' ..
    '"header":{"fg":"bright_black","bold":true},' ..
    '"status":{"fg":"bright_black"},' ..
    '"input":{},' ..
    '"selected":{"reverse":true},' ..
    '"banner":{"fg":"black","bg":"bright_yellow","bold":true}' ..
    '}}')
end

local function tree_for(s)
  if type(s) ~= "table" then s = empty_state() end
  local lines = {}
  for _, entry in ipairs(s.log or {}) do
    table.insert(lines, { id = "log_" .. tostring(entry.seq), kind = "list_item", content = tostring(entry.text) })
  end
  local draft = s.draft
  if type(draft) ~= "string" then draft = "" end
  return {
    root = {
      id = "root", kind = "root",
      children = {
        { id = "header", kind = "header", content = "kanbei workbench" },
        { id = "log", kind = "list", children = lines },
        { id = "input", kind = "input", content = draft, focusable = true },
      },
    },
  }
end

function kb_hot(dispatch)
  local entry = dispatch and dispatch.entry
  if entry == "ui_reduce" then
    local s = dispatch.state
    if type(s) ~= "table" then s = empty_state() end
    local e = dispatch.event or {}
    local intents = {}
    local kind = e.kind
    if kind == "char" and type(e.text) == "string" then
      s.draft = s.draft .. e.text
    elseif kind == "backspace" then
      s.draft = string.sub(s.draft, 1, #s.draft - 1)
    elseif kind == "enter" then
      local text = s.draft
      s.draft = ""
      if #text > 0 then
        table.insert(intents, { kind = "submit_text", text = text })
      end
    elseif kind == "refresh" then
      local facts = e.facts or {}
      local outcome = facts.last_outcome
      if type(outcome) == "string" and outcome ~= s.last_outcome then
        s.last_outcome = outcome
        table.insert(s.log, { seq = #s.log + 1, text = outcome })
      end
    end
    return { state = s, intents = intents }
  elseif entry == "ui_render" then
    return tree_for(dispatch.state)
  end
  error("unknown ui entry: " .. tostring(entry))
end
"#;
