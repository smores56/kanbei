# S3 (`ISO-1`) — lightweight inter-agent isolation bake-off

| field                    | value                                                                     |
| ------------------------ | ------------------------------------------------------------------------- |
| spike                    | `S3`                                                                      |
| validation identifier    | `ISO-1`                                                                   |
| dataset                  | `security-v1` (fixture not sealed; see evidence status)                   |
| run timestamp            | 2026-11-10T02:28:24.407Z                                                  |
| host kernel              | linux-6.18.39                                                             |
| runtime                  | deno@2.9.2                                                                |
| cpus                     | 4                                                                         |
| total memory             | 7943.8 MiB                                                                |
| systemd user delegation  | available (systemd-run --user accepted MemoryMax=32M TasksMax=8 (exit 0)) |
| cgroup                   | v2                                                                        |
| kvm device               | accessible                                                                |
| microVM runtimes on PATH | none                                                                      |
| target concurrency       | 16 agents (spike assumption; `LIM-1` owns the real ceiling)               |

## No security property is claimed

No security property is claimed for any arm. S3 (ISO-1) measures crash and resource containment only. No arm is evidence of a credential, approval, filesystem, network, or trust-root boundary; those remain owned by the OS sandbox, broker, and capability contracts.

Explicitly outside this spike:

- credential, approval, and trust-root boundaries (`ZT-01`, `ZT-11`, `ZT-13`), which remain owned by the broker, consent surface, and OS sandbox
- filesystem, network, and process capability enforcement (`DEP-1`)
- containment of malicious code that is already executing inside the agent; only crash and resource faults are measured
- container-per-agent, which the requirement excludes

## Decision input for `DG-4`

Outcome: **recommend `process-cgroup`**.

Pre-registered rule: Eligible = status measured AND every containment check pass or not-applicable AND hosts-agent-workload pass AND density at target concurrency ok AND p95 cold start <= 500 ms AND per-agent RSS <= 64 MiB AND p95 IPC <= 25 ms AND no root requirement AND host dependencies <= 2. Among eligible arms, select the lowest p95 cold start, then the lowest per-agent RSS, then the lowest p95 IPC. No eligible arm means no selection; DG-4 stays open.

| arm            | eligible | reason when not eligible                                                                                                                                                                                                                                        |
| -------------- | -------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| process-cgroup | yes      | —                                                                                                                                                                                                                                                               |
| wasm-instance  | no       | containment failed: resource-ceilings, hosts-agent-workload                                                                                                                                                                                                     |
| microvm        | no       | status blocked: no microVM runtime on PATH (firecracker, cloud-hypervisor, microvm all absent; general VMM found: none); /dev/kvm is accessible; no sealed guest kernel, initrd, or rootfs exists in the repository, and the host has no toolchain to build one |

- process-cgroup is the only arm that satisfies every pre-registered gate
- wasm-instance: containment failed: resource-ceilings, hosts-agent-workload
- microvm: status blocked: no microVM runtime on PATH (firecracker, cloud-hypervisor, microvm all absent; general VMM found: none); /dev/kvm is accessible; no sealed guest kernel, initrd, or rootfs exists in the repository, and the host has no toolchain to build one

## Method

Every arm is measured on the same host with the same reference agent workload: a fixed integer accumulation loop (2000000 iterations), a same-host request/response round trip, and a density sweep at 1, 2, 4, 8, 16, 32 agents.

The process arm runs one Deno child per agent inside a transient systemd user unit with cgroup v2 `memory.max`, `pids.max`, and `cpu.max`, plus rlimits on core size, file size, and file descriptors. The WASM arm runs one WebAssembly instance inside one permission-less Deno worker per agent. The microVM arm is not measurable on this host.

Cold start is measured from supervisor spawn to the first ready signal, after one discarded warm-up run. Per-agent RSS is read from the agent cgroup (`memory.current`) for the process arm and from the process RSS delta for the WASM arm. Density records the time until every agent is ready and the failures at that count.

