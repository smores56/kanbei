# S3 (`ISO-1`) — lightweight inter-agent isolation bake-off

| field                        | value                                                       |
| ---------------------------- | ----------------------------------------------------------- |
| spike                        | `S3`                                                        |
| validation identifier        | `ISO-1`                                                     |
| dataset                      | `security-v1` (sealed; evidence emitted)                    |
| run timestamp                | 2026-11-10T05:06:59.941Z                                    |
| host kernel                  | linux-6.18.39                                               |
| runtime                      | deno@2.9.2                                                  |
| cpus                         | 4                                                           |
| total memory                 | 7943.8 MiB                                                  |
| cgroup                       | v2 (cpuset cpu io memory hugetlb pids rdma misc dmem)       |
| unprivileged user namespaces | available                                                   |
| Landlock ABI                 | 7                                                           |
| cgroup2 nsdelegate           | present                                                     |
| delegated cgroup parent      | owned-by-uid                                                |
| kvm device                   | accessible                                                  |
| microVM runtimes on PATH     | none                                                        |
| target concurrency           | 16 agents (spike assumption; `LIM-1` owns the real ceiling) |

## Supervisor

| field                 | value                                                                                 |
| --------------------- | ------------------------------------------------------------------------------------- |
| kind                  | rootless-userns-chroot-landlock                                                       |
| uid                   | 1000                                                                                  |
| Landlock ABI          | 7                                                                                     |
| delegated cgroup root | `/user.slice/user-1000.slice/user@1000.service/app.slice/run-p360771-i360772.service` |
| delegated by          | `systemd-run --user --property=Delegate=yes --property=DelegateSubgroup=supervisor`   |
| dependency 1          | user systemd manager, used once to acquire a delegated cgroup root                    |
| dependency 2          | unprivileged user+PID+mount namespaces with chroot and Landlock                       |

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

The process arm runs one Deno child per agent as the init of its own rootless user+PID+mount namespace, with a minimal chroot root and a Landlock ruleset applied before the agent code runs. Each agent gets a cgroup directory under the one delegated root with `memory.max`, `memory.swap.max=0`, `pids.max`, and `cpu.max`, plus rlimits on core size, file size, and file descriptors. The WASM arm runs one WebAssembly instance inside one permission-less Deno worker per agent. The microVM arm is not measurable on this host.

Cold start is measured from supervisor spawn to the first ready signal, after one discarded warm-up run. Per-agent RSS is read from the agent cgroup (`memory.current`) for the process arm and from the process RSS delta for the WASM arm. Density records the time until every agent is ready and the failures at that count.

Density all-ready is the slowest agent's own ready time, not wall clock from the first spawn, so supervisor bookkeeping does not inflate it. Process-arm density RSS is the mean `memory.current` at all-ready; WASM density RSS is the process RSS delta divided by instance count.

Implementation lines count the supervision surface per arm: supervisor glue, the namespace/chroot sandbox plan, the agent entrypoint, and the protocol. Measurement probes and the probe-only WASM binary encoder are excluded, so the number reflects the mechanism rather than this harness.

Containment probes are destructive only inside the measured cgroup and sandbox: crash (`SIGKILL` of the agent host pid), hang (`cgroup.kill` on an infinite loop), OOM (request beyond the memory ceiling), fork bomb (200 fork attempts under `pids.max`), orphan reaping (a grandchild that dies with the PID-namespace init), and cleanup after kill (`cgroup.kill`, `cgroup.events populated=0`, `rmdir`). No probe writes outside the delegated cgroup, the per-agent sandbox root, and its scratch directory.

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
| cold start              | p50 290.4 ms / p95 415.6 ms / max 415.6 ms (n=5) |
| CPU workload            | 18.2 ms CPU for 2,000,000 iterations             |
| IPC round trip          | p50 0.3 ms / p95 0.5 ms / max 13.5 ms (n=200)    |
| RSS per agent at target | 16.3 MiB                                         |

| agents | outcome | all-ready ms | total RSS MiB | RSS/agent MiB | failures |
| ------ | ------- | ------------ | ------------- | ------------- | -------- |
| 1      | ok      | 396.5        | 49.8          | 49.8          | 0        |
| 2      | ok      | 441.7        | 63.5          | 31.7          | 0        |
| 4      | ok      | 551.2        | 90.2          | 22.6          | 0        |
| 8      | ok      | 673.8        | 147.2         | 18.4          | 0        |
| 16     | ok      | 1138.0       | 260.1         | 16.3          | 0        |
| 32     | ok      | 2648.0       | 477.3         | 14.9          | 0        |

