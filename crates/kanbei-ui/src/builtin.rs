//! The built-in workbench UI: one immutable Luau module generation authored
//! through the standard contribution contract (architecture.md M5). The
//! kernel hosts this source like any other module package; it publishes a UI
//! mount + theme overlay on activation and implements the `ui_reduce` /
//! `ui_render` entries of the `kb_hot` dispatcher.
//!
//! The module composes the Maki-shaped shell (decision 5): the transcript
//! region is built from the kernel-owned render context (decision 32), the
//! composer input and status line are shell contributions, and slots for
//! switchable subagent chats, pickers, and a plan panel are laid out when the
//! reducer state populates them. Layout, z-order, keys and theming stay the
//! module's (decision 8); the kernel owns rendering, focus and scrolling.
//!
//! Module contract (guest requirements): top-level code is pure (runs twice);
//! `kb_hot` is the single callable entry; `kb_on_activate(ctx)` runs through
//! the kernel's activation shim. Deterministic and total: no wall clock, and
//! any missing context/state field degrades to an empty value.
//!
//! ABI (internal/unstable, M5):
//! - `{"entry":"ui_reduce","state":<json|null>,"event":{"kind":
//!   "char"|"backspace"|"enter"|"activate"|"command"|"refresh", ...}}` →
//!   `{"state":<json>,"intents":[{"kind":"submit_text","text":...} |
//!   {"kind":"toggle_collapse","turn":n}]}`. A `command` event carries the
//!   `action` id of the winning keybinding (decision 29); a `refresh` event
//!   carries kernel facts.
//! - `{"entry":"ui_render","state":<json>,"context":{"transcript":
//!   <TranscriptView>,"status":<string>,"size":{"cols":<u16>,"rows":<u16>},
//!   "focus":<string|null>,"selection":<string|null>,"viewport_top":<u32>}}`
//!   → the semantic tree wire shape (see `crate::tree`). The context is
//!   kernel-owned and read-only (decision 32).

pub const BUILTIN_UI_NAME: &str = "workbench";
pub const BUILTIN_UI_COMPONENT: &str = "builtin_workbench";

pub const BUILTIN_UI_SOURCE: &str = r#"-- kanbei built-in workbench shell (M5/T12).
-- Reducer state: { draft, last_outcome, notices = {text,...}, plan = {line,...},
--   chats = {{id,label},...}, active_chat = n, picker = {{id,label},...},
--   picker_active = n }.
-- Render context (kernel-owned, read-only): { transcript = TranscriptView,
--   status = string, size = {cols,rows}, focus = string?, selection = string?,
--   viewport_top = n }. The shell derives the Maki frame from state + context;
--   no wall clock, so two renders of the same inputs agree.

local function empty_state()
  return {
    draft = "", last_outcome = "",
    notices = {}, plan = {}, chats = {}, active_chat = 1,
    picker = {}, picker_active = 1,
  }
end

local function str(v)
  if type(v) == "string" then return v end
  return ""
end

local function list(v)
  if type(v) == "table" then return v end
  return {}
end

-- Char-safe clamp: never split a UTF-8 sequence (the kernel counts chars).
local function clamp(s, max)
  if #s <= max then return s end
  local n = max
  while n > 0 do
    local b = string.byte(s, n + 1)
    if b < 128 or b >= 192 then break end
    n = n - 1
  end
  return string.sub(s, 1, n)
end

local function trunc(s, max)
  s = str(s)
  if #s <= max then return s end
  return clamp(s, max) .. "…"
end

-- Drop the last UTF-8 character: step over continuation bytes (0x80..0xBF) so
-- a backspace never splits a multibyte sequence (the kernel counts chars).
local function delete_last(s)
  s = str(s)
  local n = #s
  while n > 0 do
    local b = string.byte(s, n)
    if b < 128 or b >= 192 then break end
    n = n - 1
  end
  if n == 0 then return "" end
  return string.sub(s, 1, n - 1)
end

local function indent(text)
  return (string.gsub(str(text), "\n", "\n "))
end

local function txt(id, text, style)
  local node = { id = id, kind = "text", spans = { { text = text } } }
  if style then node.spans[1].style = style end
  return node
end

local function code(id, text)
  return { id = id, kind = "code", spans = { { text = text } } }
end

local function button(id, label)
  return { id = id, kind = "button", label = label }
end

