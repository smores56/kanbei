# S3 (`ISO-1`) — lightweight inter-agent isolation bake-off

| field                        | value                                                       |
| ---------------------------- | ----------------------------------------------------------- |
| spike                        | `S3`                                                        |
| validation identifier        | `ISO-1`                                                     |
| dataset                      | `security-v1` (sealed; evidence emitted)                    |
| run timestamp                | 2026-11-11T19:32:29.834Z                                    |
| host kernel                  | linux-6.18.39                                               |
| runtime                      | deno@2.9.2                                                  |
| cpus                         | 32                                                          |
| total memory                 | 31997.1 MiB                                                 |
| cgroup                       | v2 (cpuset cpu io memory hugetlb pids rdma misc dmem)       |
| unprivileged user namespaces | available                                                   |
| Landlock ABI                 | 7                                                           |
| cgroup2 nsdelegate           | present                                                     |
| delegated cgroup parent      | owned-by-uid                                                |
| kvm device                   | accessible                                                  |
| microVM runtimes on PATH     | /run/current-system/sw/bin/microvm                          |
| target concurrency           | 16 agents (spike assumption; `LIM-1` owns the real ceiling) |

## Supervisor

| field                 | value                                                                                   |
| --------------------- | --------------------------------------------------------------------------------------- |
| kind                  | rootless-userns-chroot-landlock                                                         |
| uid                   | 1000                                                                                    |
| Landlock ABI          | 7                                                                                       |
| delegated cgroup root | `/user.slice/user-1000.slice/user@1000.service/app.slice/run-p1826952-i1826953.service` |
| delegated by          | `systemd-run --user --property=Delegate=yes --property=DelegateSubgroup=supervisor`     |
| dependency 1          | user systemd manager, used once to acquire a delegated cgroup root                      |
| dependency 2          | unprivileged user+PID+mount namespaces with chroot and Landlock                         |

## No security property is claimed

No security property is claimed for any arm. S3 (ISO-1) measures crash and resource containment only. No arm is evidence of a credential, approval, filesystem, network, or trust-root boundary; those remain owned by the OS sandbox, broker, and capability contracts.

Explicitly outside this spike:

- credential, approval, and trust-root boundaries (`ZT-01`, `ZT-11`, `ZT-13`), which remain owned by the broker, consent surface, and OS sandbox
- filesystem, network, and process capability enforcement (`DEP-1`)
- containment of malicious code that is already executing inside the agent; only crash and resource faults are measured
- container-per-agent, which the requirement excludes

## Decision input for `DG-4`

Outcome: **no selection; `DG-4` stays open**.

Pre-registered rule: Eligible = status measured AND every containment check pass or not-applicable AND hosts-agent-workload pass AND density at target concurrency ok AND p95 cold start <= 500 ms AND per-agent RSS <= 64 MiB AND p95 IPC <= 25 ms AND no root requirement AND host dependencies <= 2. Among eligible arms, select the lowest p95 cold start, then the lowest per-agent RSS, then the lowest p95 IPC. No eligible arm means no selection; DG-4 stays open.

| arm            | eligible | reason when not eligible                                                                                                       |
| -------------- | -------- | ------------------------------------------------------------------------------------------------------------------------------ |
| process-cgroup | no       | 838 implementation lines exceed 700                                                                                            |
| wasm-instance  | no       | containment failed: resource-ceilings, hosts-agent-workload                                                                    |
| microvm        | no       | status inconclusive: microvm runtimes present (/run/current-system/sw/bin/microvm) but no guest image is sealed for this spike |

- no arm satisfies the pre-registered gates; DG-4 stays open
- process-cgroup: 838 implementation lines exceed 700
- wasm-instance: containment failed: resource-ceilings, hosts-agent-workload
- microvm: status inconclusive: microvm runtimes present (/run/current-system/sw/bin/microvm) but no guest image is sealed for this spike

## Method

Every arm is measured on the same host with the same reference agent workload: a fixed integer accumulation loop (2000000 iterations), a same-host request/response round trip, and a density sweep at 1, 2, 4, 8, 16, 32 agents.