Density all-ready is the slowest agent's own ready time, not wall clock from the first spawn, so supervisor bookkeeping does not inflate it. Process-arm density RSS is the mean `memory.current` at all-ready; WASM density RSS is the process RSS delta divided by instance count.

Implementation lines count the supervision surface per arm: supervisor glue, the agent entrypoint, and the protocol. Measurement probes and the probe-only WASM binary encoder are excluded, so the number reflects the mechanism rather than this harness.

Containment probes are destructive only inside the measured unit: crash (`SIGKILL`), hang (infinite loop), OOM (request beyond the memory ceiling), fork bomb (200 fork attempts under `pids.max`), orphan reaping (a grandchild that outlives the agent), and cleanup after kill. No probe writes outside `/tmp` and the caller's systemd user manager.

## Containment matrix

| check                 | process-cgroup | wasm-instance | microvm |
| --------------------- | -------------- | ------------- | ------- |
| crash-isolation       | pass           | pass          | not-run |
| hang-containment      | pass           | pass          | not-run |
| oom-containment       | pass           | pass          | not-run |
| fork-bomb-containment | pass           | n/a           | not-run |
| cleanup-after-kill    | pass           | pass          | not-run |
| orphan-reaping        | pass           | n/a           | not-run |
| ipc-round-trip        | pass           | pass          | not-run |
| resource-ceilings     | pass           | fail          | not-run |
| hosts-agent-workload  | pass           | fail          | not-run |

## Measurements

### process-cgroup

| metric                  | value                                            |
| ----------------------- | ------------------------------------------------ |
| cold start              | p50 404.8 ms / p95 428.6 ms / max 428.6 ms (n=5) |
| CPU workload            | 16.7 ms CPU for 2,000,000 iterations             |
| IPC round trip          | p50 0.2 ms / p95 0.3 ms / max 3.4 ms (n=200)     |
| RSS per agent at target | 15.8 MiB                                         |

| agents | outcome | all-ready ms | total RSS MiB | RSS/agent MiB | failures |
| ------ | ------- | ------------ | ------------- | ------------- | -------- |
| 1      | ok      | 319.4        | 52.6          | 52.6          | 0        |
| 2      | ok      | 328.9        | 65.5          | 32.7          | 0        |
| 4      | ok      | 436.2        | 92.8          | 23.2          | 0        |
| 8      | ok      | 751.1        | 145.2         | 18.1          | 0        |
| 16     | ok      | 1168.3       | 252.1         | 15.8          | 0        |
| 32     | ok      | 2082.0       | 460.9         | 14.4          | 0        |

- cold start includes systemd transient unit creation and Deno module loading (5 samples after one warm-up)
- density sweep at 1, 2, 4, 8, 16, 32 agents; per-agent RSS is the mean cgroup memory.current at all-ready

### wasm-instance

| metric                  | value                                            |
| ----------------------- | ------------------------------------------------ |
| cold start              | p50 182.8 ms / p95 196.9 ms / max 196.9 ms (n=5) |
| CPU workload            | 1.9 ms CPU for 2,000,000 iterations              |
| IPC round trip          | p50 0.2 ms / p95 0.4 ms / max 4.0 ms (n=200)     |
| RSS per agent at target | 21.1 MiB                                         |

| agents | outcome | all-ready ms | total RSS MiB | RSS/agent MiB | failures |
| ------ | ------- | ------------ | ------------- | ------------- | -------- |
| 1      | ok      | 274.5        | 0.0           | 0.0           | 0        |
| 2      | ok      | 395.8        | 39.0          | 19.5          | 0        |
| 4      | ok      | 568.4        | 83.0          | 20.7          | 0        |
| 8      | ok      | 975.5        | 168.7         | 21.1          | 0        |
| 16     | ok      | 1841.7       | 337.3         | 21.1          | 0        |
| 32     | ok      | 2274.4       | 667.9         | 20.9          | 0        |
| 64     | ok      | 6440.6       | 1346.0        | 21.0          | 0        |

