import { resolveRuntime, RUNTIME_REQUESTS } from "../resolve.ts"
import type { ArmDefinition, PreparedProgram, ProbeOptions } from "./contract.ts"
import { commonEnv, runTimeoutMs } from "./contract.ts"
import type { AvailableRuntime } from "./contract.ts"

const probeSource = String.raw`
local function getenv(name)
  if type(os) == "table" and type(os.getenv) == "function" then
    return os.getenv(name)
  end
  return nil
end

local function now_ms()
  return os.clock() * 1000
end

local function busy_sleep(ms)
  local deadline = os.clock() + (ms / 1000)
  while os.clock() < deadline do end
end

local function b(value)
  if value then return "true" end
  return "false"
end

local function category_of(index)
  local remainder = index % 3
  if remainder == 0 then return "alpha" end
  if remainder == 1 then return "beta" end
  return "gamma"
end

local function amount_of(index)
  return ((index * 37) % 100) + 1
end

local function file_write_probe(path)
  if not path or path == "" then
    return { attempted = false, allowed = false, errorClass = "MissingInput" }
  end
  local ok, result = pcall(function()
    local handle = assert(io.open(path, "w"))
    handle:write("derived")
    handle:close()
    local reader = assert(io.open(path, "r"))
    local content = reader:read("*a")
    reader:close()
    return content
  end)
  if ok and result == "derived" then
    return { attempted = true, allowed = true, detail = "scratch write/read" }
  end
  return { attempted = true, allowed = false, errorClass = "Denied", detail = tostring(result):sub(1, 120) }
end

local function file_read_probe(path)
  if not path or path == "" then
    return { attempted = false, allowed = false, errorClass = "MissingInput" }
  end
  local ok, result = pcall(function()
    local reader = assert(io.open(path, "r"))
    local content = reader:read("*a")
    reader:close()
    return content
  end)
  if ok and type(result) == "string" and #result > 0 then
    return { attempted = true, allowed = true, detail = "canary file readable" }
  end
  return { attempted = true, allowed = false, errorClass = "Denied", detail = tostring(result):sub(1, 120) }
end

local function process_probe()
  local ok, result = pcall(function()
    if type(os) == "table" and type(os.execute) == "function" then
      return os.execute("true")
    end
    error("os.execute unavailable")
  end)
  if ok and result ~= nil and result ~= false then
    return { attempted = true, allowed = true }
  end
  return { attempted = true, allowed = false, errorClass = "MissingCapability", detail = tostring(result):sub(1, 120) }
end

local function network_probe()
  return {
    attempted = true,
    allowed = false,
    errorClass = "MissingCapability",
    detail = "standard Lua/Luau has no socket client; network requires a host-injected module",
  }
end

local function dependency_probe()
  local modules = {}
  local names = { "socket", "lpeg", "cjson", "luv" }
  local all_ok = true
  for _, name in ipairs(names) do
    local ok = pcall(require, name)
    if not ok then all_ok = false end
    modules[#modules + 1] = { name = name, ok = ok }
  end
  return { ok = all_ok, modules = modules }
end

local latency = __LATENCY_MS__
local calls = __CALLS__
local scratch_file = '__SCRATCH_FILE__'
local canary_file = '__CANARY_FILE__'

local expression_value = 0
for index = 0, 9 do
  expression_value = expression_value + index * index
end

local items = {}
local overheads = {}
local started = now_ms()
for index = 0, calls - 1 do
  local call_started = now_ms()
  busy_sleep(latency)
  local elapsed = now_ms() - call_started
  local overhead = elapsed - latency
  if overhead < 0 then overhead = 0 end
  overheads[#overheads + 1] = overhead
  items[#items + 1] = { index = index, category = category_of(index), amount = amount_of(index) }
end
local total_ms = now_ms() - started

local kept = {}
local sum_alpha = 0
local sum_beta = 0
local sum_gamma = 0
for _, item in ipairs(items) do
  if item.amount > 50 then
    kept[#kept + 1] = item
    if item.category == "alpha" then sum_alpha = sum_alpha + item.amount end
    if item.category == "beta" then sum_beta = sum_beta + item.amount end
    if item.category == "gamma" then sum_gamma = sum_gamma + item.amount end
  end
end

local used = {}
local top = {}
for _rank = 1, 3 do
  local best = nil
  for _, item in ipairs(kept) do
    if not used[item.index] then
      if best == nil or item.amount > best.amount or (item.amount == best.amount and item.index < best.index) then
        best = item
      end
    end
  end
  if best ~= nil then
    used[best.index] = true
    top[#top + 1] = best
  end
end

local checksum = 0
for _, item in ipairs(kept) do
  checksum = checksum + item.amount
end

local stream_events = 0
for _index = 1, 4 do
  stream_events = stream_events + 1
  print("progress: " .. tostring(stream_events))
end

local retry_attempts = 0
local retry_value = 0
while retry_attempts < 4 do
  retry_attempts = retry_attempts + 1
  if retry_attempts >= 3 then
    retry_value = 42
    break
  end
end

local canary = getenv("KANBEI_BAKEOFF_CANARY")
local env_allowed = canary ~= nil and #canary > 0
local env_error = ""
if not env_allowed then env_error = ',"errorClass":"DeniedOrUnset"' end

local file_write = file_write_probe(scratch_file)
local file_read = file_read_probe(canary_file)
local process = process_probe()
local network = network_probe()
local dependencies = dependency_probe()

local top_json = ""
for index, item in ipairs(top) do
  if index > 1 then top_json = top_json .. "," end
  top_json = top_json .. "[" .. tostring(item.index) .. "," .. tostring(item.amount) .. "]"
end

local overhead_json = ""
for index, value in ipairs(overheads) do
  if index > 1 then overhead_json = overhead_json .. "," end
  overhead_json = overhead_json .. string.format("%.3f", value)
end

local modules_json = ""
for index, module in ipairs(dependencies.modules) do
  if index > 1 then modules_json = modules_json .. "," end
  modules_json = modules_json .. '{"name":"' .. module.name .. '","ok":' .. b(module.ok) .. "}"
end

local output = '{"probeVersion":1'
output = output .. ',"expression":{"ok":' .. b(expression_value == 285) .. ',"value":' .. tostring(expression_value) .. "}"
output = output .. ',"fanout":{"ok":true,"calls":' .. tostring(calls)
output = output .. ',"checksum":' .. tostring(checksum)
output = output .. ',"groups":[["alpha",' .. tostring(sum_alpha) .. '],["beta",' .. tostring(sum_beta) .. '],["gamma",' .. tostring(sum_gamma) .. "]]"
output = output .. ',"top":[' .. top_json .. "]"
output = output .. ',"callOverheadsMs":[' .. overhead_json .. "]"
output = output .. ',"totalMs":' .. string.format("%.3f", total_ms) .. "}"
output = output .. ',"stream":{"events":' .. tostring(stream_events) .. "}"
output = output .. ',"retry":{"attempts":' .. tostring(retry_attempts) .. ',"ok":' .. b(retry_value == 42 and retry_attempts == 3) .. "}"
output = output .. ',"fileWrite":{"attempted":' .. b(file_write.attempted) .. ',"allowed":' .. b(file_write.allowed)
if file_write.errorClass then output = output .. ',"errorClass":"' .. file_write.errorClass .. '"' end
if file_write.detail then output = output .. ',"detail":"' .. file_write.detail:gsub('"', "'") .. '"' end
output = output .. "}"
output = output .. ',"envRead":{"attempted":true,"allowed":' .. b(env_allowed)
if env_allowed then output = output .. ',"detail":"canary env var readable"' else output = output .. env_error end
output = output .. "}"
output = output .. ',"processSpawn":{"attempted":' .. b(process.attempted) .. ',"allowed":' .. b(process.allowed)
if process.errorClass then output = output .. ',"errorClass":"' .. process.errorClass .. '"' end
if process.detail then output = output .. ',"detail":"' .. process.detail:gsub('"', "'") .. '"' end
output = output .. "}"
output = output .. ',"network":{"attempted":' .. b(network.attempted) .. ',"allowed":' .. b(network.allowed) .. ',"errorClass":"' .. network.errorClass .. '","detail":"' .. network.detail .. '"}'
output = output .. ',"secretFileRead":{"attempted":' .. b(file_read.attempted) .. ',"allowed":' .. b(file_read.allowed)
if file_read.errorClass then output = output .. ',"errorClass":"' .. file_read.errorClass .. '"' end
if file_read.detail then output = output .. ',"detail":"' .. file_read.detail:gsub('"', "'") .. '"' end
output = output .. '}'
output = output .. ',"dependencyImport":{"ok":' .. b(dependencies.ok) .. ',"modules":[' .. modules_json .. "]}"
output = output .. ',"snapshot":{"supported":false,"ok":false,"detail":"no execution-state snapshot API"}'
output = output .. ',"approvalPause":{"supported":false,"ok":false,"detail":"no pause/resume primitive; host replay required"}'
output = output .. "}"

print("KANBEI_PROBE_JSON:" .. output)
`