The process arm runs one Deno child per agent as the init of its own rootless user+PID+mount namespace, with a minimal chroot root and a Landlock ruleset applied before the agent code runs. Each agent gets a cgroup directory under the one delegated root with `memory.max`, `memory.swap.max=0`, `pids.max`, and `cpu.max`, plus rlimits on core size, file size, and file descriptors. The WASM arm runs one WebAssembly instance inside one permission-less Deno worker per agent. The microVM arm is not measurable on this host.

Cold start is measured from supervisor spawn to the first ready signal, after one discarded warm-up run, and includes the SIGSTOP/SIGCONT handshake that moves the sandboxed process into its cgroup before Deno allocates. Per-agent RSS is read from the agent cgroup (`memory.current`) for the process arm and from the process RSS delta for the WASM arm. Density records the time until every agent is ready and the failures at that count.

Density all-ready is the slowest agent's own ready time, not wall clock from the first spawn, so supervisor bookkeeping does not inflate it. Process-arm density RSS is the mean `memory.current` at all-ready; the sandbox stops itself before exec so the supervisor migrates the stopped pid and only then resumes it, which charges every agent page to `agent-N`. WASM density RSS is the process RSS delta divided by instance count.

Implementation lines count the supervision surface per arm: supervisor glue, the namespace/chroot sandbox plan, the agent entrypoint, and the protocol. Measurement probes and the probe-only WASM binary encoder are excluded, so the number reflects the mechanism rather than this harness.

Containment probes are destructive only inside the measured cgroup and sandbox: crash (`SIGKILL` of the agent host pid), hang (`cgroup.kill` on an infinite loop), OOM (request beyond the memory ceiling), fork bomb (200 fork attempts under `pids.max`), orphan reaping (a grandchild that dies with the PID-namespace init), and cleanup after kill (`cgroup.kill`, `cgroup.events populated=0`, `cgroup.procs` empty, `rmdir`). Orphan and fork-bomb checks prove the child is alive before the kill through the agent cgroup's `pids.current` delta, a marker that survives exec, and the host process count; after the kill they require `cgroup.events populated 0`, an empty `cgroup.procs`, no marker survivors, and the specific orphan pid gone. No probe writes outside the delegated cgroup, the per-agent sandbox root, and its scratch directory.

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

Scope: The nine containment ids, cold start, CPU, IPC, and density describe one Deno agent per rootless namespace, chroot, Landlock ruleset, and cgroup; no network, credential, or trust-root boundary is claimed

| metric                  | value                                            |
| ----------------------- | ------------------------------------------------ |
| cold start              | p50 112.7 ms / p95 120.3 ms / max 120.3 ms (n=5) |
| CPU workload            | 16.2 ms CPU for 2,000,000 iterations             |
| IPC round trip          | p50 0.2 ms / p95 0.3 ms / max 6.0 ms (n=200)     |
| RSS per agent at target | 13.7 MiB                                         |

| agents | outcome | all-ready ms | total RSS MiB | RSS/agent MiB | failures |
| ------ | ------- | ------------ | ------------- | ------------- | -------- |
| 1      | ok      | 103.4        | 14.9          | 14.9          | 0        |
| 2      | ok      | 118.6        | 28.3          | 14.2          | 0        |
| 4      | ok      | 135.8        | 61.5          | 15.4          | 0        |
| 8      | ok      | 139.0        | 114.2         | 14.3          | 0        |
| 16     | ok      | 169.4        | 218.6         | 13.7          | 0        |
| 32     | ok      | 290.4        | 414.6         | 13.0          | 0        |

- cold start includes delegated-cgroup creation, user/PID/mount namespace setup, bind mounts, chroot, Landlock, and Deno module loading (5 samples after one warm-up)
- density sweep at 1, 2, 4, 8, 16, 32 agents; per-agent RSS is the mean cgroup memory.current at all-ready

### wasm-instance

Scope: Crash, hang, OOM, cleanup, and IPC describe one WebAssembly instance inside one permission-less Deno worker; fork-bomb and orphan checks are not applicable because an instance cannot create processes, and filesystem, network, and process capability are absent