- cold start includes delegated-cgroup creation, user/PID/mount namespace setup, bind mounts, chroot, Landlock, and Deno module loading (5 samples after one warm-up)
- density sweep at 1, 2, 4, 8, 16, 32 agents; per-agent RSS is the mean cgroup memory.current at all-ready

### wasm-instance

| metric                  | value                                            |
| ----------------------- | ------------------------------------------------ |
| cold start              | p50 430.9 ms / p95 443.9 ms / max 443.9 ms (n=5) |
| CPU workload            | 1.3 ms CPU for 2,000,000 iterations              |
| IPC round trip          | p50 0.1 ms / p95 0.3 ms / max 1.4 ms (n=200)     |
| RSS per agent at target | 21.2 MiB                                         |

| agents | outcome | all-ready ms | total RSS MiB | RSS/agent MiB | failures |
| ------ | ------- | ------------ | ------------- | ------------- | -------- |
| 1      | ok      | 361.4        | 0.0           | 0.0           | 0        |
| 2      | ok      | 286.7        | 41.9          | 20.9          | 0        |
| 4      | ok      | 459.8        | 87.0          | 21.7          | 0        |
| 8      | ok      | 879.1        | 173.7         | 21.7          | 0        |
| 16     | ok      | 2062.5       | 338.6         | 21.2          | 0        |
| 32     | ok      | 4489.2       | 656.1         | 20.5          | 0        |
| 64     | ok      | 8431.0       | 1317.1        | 20.6          | 0        |

- cold start includes V8 worker boot, module compilation, and instantiation (5 samples after one warm-up)
- density sweep at 1, 2, 4, 8, 16, 32, 64 instances

### microvm

Status: **blocked**. no microVM runtime on PATH (firecracker, cloud-hypervisor, microvm all absent; general VMM found: none); /dev/kvm is accessible; no sealed guest kernel, initrd, or rootfs exists in the repository, and the host has no toolchain to build one

- Blocked, not inconclusive: no measurement was attempted because the mechanism is not installable without a guest image

## Containment evidence

### process-cgroup

- **crash-isolation** — `pass` — SIGKILL to agent host pid 362821 (namespace pid 1): waiter exit code=137 signal=SIGKILL, cgroup populated=0=true; 2/2 peers still answered PING
- **hang-containment** — `pass` — cgroup.kill on a spinning agent: populated=0 in 15 ms (limit 5000 ms), waiter exit code=137; peer answered PING
- **oom-containment** — `pass` — memory.max=64 MiB, 128 MiB request: memory.events oom_kill=1, waiter exit code=137, cgroup populated=0=true after 99 ms; peer answered PING
- **fork-bomb-containment** — `pass` — 200 fork attempts with pids.max=16: FORKBOMB 28, peak pids.current=16, pids.events max=2, cgroup populated=0=true, 0 processes survived cgroup teardown
- **cleanup-after-kill** — `pass` — SIGKILL of agent host pid 363058 (namespace pid 1, waiter code=137); orphan 28 reaped, cgroup populated=0=true, cgroup removed=true, 0 marker processes left
- **orphan-reaping** — `pass` — grandchild visible before kill (1), 0 alive after the namespace init died and the cgroup emptied=true
- **ipc-round-trip** — `pass` — PING/PONG over the supervisor pipe completed 200 times, p95 0.52 ms
- **resource-ceilings** — `pass` — memory.max=134217728 pids.max=64 cpu.max=100000 100000 nofile=256 fsize=16777216
- **hosts-agent-workload** — `pass` — the reference agent ran as a Deno TypeScript process with environment and process capability inside the per-agent namespace, chroot, and cgroup

### wasm-instance

- **crash-isolation** — `pass` — unreachable trap surfaced as RuntimeError: unreachable; peer instance answered; supervisor process stayed up
- **hang-containment** — `pass` — infinite wasm loop blocked its worker until the supervisor terminated it in 0.1 ms; peer instance answered (instances cannot be preempted individually)
- **oom-containment** — `pass` — requested 64 pages, memory.grow returned -1, instance stayed at 1 pages and kept serving requests
- **fork-bomb-containment** — `not-applicable` — module imports 1 host function(s) and the worker runtime has no permissions (process spawn denied=true); a fork bomb has no reachable syscall
- **cleanup-after-kill** — `pass` — 8 workers terminated and the supervisor created a fresh instance afterwards; RSS peak 307.5 MiB, residual 179.2 MiB not returned to the OS
- **orphan-reaping** — `not-applicable` — an instance cannot create processes; there is no orphan to reap
- **ipc-round-trip** — `pass` — postMessage ping/pong completed 200 times, p95 0.3 ms
- **resource-ceilings** — `fail` — linear memory is capped at 16 pages and growth is refused, but the runtime exposes no CPU-time or wall-clock ceiling and no fuel metering
- **hosts-agent-workload** — `fail` — the reference agent needs filesystem, network, and process capability; this arm exposes one integer host import and no WASI runtime, so it cannot host the LANG-1 execution surface

