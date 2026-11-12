import { resolveRuntime, RUNTIME_REQUESTS } from "../resolve.ts"
import type { ArmDefinition, PreparedProgram, ProbeOptions } from "./contract.ts"
import { commonEnv, runTimeoutMs } from "./contract.ts"
import type { AvailableRuntime } from "./contract.ts"

const probeSourceRaw = String.raw`#!/usr/bin/env bash

latency="@@{KANBEI_LATENCY_MS:-10}"
calls="@@{KANBEI_CALLS:-50}"

now_us() {
  local value="$EPOCHREALTIME"
  value="@@{value/./}"
  echo $((10#$value))
}

amount_of() {
  echo $(( ($1 * 37 % 100) + 1 ))
}

category_of() {
  case $(( $1 % 3 )) in
    0) echo alpha ;;
    1) echo beta ;;
    *) echo gamma ;;
  esac
}

sleep_seconds() {
  printf '%d.%03d' $(( $1 / 1000 )) $(( $1 % 1000 ))
}

expression_value=0
for ((i = 0; i < 10; i++)); do
  expression_value=$(( expression_value + i * i ))
done

items_index=()
items_category=()
items_amount=()
overheads=()

started=$(now_us)
for ((i = 0; i < calls; i++)); do
  call_started=$(now_us)
  sleep "$(sleep_seconds "$latency")"
  call_finished=$(now_us)
  overhead=$(( (call_finished - call_started) / 1000 - latency ))
  if (( overhead < 0 )); then overhead=0; fi
  overheads+=("$overhead")
  items_index+=("$i")
  items_category+=("$(category_of "$i")")
  items_amount+=("$(amount_of "$i")")
done
finished=$(now_us)
total_ms=$(( (finished - started) / 1000 ))

sum_alpha=0
sum_beta=0
sum_gamma=0
kept_index=()
kept_amount=()
for ((i = 0; i < calls; i++)); do
  if (( items_amount[i] > 50 )); then
    case "@@{items_category[i]}" in
      alpha) sum_alpha=$(( sum_alpha + items_amount[i] )) ;;
      beta) sum_beta=$(( sum_beta + items_amount[i] )) ;;
      gamma) sum_gamma=$(( sum_gamma + items_amount[i] )) ;;
    esac
    kept_index+=("@@{items_index[i]}")
    kept_amount+=("@@{items_amount[i]}")
  fi
done

declare -A used_top=()
top_json=""
for ((rank = 0; rank < 3; rank++)); do
  best=-1
  best_amount=-1
  for ((k = 0; k < @@{#kept_index[@]}; k++)); do
    index="@@{kept_index[k]}"
    if [[ -n "@@{used_top[$index]:-}" ]]; then continue; fi
    amount="@@{kept_amount[k]}"
    if (( amount > best_amount || (amount == best_amount && (best == -1 || index < best)) )); then
      best="$index"
      best_amount="$amount"
    fi
  done
  if (( best >= 0 )); then
    used_top[$best]=1
    if [[ -n "$top_json" ]]; then top_json+=","; fi
    top_json+="[$best,$best_amount]"
  fi
done

checksum=0
for amount in "@@{kept_amount[@]}"; do
  checksum=$(( checksum + amount ))
done

stream_events=0
for ((i = 0; i < 4; i++)); do
  stream_events=$(( stream_events + 1 ))
  echo "progress: $stream_events"
done

retry_attempts=0
retry_value=0
while (( retry_attempts < 4 )); do
  retry_attempts=$(( retry_attempts + 1 ))
  if (( retry_attempts >= 3 )); then
    retry_value=42
    break
  fi
done

overheads_json=$(IFS=,; echo "@@{overheads[*]}")
expression_ok=false
if (( expression_value == 285 )); then expression_ok=true; fi
retry_ok=false
if (( retry_value == 42 && retry_attempts == 3 )); then retry_ok=true; fi

file_write_attempted=false
file_write_allowed=false
file_write_detail=""
file_write_error=""
if [[ -n "@@{KANBEI_SCRATCH_FILE:-}" ]]; then
  file_write_attempted=true
  if printf 'derived' > "$KANBEI_SCRATCH_FILE" 2>/dev/null && [[ "$(cat "$KANBEI_SCRATCH_FILE" 2>/dev/null)" == "derived" ]]; then
    file_write_allowed=true
    file_write_detail="scratch write/read"
  else
    file_write_error="Denied"
  fi
fi

canary="@@{KANBEI_BAKEOFF_CANARY:-}"
env_allowed=false
if [[ -n "$canary" ]]; then env_allowed=true; fi

process_allowed=false
if bash -c true 2>/dev/null; then process_allowed=true; fi

network_allowed=false
network_detail=""
if (exec 3<>/dev/tcp/127.0.0.1/1) 2>/dev/null; then
  network_allowed=true
  network_detail="loopback connect succeeded"
else
  network_allowed=true
  network_detail="connect refused; bash has no policy denial for /dev/tcp"
fi

dep_jq=false
dep_curl=false
dep_node=false
dep_python=false
command -v jq >/dev/null 2>&1 && dep_jq=true
command -v curl >/dev/null 2>&1 && dep_curl=true
command -v node >/dev/null 2>&1 && dep_node=true
command -v python3 >/dev/null 2>&1 && dep_python=true

file_read_attempted=false
file_read_allowed=false
file_read_detail=""
file_read_error=""
if [[ -n "@@{KANBEI_CANARY_FILE:-}" ]]; then
  file_read_attempted=true
  canary_contents="$(cat "$KANBEI_CANARY_FILE" 2>/dev/null)"
  if [[ -n "$canary_contents" ]]; then
    file_read_allowed=true
    file_read_detail="canary file readable"
  else
    file_read_error="Denied"
  fi
fi

modules_json="{\"name\":\"jq\",\"ok\":$dep_jq},{\"name\":\"curl\",\"ok\":$dep_curl},{\"name\":\"node\",\"ok\":$dep_node},{\"name\":\"python3\",\"ok\":$dep_python}"
dep_ok=false
if [[ "$dep_jq" == "true" && "$dep_curl" == "true" && "$dep_node" == "true" && "$dep_python" == "true" ]]; then dep_ok=true; fi

json='{"probeVersion":1'
json+=',"expression":{"ok":'$expression_ok',"value":'$expression_value'}'
json+=',"fanout":{"ok":true,"calls":'$calls',"checksum":'$checksum
json+=',"groups":[["alpha",'$sum_alpha'],["beta",'$sum_beta'],["gamma",'$sum_gamma']]'
json+=',"top":['"$top_json"']'
json+=',"callOverheadsMs":['"$overheads_json"']'
json+=',"totalMs":'$total_ms'}'
json+=',"stream":{"events":'$stream_events'}'
json+=',"retry":{"attempts":'$retry_attempts',"ok":'$retry_ok'}'
json+=',"fileWrite":{"attempted":'$file_write_attempted',"allowed":'$file_write_allowed
if [[ -n "$file_write_error" ]]; then json+=',"errorClass":"'$file_write_error'"'; fi
if [[ -n "$file_write_detail" ]]; then json+=',"detail":"'$file_write_detail'"'; fi
json+='}'
json+=',"envRead":{"attempted":true,"allowed":'$env_allowed
if [[ "$env_allowed" == "true" ]]; then json+=',"detail":"canary env var readable"'; else json+=',"errorClass":"DeniedOrUnset"'; fi
json+='}'
json+=',"processSpawn":{"attempted":true,"allowed":'$process_allowed'}'
json+=',"network":{"attempted":true,"allowed":'$network_allowed',"detail":"'$network_detail'"}'
json+=',"secretFileRead":{"attempted":'$file_read_attempted',"allowed":'$file_read_allowed
if [[ -n "$file_read_error" ]]; then json+=',"errorClass":"'$file_read_error'"'; fi
if [[ -n "$file_read_detail" ]]; then json+=',"detail":"'$file_read_detail'"'; fi
json+='}'
json+=',"dependencyImport":{"ok":'$dep_ok',"modules":['$modules_json']}'
json+=',"snapshot":{"supported":false,"ok":false,"detail":"no execution-state snapshot API in bash"}'
json+=',"approvalPause":{"supported":false,"ok":false,"detail":"no pause/resume primitive; host replay required"}'
json+='}'

echo "KANBEI_PROBE_JSON:$json"
`
const probeSource = probeSourceRaw.replaceAll("@@{", "${")

const prepared = (runtime: AvailableRuntime, options: ProbeOptions): PreparedProgram => ({
  files: [{ name: "probe.sh", text: probeSource }],
  argv: (directory) => [runtime.binaryPath, `${directory}/probe.sh`],
  env: () => commonEnv(options),
  latencySimulation: "external-sleep",
  timeoutMs: runTimeoutMs(options),
})

const startupPrepared = (runtime: AvailableRuntime): PreparedProgram => ({
  files: [{ name: "startup.sh", text: "echo ok" }],
  argv: (directory) => [runtime.binaryPath, `${directory}/startup.sh`],
  env: () => ({}),
  latencySimulation: "sleep",
  timeoutMs: 30_000,
})

export const bashArm: ArmDefinition = {
  arm: "bash",
  variant: "bash",
  resolve: () => resolveRuntime(RUNTIME_REQUESTS.bash),
  prepare: prepared,
  startup: startupPrepared,
}