| metric                  | value                                         |
| ----------------------- | --------------------------------------------- |
| cold start              | p50 93.8 ms / p95 96.7 ms / max 96.7 ms (n=5) |
| CPU workload            | 0.6 ms CPU for 2,000,000 iterations           |
| IPC round trip          | p50 0.1 ms / p95 0.1 ms / max 1.3 ms (n=200)  |
| RSS per agent at target | 21.2 MiB                                      |

| agents | outcome | all-ready ms | total RSS MiB | RSS/agent MiB | failures |
| ------ | ------- | ------------ | ------------- | ------------- | -------- |
| 1      | ok      | 89.1         | 0.0           | 0.0           | 0        |
| 2      | ok      | 120.3        | 38.8          | 19.4          | 0        |
| 4      | ok      | 139.9        | 83.9          | 21.0          | 0        |
| 8      | ok      | 201.3        | 168.5         | 21.1          | 0        |
| 16     | ok      | 516.5        | 339.0         | 21.2          | 0        |
| 32     | ok      | 856.8        | 681.1         | 21.3          | 0        |
| 64     | ok      | 1736.6       | 1362.1        | 21.3          | 0        |

- cold start includes V8 worker boot, module compilation, and instantiation (5 samples after one warm-up)
- density sweep at 1, 2, 4, 8, 16, 32, 64 instances

### microvm

Scope: No containment or density measurement ran; a runtime exists but no sealed guest image is available

Status: **inconclusive**. microvm runtimes present (/run/current-system/sw/bin/microvm) but no guest image is sealed for this spike

- Runtime present but the spike has no sealed guest image; no claim is made

## Containment evidence

### process-cgroup

- **crash-isolation** — `pass` — SIGKILL to agent host pid 1829689 (namespace pid 1): waiter exit code=137 signal=SIGKILL, cgroup populated=0=true; 2/2 peers still answered PING
- **hang-containment** — `pass` — cgroup.kill on a spinning agent: populated=0 in 11 ms (limit 5000 ms), waiter exit code=137; peer answered PING
- **oom-containment** — `pass` — memory.max=64 MiB, 128 MiB request: memory.events oom_kill=1, waiter exit code=137, cgroup populated=0=true after 27 ms; peer answered PING
- **fork-bomb-containment** — `pass` — 200 fork attempts with pids.max=16: FORKBOMB 28, pids.events max=6 (refusals observed), peak pids.current=16 (observation only; the kernel caps it at pids.max), marker processes before kill=7, after kill populated=0, cgroup.procs=0, marker survivors=0
- **cleanup-after-kill** — `pass` — SIGKILL of agent host pid 1830283 (namespace pid 1, waiter code=137): orphan host pid 1830330 (namespace pid 28) confirmed gone; pids.current before kill=10, marker pids before kill=1, after kill populated=0, cgroup.procs=0, marker survivors=0, cgroup removed=true, host processes 567->567
- **orphan-reaping** — `pass` — orphan host pid 1830480 (namespace pid 28) visible before the kill via cgroup membership (pids.current=10) and marker scan (1 pids); after the namespace init died: populated=0, cgroup.procs=0, marker survivors=0, orphan confirmed gone, host processes 567->567
- **ipc-round-trip** — `pass` — PING/PONG over the supervisor pipe completed 200 times, p95 0.25 ms
- **resource-ceilings** — `pass` — memory.max=134217728 pids.max=64 cpu.max=100000 100000 nofile=256 fsize=16777216
- **hosts-agent-workload** — `pass` — agent reported `WORKLOAD ok`: read /etc/os-release, wrote and read /scratch/workload.txt, spawned true, and reached 127.0.0.1:9 with ECONNREFUSED

### wasm-instance

