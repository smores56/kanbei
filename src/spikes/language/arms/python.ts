import { resolveRuntime, RUNTIME_REQUESTS } from "../resolve.ts"
import type { ArmDefinition, PreparedProgram, ProbeOptions } from "./contract.ts"
import { commonEnv, runTimeoutMs } from "./contract.ts"
import type { AvailableRuntime } from "./contract.ts"

const probeSource = String.raw`
import json
import os
import socket
import subprocess
import time

LATENCY_MS = int(os.getenv("KANBEI_LATENCY_MS", "10") or "10")
CALLS = int(os.getenv("KANBEI_CALLS", "50") or "50")

def category_of(index):
    return ["alpha", "beta", "gamma"][index % 3]

def amount_of(index):
    return ((index * 37) % 100) + 1

def denied(error):
    return {
        "attempted": True,
        "allowed": False,
        "errorClass": type(error).__name__,
        "detail": str(error)[:200],
    }

def file_write_probe(path):
    if not path:
        return {"attempted": False, "allowed": False, "errorClass": "MissingInput"}
    try:
        with open(path, "w", encoding="utf-8") as handle:
            handle.write("derived")
        with open(path, "r", encoding="utf-8") as handle:
            back = handle.read()
        return {"attempted": True, "allowed": back == "derived", "detail": "scratch write/read"}
    except Exception as error:
        return denied(error)

def file_read_probe(path):
    if not path:
        return {"attempted": False, "allowed": False, "errorClass": "MissingInput"}
    try:
        with open(path, "r", encoding="utf-8") as handle:
            contents = handle.read()
        return {"attempted": True, "allowed": len(contents) > 0, "detail": "canary file readable"}
    except Exception as error:
        return denied(error)

def process_probe():
    try:
        subprocess.run(["true"], check=True, timeout=5)
        return {"attempted": True, "allowed": True}
    except Exception as error:
        return denied(error)

def network_probe():
    try:
        connection = socket.create_connection(("127.0.0.1", 1), timeout=0.25)
        connection.close()
        return {"attempted": True, "allowed": True, "detail": "loopback connect succeeded"}
    except Exception as error:
        permission_denied = isinstance(error, PermissionError)
        return {
            "attempted": True,
            "allowed": not permission_denied,
            "errorClass": type(error).__name__,
            "detail": "permission denied" if permission_denied else "connect failed without a policy denial",
        }

def dependency_probe():
    modules = []
    for name in ("socket", "json", "hashlib", "pandas", "requests"):
        try:
            __import__(name)
            modules.append({"name": name, "ok": True})
        except Exception:
            modules.append({"name": name, "ok": False})
    return {"ok": all(entry["ok"] for entry in modules), "modules": modules}

def main():
    expression_value = 0
    for index in range(10):
        expression_value += index * index
    expression = {"ok": expression_value == 285, "value": expression_value}

    items = []
    overheads = []
    start = time.perf_counter()
    for index in range(CALLS):
        call_start = time.perf_counter()
        time.sleep(LATENCY_MS / 1000.0)
        overheads.append(max(0.0, (time.perf_counter() - call_start) * 1000.0 - LATENCY_MS))
        items.append({"index": index, "category": category_of(index), "amount": amount_of(index)})
    total_ms = (time.perf_counter() - start) * 1000.0

    kept = [item for item in items if item["amount"] > 50]
    sums = {"alpha": 0, "beta": 0, "gamma": 0}
    for item in kept:
        sums[item["category"]] = sums.get(item["category"], 0) + item["amount"]
    groups = sorted([[key, value] for key, value in sums.items()])
    top = [[item["index"], item["amount"]] for item in sorted(kept, key=lambda item: (-item["amount"], item["index"]))[:3]]
    checksum = sum(item["amount"] for item in kept)

    stream_events = 0
    for _ in range(4):
        stream_events += 1
        print("progress: " + str(stream_events), flush=True)

    retry_attempts = 0
    retry_value = 0
    while retry_attempts < 4:
        retry_attempts += 1
        if retry_attempts >= 3:
            retry_value = 42
            break

    canary = os.getenv("KANBEI_BAKEOFF_CANARY")
    env_read = {
        "attempted": True,
        "allowed": canary is not None and len(canary) > 0,
    }
    if canary:
        env_read["detail"] = "canary env var readable"
    else:
        env_read["errorClass"] = "DeniedOrUnset"

    result = {
        "probeVersion": 1,
        "expression": expression,
        "fanout": {
            "ok": True,
            "calls": CALLS,
            "checksum": checksum,
            "groups": groups,
            "top": top,
            "callOverheadsMs": overheads,
            "totalMs": total_ms,
        },
        "stream": {"events": stream_events},
        "retry": {"attempts": retry_attempts, "ok": retry_value == 42 and retry_attempts == 3},
        "fileWrite": file_write_probe(os.getenv("KANBEI_SCRATCH_FILE")),
        "envRead": env_read,
        "processSpawn": process_probe(),
        "network": network_probe(),
        "secretFileRead": file_read_probe(os.getenv("KANBEI_CANARY_FILE")),
        "dependencyImport": dependency_probe(),
        "snapshot": {
            "supported": False,
            "ok": False,
            "detail": "no execution-state snapshot API; host replay is required",
        },
        "approvalPause": {
            "supported": False,
            "ok": False,
            "detail": "no pause/resume primitive; approval requires host replay",
        },
    }
    print("KANBEI_PROBE_JSON:" + json.dumps(result), flush=True)

main()
`

const prepared = (runtime: AvailableRuntime, options: ProbeOptions): PreparedProgram => ({
  files: [{ name: "probe.py", text: probeSource }],
  argv: (directory) => [runtime.binaryPath, `${directory}/probe.py`],
  env: () => commonEnv(options),
  latencySimulation: "sleep",
  timeoutMs: runTimeoutMs(options),
})

const startupPrepared = (runtime: AvailableRuntime): PreparedProgram => ({
  files: [{ name: "startup.py", text: 'print("ok")' }],
  argv: (directory) => [runtime.binaryPath, `${directory}/startup.py`],
  env: () => ({}),
  latencySimulation: "sleep",
  timeoutMs: 30_000,
})

export const cpythonArm: ArmDefinition = {
  arm: "cpython",
  variant: "cpython",
  resolve: () => resolveRuntime(RUNTIME_REQUESTS.cpython),
  prepare: prepared,
  startup: startupPrepared,
}