- cold start includes V8 worker boot, module compilation, and instantiation (5 samples after one warm-up)
- density sweep at 1, 2, 4, 8, 16, 32, 64 instances

### microvm

Status: **blocked**. no microVM runtime on PATH (firecracker, cloud-hypervisor, microvm all absent; general VMM found: none); /dev/kvm is accessible; no sealed guest kernel, initrd, or rootfs exists in the repository, and the host has no toolchain to build one

- Blocked, not inconclusive: no measurement was attempted because the mechanism is not installable without a guest image

## Containment evidence

### process-cgroup

- **crash-isolation** — `pass` — SIGKILL on one agent: signal=null code=255 Result=signal; 2/2 peers still answered PING
- **hang-containment** — `pass` — infinite loop in one agent killed through its cgroup in 14 ms (Result=signal); peer answered PING
- **oom-containment** — `pass` — MemoryMax=64M, 512 MiB request: Result=oom-kill signal=null after 59 ms; peer answered PING
- **fork-bomb-containment** — `pass` — 200 fork attempts with TasksMax=16: peak pids.current=16, pids.max=16, 0 processes survived unit teardown (20 ms)
- **cleanup-after-kill** — `pass` — SIGKILL of agent (code=255); orphan 314574 reaped, cgroup removed=true, unit=inactive, 0 marker processes left
- **orphan-reaping** — `pass` — grandchild visible before kill (1), 0 alive after cgroup teardown
- **ipc-round-trip** — `pass` — PING/PONG over the supervisor pipe completed 200 times, p95 0.35 ms
- **resource-ceilings** — `pass` — memory.max=134217728 pids.max=64 cpu.max=100000 100000 nofile=256 fsize=16777216
- **hosts-agent-workload** — `pass` — the reference agent ran as a Deno TypeScript process with environment and process capability under the cgroup

### wasm-instance

- **crash-isolation** — `pass` — unreachable trap surfaced as RuntimeError: unreachable; peer instance answered; supervisor process stayed up
- **hang-containment** — `pass` — infinite wasm loop blocked its worker until the supervisor terminated it in 0.0 ms; peer instance answered (instances cannot be preempted individually)
- **oom-containment** — `pass` — requested 64 pages, memory.grow returned -1, instance stayed at 1 pages and kept serving requests
- **fork-bomb-containment** — `not-applicable` — module imports 1 host function(s) and the worker runtime has no permissions (process spawn denied=true); a fork bomb has no reachable syscall
- **cleanup-after-kill** — `pass` — 8 workers terminated and the supervisor created a fresh instance afterwards; RSS peak 122.9 MiB, residual 0.0 MiB not returned to the OS
- **orphan-reaping** — `not-applicable` — an instance cannot create processes; there is no orphan to reap
- **ipc-round-trip** — `pass` — postMessage ping/pong completed 200 times, p95 0.42 ms
- **resource-ceilings** — `fail` — linear memory is capped at 16 pages and growth is refused, but the runtime exposes no CPU-time or wall-clock ceiling and no fuel metering
- **hosts-agent-workload** — `fail` — the reference agent needs filesystem, network, and process capability; this arm exposes one integer host import and no WASI runtime, so it cannot host the LANG-1 execution surface

### microvm

No probe was run for this arm.

## Operability costs

