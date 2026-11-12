import { resolveMonty } from "../resolve.ts"
import type { ArmDefinition, PreparedProgram, ProbeOptions } from "./contract.ts"
import { commonEnv, runTimeoutMs } from "./contract.ts"
import type { AvailableRuntime } from "./contract.ts"

const runnerSource = String.raw`
import json
import os
import sys
import time
from pydantic_monty import (
    FunctionSnapshot,
    Monty,
    MontyComplete,
    MontyError,
    load_snapshot,
)

LATENCY_MS = int(os.getenv("KANBEI_LATENCY_MS", "10") or "10")
CALLS = int(os.getenv("KANBEI_CALLS", "50") or "50")
SCRATCH_DIR = os.getenv("KANBEI_SCRATCH") or ""
SCRATCH_FILE = os.getenv("KANBEI_SCRATCH_FILE") or ""
CANARY_FILE = os.getenv("KANBEI_CANARY_FILE") or ""

overheads = []
stream_events = 0


def host_call(index):
    started = time.perf_counter()
    time.sleep(LATENCY_MS / 1000.0)
    overheads.append(max(0.0, (time.perf_counter() - started) * 1000.0 - LATENCY_MS))
    remainder = index % 3
    if remainder == 0:
        category = "alpha"
    elif remainder == 1:
        category = "beta"
    else:
        category = "gamma"
    return {"index": index, "category": category, "amount": ((index * 37) % 100) + 1}


program = """
results = []
i = 0
while i < __CALLS__:
    results.append(host_call(i))
    i = i + 1

kept = []
sum_alpha = 0
sum_beta = 0
sum_gamma = 0
idx = 0
while idx < len(results):
    item = results[idx]
    if item["amount"] > 50:
        kept.append(item)
        if item["category"] == "alpha":
            sum_alpha = sum_alpha + item["amount"]
        if item["category"] == "beta":
            sum_beta = sum_beta + item["amount"]
        if item["category"] == "gamma":
            sum_gamma = sum_gamma + item["amount"]
    idx = idx + 1

checksum = 0
idx = 0
while idx < len(kept):
    checksum = checksum + kept[idx]["amount"]
    idx = idx + 1

used = {}
top = []
rank = 0
while rank < 3:
    best_index = -1
    best_amount = -1
    k = 0
    while k < len(kept):
        item = kept[k]
        if item["index"] not in used:
            if item["amount"] > best_amount or (item["amount"] == best_amount and (best_index == -1 or item["index"] < best_index)):
                best_index = item["index"]
                best_amount = item["amount"]
        k = k + 1
    if best_index >= 0:
        used[best_index] = True
        top.append([best_index, best_amount])
    rank = rank + 1

expression_value = 0
i = 0
while i < 10:
    expression_value = expression_value + i * i
    i = i + 1

events = 0
i = 0
while i < 4:
    events = events + 1
    print("progress: " + str(events))
    i = i + 1

attempts = 0
value = 0
while attempts < 4:
    attempts = attempts + 1
    if attempts >= 3:
        value = 42
        break

retry_ok = False
if value == 42 and attempts == 3:
    retry_ok = True

result = {
    "probeVersion": 1,
    "expression": {"ok": expression_value == 285, "value": expression_value},
    "fanout": {
        "ok": True,
        "calls": __CALLS__,
        "checksum": checksum,
        "groups": [["alpha", sum_alpha], ["beta", sum_beta], ["gamma", sum_gamma]],
        "top": top,
        "callOverheadsMs": [],
        "totalMs": 0.0,
    },
    "stream": {"events": events},
    "retry": {"attempts": attempts, "ok": retry_ok},
}
result
""".replace("__CALLS__", str(CALLS))


def denied(error):
    return {
        "attempted": True,
        "allowed": False,
        "errorClass": type(error).__name__,
        "detail": str(error)[:200],
    }


def run_capability(code):
    try:
        machine = Monty(code, type_check=False)
        machine.run(limits={"max_duration_secs": 10})
        return {"attempted": True, "allowed": True}
    except Exception as error:
        return denied(error)


def snapshot_probe():
    code = 'x = host_call_pause(1)\ny = host_call_pause(2)\nresult = x + y\nresult'
    try:
        machine = Monty(code, type_check=False)
        first = machine.start()
        if not isinstance(first, FunctionSnapshot):
            return {"supported": True, "ok": False, "detail": "start() did not pause at the external call"}
        restored = load_snapshot(first.dump())
        second = restored.resume({"return_value": 10})
        if not isinstance(second, FunctionSnapshot):
            return {"supported": True, "ok": False, "detail": "restored snapshot did not pause again"}
        completed = second.resume({"return_value": 20})
        ok = isinstance(completed, MontyComplete) and completed.output == 30
        detail = "dump/load_snapshot resumed two host calls and returned 30"
        if not ok:
            detail = "resumed execution returned an unexpected value"
        return {
            "supported": True,
            "ok": ok,
            "detail": detail,
        }
    except Exception as error:
        return {"supported": True, "ok": False, "detail": str(error)[:200]}


def main():
    started = time.perf_counter()

    def capture(stream, text):
        global stream_events
        if stream == "stdout" and "progress:" in text:
            stream_events = stream_events + 1

    machine = Monty(program, type_check=False)
    completed = machine.run(
        external_functions={"host_call": host_call},
        print_callback=capture,
        limits={"max_duration_secs": max(10, CALLS * LATENCY_MS // 1000 + 10)},
    )
    output = completed
    if not isinstance(output, dict):
        raise RuntimeError("Monty did not return the fan-out result dict")
    total_ms = (time.perf_counter() - started) * 1000.0
    output["fanout"]["callOverheadsMs"] = overheads
    output["fanout"]["totalMs"] = total_ms
    output["stream"]["events"] = stream_events

    file_write = run_capability('handle = open("' + SCRATCH_FILE + '", "w")\nhandle.close()\n"ok"')
    if not file_write["allowed"]:
        file_write["detail"] = (
            "Monty has no ambient open(); filesystem effects require host-provided OSAccess"
        )
    else:
        file_write["detail"] = "host-provided OS access allowed the write"

    env_code = 'import os\nvalue = os.environ.get("KANBEI_BAKEOFF_CANARY")'
    process_code = 'import subprocess\nsubprocess.run(["true"])'
    network_code = 'import socket\nconnection = socket.create_connection(("127.0.0.1", 1), timeout=0.2)'
    secret_code = 'handle = open("' + CANARY_FILE + '", "r")\ncontents = handle.read()\ncontents'

    snapshot = snapshot_probe()

    modules = []
    for name in ("os", "subprocess", "socket", "json"):
        result = run_capability("import " + name)
        modules.append({"name": name, "ok": result["allowed"]})

    output["fileWrite"] = file_write
    output["envRead"] = run_capability(env_code)
    output["processSpawn"] = run_capability(process_code)
    output["network"] = run_capability(network_code)
    output["secretFileRead"] = run_capability(secret_code)
    output["dependencyImport"] = {"ok": False, "modules": modules}
    output["snapshot"] = snapshot
    output["approvalPause"] = {
        "supported": snapshot["supported"],
        "ok": snapshot["ok"],
        "detail": "pause at a host call; resume after the recorded approval decision",
    }

    print("KANBEI_PROBE_JSON:" + json.dumps(output), flush=True)


try:
    main()
except Exception as error:
    print("monty probe failed: " + repr(error), file=sys.stderr)
    sys.exit(1)
`

const prepared = (runtime: AvailableRuntime, options: ProbeOptions): PreparedProgram => ({
  files: [{ name: "probe_monty.py", text: runnerSource }],
  argv: (directory) => [runtime.binaryPath, `${directory}/probe_monty.py`],
  env: () => ({ ...commonEnv(options), ...runtime.env }),
  latencySimulation: "host-sleep",
  timeoutMs: runTimeoutMs(options),
})

const startupSource = String.raw`
import sys
from pydantic_monty import Monty

machine = Monty("1 + 1", type_check=False)
if machine.run() != 2:
    sys.exit(1)
print("ok")
`

const startupPrepared = (runtime: AvailableRuntime): PreparedProgram => ({
  files: [{ name: "startup_monty.py", text: startupSource }],
  argv: (directory) => [runtime.binaryPath, `${directory}/startup_monty.py`],
  env: () => runtime.env,
  latencySimulation: "sleep",
  timeoutMs: 30_000,
})

export const montyArm: ArmDefinition = {
  arm: "monty",
  variant: "monty",
  resolve: () => resolveMonty(),
  prepare: prepared,
  startup: startupPrepared,
}