local function state_symbol(s)
  if s == "Running" then return "…" end
  if s == "Completed" then return "✓" end
  if s == "Failed" then return "✗" end
  if s == "Blocked" then return "!" end
  return "?"
end

local function step_status(s)
  if s == "InFlight" then return "…" end
  if s == "Ok" then return "✓" end
  if s == "Interrupted" then return "✗" end
  return "?"
end

local function step_detail(st)
  local parts = {}
  if str(st.detail) ~= "" then table.insert(parts, str(st.detail)) end
  if str(st.error) ~= "" then table.insert(parts, str(st.error)) end
  if str(st.result) ~= "" then table.insert(parts, trunc(st.result, 200)) end
  return table.concat(parts, " · ")
end

local function step_line(st)
  local out = "  " .. step_status(st.status) .. " " .. str(st.tool) ..
    "(" .. trunc(st.args, 120) .. ")"
  local detail = step_detail(st)
  if detail ~= "" then out = out .. " — " .. trunc(detail, 160) end
  return out
end

local function turn_summary(t)
  local state = str(t.state)
  if state == "" then state = "Completed" end
  local out = "[" .. state_symbol(state) .. "] " ..
    tostring(tonumber(t.tools) or 0) .. " step(s), " ..
    tostring(tonumber(t.runs) or 0) .. " run(s), " ..
    tostring(tonumber(t.input_tokens) or 0) .. "+" ..
    tostring(tonumber(t.output_tokens) or 0) .. " tok"
  if state ~= "Completed" and type(t.reason) == "string" then
    out = out .. " — " .. trunc(t.reason, 160)
  end
  return trunc(out, 300)
end

-- Transcript node caps: a long session must never exceed the kernel's
-- MAX_TREE_NODES (4096) or the mount degrades to a placeholder. Render only
-- the tail turns and elide the rest, with a hard node budget as backstop.
local MAX_TURNS = 40
local MAX_THOUGHTS = 400
local MAX_TRANSCRIPT_NODES = 3000