| arm            | host dependencies                                                                            | root         | implementation lines | packaging                                                                            | debuggability                                                                                    |
| -------------- | -------------------------------------------------------------------------------------------- | ------------ | -------------------- | ------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------ |
| process-cgroup | deno; systemd user manager with cgroup v2 delegation                                         | not required | 386                  | One process per agent unit; agent entrypoint is a TypeScript module run by deno      | Standard POSIX process tools, /proc, cgroup files, and systemd unit status                       |
| wasm-instance  | deno (V8 core wasm)                                                                          | not required | 249                  | One worker and one WebAssembly instance per agent; module bytes are built in-process | No process or thread introspection; traps surface as RuntimeError, hangs need worker termination |
| microvm        | firecracker or cloud-hypervisor; sealed guest kernel and initrd; guest rootfs build pipeline | not required | 0                    | One microVM per agent with a pre-built, sealed guest image                           | Serial console and guest agent only; no host-side process introspection                          |

**process-cgroup limitations**

- Cgroup memory, pids, and CPU ceilings require systemd user delegation; without it only rlimits apply
- RLIMIT_AS is unusable for a V8 process because the runtime reserves large virtual ranges
- Each agent pays systemd unit creation on the cold-start path
- Deno denies even `/proc/self` under a narrow `--allow-read` grant, so the agent observes nothing about itself; the supervisor reads all cgroup and rlimit state
- Agent capabilities are inner Deno permissions only (`--allow-env`, `--allow-run` here); the OS sandbox remains the real boundary

**wasm-instance limitations**

- No WASI runtime on the host, so filesystem, network, and process capability are unreachable from an instance
- Instances share the worker thread: no CPU preemption or fuel metering, so a hang is only bounded by terminating the worker
- Worker RSS is not attributable per instance; density RSS is a process-level delta
- Worker permission scoping (`deno: { permissions: "none" }`) needs the unstable `--unstable-worker-options` flag in Deno 2.9

**microvm limitations**

- Every agent code or runtime change requires rebuilding and resealing the guest image
- Guest images are additional release artifacts to store, verify, and roll back
- Not measured in this spike; no containment or density claim is made

## Blockers

- **microvm** (blocked): no microVM runtime on PATH (firecracker, cloud-hypervisor, microvm all absent; general VMM found: none); /dev/kvm is accessible; no sealed guest kernel, initrd, or rootfs exists in the repository, and the host has no toolchain to build one

## Residuals

- No security property is claimed for any arm. This record covers crash and resource containment only.
- Target concurrency 16 is a spike-local assumption; `LIM-1` sets the real operator ceiling, so density at any other count must be re-read from the sweep.
- Process-arm ceilings depend on systemd user cgroup delegation. Without it the arm degrades to rlimits and would fail the containment checks; the deployment plane must guarantee delegation (`DEP-1`).
- WASM RSS is a process-level delta because worker isolates share one address space. Per-instance memory is the declared linear-memory cap, not measured RSS.
- The WASM density row at one instance can read near-zero MiB because the process RSS delta is dominated by V8 heap reuse; the rows above one instance are the meaningful per-agent figures.
- The microVM arm is blocked: no microVM runtime and no sealed guest image on this host. `DG-4` cannot select a microVM on this evidence.
- Cold start includes systemd transient unit creation and on-the-fly Deno TypeScript loading. A packaged agent binary or a precompiled JS entrypoint would be faster; the comparison is same-host and same-runtime, not absolute.
- Fork-bomb containment was probed with `pids.max=16` on a dedicated transient unit, not at the 64-task per-agent ceiling.
- `security-v1` is not sealed in this repository, so no `EvidenceEnvelope` is emitted. S3 must be re-run against the sealed dataset before `A5` consumes it.

## Evidence status

`EvidenceEnvelope`: **unavailable**. `security-v1` has no sealed fixture in this repository (`fixtures/registry.json` lists journey-v1, append-v1, cost-v1, ui-v1 only), so this is a measurement record rather than a validation envelope.

## Reproduction

```sh
deno run -A --unstable-worker-options src/spikes/isolation/main.ts
deno test -A tests/spikes/isolation
```

The runner rewrites `spikes/isolation/result.json` and this file. Any change to a threshold in `src/spikes/isolation/protocol.ts` invalidates the recorded result and requires a re-run.