- **crash-isolation** — `pass` — unreachable trap surfaced as RuntimeError: unreachable; peer instance answered; supervisor process stayed up
- **hang-containment** — `pass` — infinite wasm loop blocked its worker until the supervisor terminated it in 0.0 ms; peer instance answered (instances cannot be preempted individually)
- **oom-containment** — `pass` — requested 64 pages, memory.grow returned -1, instance stayed at 1 pages and kept serving requests
- **fork-bomb-containment** — `not-applicable` — module imports 1 host function(s) and the worker runtime has no permissions (process spawn denied=true); a fork bomb has no reachable syscall
- **cleanup-after-kill** — `pass` — 8 workers terminated and the supervisor created a fresh instance afterwards; RSS peak 283.0 MiB, residual 165.1 MiB not returned to the OS
- **orphan-reaping** — `not-applicable` — an instance cannot create processes; there is no orphan to reap
- **ipc-round-trip** — `pass` — postMessage ping/pong completed 200 times, p95 0.13 ms
- **resource-ceilings** — `fail` — linear memory is capped at 16 pages and growth is refused, but the runtime exposes no CPU-time or wall-clock ceiling and no fuel metering
- **hosts-agent-workload** — `fail` — the reference agent needs filesystem, network, and process capability; this arm exposes one integer host import and no WASI runtime, so it cannot host the LANG-1 execution surface

### microvm

No probe was run for this arm.

## Operability costs

| arm            | host dependencies                                                                                                                   | root         | implementation lines | packaging                                                                                                                                                 | debuggability                                                                                                                                                                |
| -------------- | ----------------------------------------------------------------------------------------------------------------------------------- | ------------ | -------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| process-cgroup | user systemd manager, used once to acquire a delegated cgroup root; unprivileged user+PID+mount namespaces with chroot and Landlock | not required | 838                  | One process per agent inside a per-agent rootless namespace and chroot, under a delegated cgroup; the agent entrypoint is a TypeScript module run by deno | Host-side /proc and cgroup files; the agent pid is the namespace init and cgroup membership is the signal path. The agent sees no /sys and no systemd unit exists to inspect |
| wasm-instance  | deno (V8 core wasm)                                                                                                                 | not required | 250                  | One worker and one WebAssembly instance per agent; module bytes are built in-process                                                                      | No process or thread introspection; traps surface as RuntimeError, hangs need worker termination                                                                             |
| microvm        | /run/current-system/sw/bin/microvm; sealed guest kernel and initrd                                                                  | not required | 0                    | One microVM per agent with a pre-built, sealed guest image                                                                                                | Serial console and guest agent only                                                                                                                                          |

**process-cgroup limitations**

- Agent, supervisor, and broker share one UID; isolation is namespace, chroot, Landlock, and cgroup, not UID-based
- No seccomp policy and io_uring remains enabled, which Landlock does not cover
- /sys is intentionally not mounted, which is what reverses SD-10: the agent cannot reach its own cgroup to raise its limits
- Landlock covers path-based filesystem access only; network and signal policy stay outside this spike
- Each agent pays user/PID/mount namespace setup, bind mounts, chroot, Landlock, and cgroup creation on the cold-start path
- RLIMIT_AS is unusable for a V8 process because the runtime reserves large virtual ranges
- Agent capabilities are inner Deno permissions only (`--allow-env`, `--allow-run`, `--allow-read=/etc`, `--allow-write=/scratch`, `--allow-net=127.0.0.1:9` here); the OS sandbox remains the real boundary

**wasm-instance limitations**

- No WASI runtime on the host, so filesystem, network, and process capability are unreachable from an instance
- Instances share the worker thread: no CPU preemption or fuel metering, so a hang is only bounded by terminating the worker
- Worker RSS is not attributable per instance; density RSS is a process-level delta
- Worker permission scoping (`deno: { permissions: "none" }`) needs the unstable `--unstable-worker-options` flag in Deno 2.9

**microvm limitations**

- No guest image available, so nothing was measured

## Blockers

- **microvm** (inconclusive): microvm runtimes present (/run/current-system/sw/bin/microvm) but no guest image is sealed for this spike

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
- Orphan and fork-bomb liveness is proved before the kill by cgroup membership, marker argv survival, and host process counts. The marker is the argv[0] of a resident bash because nix coreutils dispatches applets on argv[0] and would abort under `exec -a`.

## Evidence status

`EvidenceEnvelope`: **emitted** — testId `S3`, fixture `security-v1@1.0.0`, outcome `inconclusive`, artifacts `sha256:48dcc60ad222c536a6dbdfb4b38fa6b6cc74e06034e51fa26187c30718373c28`. The envelope is committed beside the result as `spikes/isolation/envelope.json`.

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