-- The transcript region: turn rows, thought/tool rows, the live working
-- indicator, the settled collapse header (an activatable button), the answer,
-- and a divider. Collapse is kernel-applied: the module renders `turn.open`.
local function transcript_nodes(ctx)
  local turns = list(list(ctx.transcript).turns)
  local nodes = {}
  local total = #turns
  local first = 1
  if total > MAX_TURNS then first = total - MAX_TURNS + 1 end
  if first > 1 then
    table.insert(nodes, txt("elide_turns",
      "… " .. tostring(first - 1) .. " earlier turn(s)", "status"))
  end
  local full = false
  local function add(node)
    if #nodes >= MAX_TRANSCRIPT_NODES then
      full = true
      return
    end
    table.insert(nodes, node)
  end
  for n = first, total do
    if full then break end
    local t = turns[n]
    local open = t.open == true
    local state = str(t.state)
    local base = "t" .. (n - 1)
    add(txt(base .. "_u", "❯ " .. trunc(t.user, 4000), "user"))
    if open then
      local thoughts = list(t.thoughts)
      for k, row in ipairs(thoughts) do
        if k > MAX_THOUGHTS then
          add(txt(base .. "_more",
            "  … " .. tostring(#thoughts - MAX_THOUGHTS) .. " more", "status"))
          break
        end
        local id = base .. "_b" .. k
        if type(row.Text) == "string" then
          add(txt(id, "  " .. trunc(row.Text, 4000), "thought"))
        elseif type(row.Notice) == "string" then
          add(txt(id, "  " .. trunc(row.Notice, 4000), "status"))
        elseif type(row.Step) == "table" then
          add(code(id, step_line(row.Step)))
        end
      end
    end
    if open then
      if type(t.streaming) == "string" then
        add(txt(base .. "_s", "  " .. trunc(t.streaming, 4000), "thought"))
      end
      if state == "Running" then
        add(txt(base .. "_p", "  … working", "progress"))
      end
    end
    if state ~= "Running" then
      local marker = "▸"
      if open then marker = "▾" end
      add(button(base, marker .. " " .. turn_summary(t)))
    end
    if type(t.response) == "string" then
      add(txt(base .. "_r", trunc(indent(t.response), 4000), "response"))
    end
    add(txt(base .. "_d", "──", "divider"))
  end
  if full then
    table.insert(nodes, txt("elide_nodes", "… transcript truncated", "status"))
  end
  return nodes
end

local function notice_items(s)
  local items = {}
  for i, text in ipairs(list(s.notices)) do
    table.insert(items, { id = "notice_" .. i, label = trunc(text, 300) })
  end
  return items
end

local function chat_items(s)
  local items = {}
  for i, c in ipairs(list(s.chats)) do
    local label = str(c.label)
    if label == "" then label = str(c.id) end
    if i == tonumber(s.active_chat) then label = "▸ " .. label end
    table.insert(items, { id = "chat_" .. i, label = trunc(label, 200) })
  end
  return items
end

local function picker_items(s)
  local items = {}
  for i, c in ipairs(list(s.picker)) do
    local label = str(c.label)
    if label == "" then label = str(c.id) end
    if i == tonumber(s.picker_active) then label = "▸ " .. label end
    table.insert(items, { id = "picker_" .. i, label = trunc(label, 200) })
  end
  return items
end

local function plan_nodes(s)
  local nodes = {}
  for i, line in ipairs(list(s.plan)) do
    table.insert(nodes, txt("plan_" .. i, trunc(line, 300), "status"))
  end
  return nodes
end

local function shell(s, ctx)
  ctx = ctx or {}
  local children = {}
  local status = str(ctx.status)
  if status == "" then status = "idle" end
  -- Header + status line, then the notice log, then the optional shell slots
  -- (chats, plan, pickers), the transcript region, and the composer input.
  table.insert(children, txt("shell_header", "kanbei · " .. status, "header"))
  table.insert(children, { id = "shell_notices", kind = "list", items = notice_items(s) })
  local chats = chat_items(s)
  if #chats > 0 then
    table.insert(children, txt("shell_chats_label", "chats", "status"))
    table.insert(children, { id = "shell_chats", kind = "list", items = chats })
  end
  local plan = plan_nodes(s)
  if #plan > 0 then
    table.insert(children, txt("shell_plan_label", "plan", "status"))
    for _, node in ipairs(plan) do table.insert(children, node) end
  end
  local picker = picker_items(s)
  if #picker > 0 then
    table.insert(children, { id = "shell_picker", kind = "list", items = picker })
  end
  table.insert(children, { id = "transcript", kind = "col", children = transcript_nodes(ctx) })
  -- The composer is the module's own row: a prompt span plus the input
  -- primitive, which the kernel lays out and places the caret in.
  table.insert(children, { id = "composer", kind = "row", children = {
    txt("composer_prompt", "❯"),
    { id = "input", kind = "input", content = str(s.draft) },
  } })
  return { root = { id = "root", kind = "stack", children = children } }
end

local function reduce(d)
  local s = d.state
  if type(s) ~= "table" then s = empty_state() end
  if type(s.notices) ~= "table" then s.notices = {} end
  if type(s.plan) ~= "table" then s.plan = {} end
  if type(s.chats) ~= "table" then s.chats = {} end
  if type(s.picker) ~= "table" then s.picker = {} end
  local e = d.event or {}
  local intents = {}
  local kind = e.kind
  if kind == "char" and type(e.text) == "string" then
    s.draft = str(s.draft) .. e.text
  elseif kind == "backspace" then
    s.draft = delete_last(s.draft)
  elseif kind == "enter" then
    local text = str(s.draft)
    s.draft = ""
    if #text > 0 then
      table.insert(intents, { kind = "submit_text", text = text })
    end
  elseif kind == "activate" then
    -- The only activatable node the shell authors is a settled turn's
    -- collapse header (`t<N>`); toggling is presentation-only.
    local turn = string.match(str(e.node), "^t(%d+)$")
    if turn then
      table.insert(intents, { kind = "toggle_collapse", turn = tonumber(turn) })
    end
  elseif kind == "refresh" then
    local facts = e.facts or {}
    local outcome = facts.last_outcome
    if type(outcome) == "string" and outcome ~= s.last_outcome then
      s.last_outcome = outcome
      table.insert(s.notices, outcome)
    end
  end
  return { state = s, intents = intents }
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

function kb_hot(dispatch)
  local entry = dispatch and dispatch.entry
  if entry == "ui_reduce" then
    return reduce(dispatch)
  elseif entry == "ui_render" then
    local s = dispatch.state
    if type(s) ~= "table" then s = empty_state() end
    return shell(s, dispatch.context)
  end
  error("unknown ui entry: " .. tostring(entry))
end
"#;