### microvm

No probe was run for this arm.

## Operability costs

| arm            | host dependencies                                                                                                                   | root         | implementation lines | packaging                                                                                                                                                 | debuggability                                                                                                                                                                |
| -------------- | ----------------------------------------------------------------------------------------------------------------------------------- | ------------ | -------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| process-cgroup | user systemd manager, used once to acquire a delegated cgroup root; unprivileged user+PID+mount namespaces with chroot and Landlock | not required | 700                  | One process per agent inside a per-agent rootless namespace and chroot, under a delegated cgroup; the agent entrypoint is a TypeScript module run by deno | Host-side /proc and cgroup files; the agent pid is the namespace init and cgroup membership is the signal path. The agent sees no /sys and no systemd unit exists to inspect |
| wasm-instance  | deno (V8 core wasm)                                                                                                                 | not required | 250                  | One worker and one WebAssembly instance per agent; module bytes are built in-process                                                                      | No process or thread introspection; traps surface as RuntimeError, hangs need worker termination                                                                             |
| microvm        | firecracker or cloud-hypervisor; sealed guest kernel and initrd; guest rootfs build pipeline                                        | not required | 0                    | One microVM per agent with a pre-built, sealed guest image                                                                                                | Serial console and guest agent only; no host-side process introspection                                                                                                      |

**process-cgroup limitations**

- Agent, supervisor, and broker share one UID; isolation is namespace, chroot, Landlock, and cgroup, not UID-based
- No seccomp policy and io_uring remains enabled, which Landlock does not cover
- /sys is intentionally not mounted, which is what reverses SD-10: the agent cannot reach its own cgroup to raise its limits
- Landlock covers path-based filesystem access only; network and signal policy stay outside this spike
- Each agent pays user/PID/mount namespace setup, bind mounts, chroot, Landlock, and cgroup creation on the cold-start path
- RLIMIT_AS is unusable for a V8 process because the runtime reserves large virtual ranges
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
- Agent, supervisor, and broker share one UID. Isolation is namespace, chroot, Landlock, and cgroup; it is not UID-based, so a same-UID host process outside the sandbox is not contained.
- No seccomp policy is installed and io_uring remains enabled; Landlock does not mediate io_uring, so kernel attack surface remains.
- `/sys` is intentionally not mounted inside the agent root and Landlock never grants it. That is what reverses `SD-10`: an agent cannot reach its own cgroup to raise `memory.max`, `pids.max`, or `cpu.max`.
- Cold start now includes delegated-cgroup creation, user/PID/mount namespace setup, bind mounts, chroot, Landlock, and Deno module loading. A packaged binary would be faster; the comparison is same-host and same-runtime, not absolute.
- WASM RSS is a process-level delta because worker isolates share one address space. Per-instance memory is the declared linear-memory cap, not measured RSS.
- The WASM density row at one instance can read near-zero MiB because the process RSS delta is dominated by V8 heap reuse; the rows above one instance are the meaningful per-agent figures.
- The microVM arm is blocked: no microVM runtime and no sealed guest image on this host. `DG-4` cannot select a microVM on this evidence.
- Fork-bomb containment was probed with `pids.max=16` on a dedicated agent cgroup, not at the 64-task per-agent ceiling.
- The one systemd call is the delegated cgroup acquisition in `main.ts`; the per-agent path uses cgroupfs and namespaces only, but the supervisor still runs under the deployment user manager.

## Evidence status

`EvidenceEnvelope`: **emitted** — testId `S3`, fixture `security-v1@1.0.0`, outcome `pass`, artifacts `sha256:17853dc7d10831c12ef0582d63a15ceef71ba16e20b0b68056539fef46b8cde3`. The envelope is committed beside the result as `spikes/isolation/envelope.json`.

## Reproduction

```sh
systemd-run --user --property=Delegate=yes --property=DelegateSubgroup=supervisor \
  --pipe --wait --working-directory="$PWD" --setenv=KANBEI_S3_DELEGATED=1 \
  --setenv=DENO_DIR="$PWD/tmp/deno-cache" \
  "$(command -v deno)" run -A --unstable-worker-options src/spikes/isolation/main.ts
# or simply, from the repository root: main.ts re-execs into the command above
deno run -A --unstable-worker-options src/spikes/isolation/main.ts
deno test -A tests/spikes/isolation
```

The runner rewrites `spikes/isolation/result.json` and this file. Any change to a threshold in `src/spikes/isolation/protocol.ts` invalidates the recorded result and requires a re-run.