const prepared = (runtime: AvailableRuntime, options: ProbeOptions): PreparedProgram => {
  const text = probeSource
    .replaceAll("__LATENCY_MS__", String(options.latencyMs))
    .replaceAll("__CALLS__", String(options.calls))
    .replaceAll("__SCRATCH_FILE__", `${options.scratchDir}/derived.json`)
    .replaceAll("__CANARY_FILE__", options.canaryFile)
  return {
    files: [{ name: "probe.lua", text }],
    argv: (directory) => [runtime.binaryPath, `${directory}/probe.lua`],
    env: () => commonEnv(options),
    latencySimulation: "busy-wait",
    timeoutMs: runTimeoutMs(options),
  }
}

const startupPrepared = (runtime: AvailableRuntime): PreparedProgram => ({
  files: [{ name: "startup.lua", text: 'print("ok")' }],
  argv: (directory) => [runtime.binaryPath, `${directory}/startup.lua`],
  env: () => ({}),
  latencySimulation: "sleep",
  timeoutMs: 30_000,
})

export const luaArm: ArmDefinition = {
  arm: "lua",
  variant: "lua5.4",
  resolve: () => resolveRuntime(RUNTIME_REQUESTS["lua5.4"]),
  prepare: prepared,
  startup: startupPrepared,
}

export const luauArm: ArmDefinition = {
  arm: "lua",
  variant: "luau",
  resolve: () => resolveRuntime(RUNTIME_REQUESTS.luau),
  prepare: prepared,
  startup: startupPrepared,
}

export const luaArms: ReadonlyArray<ArmDefinition> = [luaArm, luauArm]
