# A5 — Gate consolidation and baseline freeze

Status: `partially-signed` — `DG-2` and `DG-3` closed; `DG-1` blocked; `DG-4` re-run complete and awaiting human sign-off.

## Inputs

| Task  | Protocol             | Result                                   | Gate   | Recommendation                       |
| ----- | -------------------- | ---------------------------------------- | ------ | ------------------------------------ |
| `A1`  | `S1` / `NODE-DENO-1` | `spikes/runtime/result.json`             | `DG-2` | `deno@2.9.2`, conditional            |
| `A2`  | `S2` / `DEP-1`       | `spikes/deployment/result.json`          | `DG-3` | variant D, fallback; E selected      |
| `A2b` | `S2` extension       | `spikes/deployment/rootless-result.json` | `DG-3` | variant E rootless sandbox candidate |
| `A3`  | `S3` / `ISO-1`       | `spikes/isolation/result.json`           | `DG-4` | `process-cgroup`                     |
| `A4`  | `S1` / `LANG-1`      | `spikes/language/result.json`            | `DG-1` | none; model arm blocked              |

## Gate records

`spikes/gates/records.json` contains two signed decisions and two blocked records. `DG-2` and `DG-3` have `selected` values and a human signature; `DG-1` and `DG-4` remain blocked.

### `DG-1` — execution language

Status: **blocked**. Recommendation: **none**.

The pre-registration was amended before any trial: `AMD-1` (recorded 2026-11-10, `beforeTrials: true`) replaced the two-provider requirement with a single provider, NeuralWatt, serving primary `deepseek-v4.1-flash` and smaller `qwen-3.8-27b` under `NEURALWATT_API_KEY`. The manifest is frozen at `spikes/language/model-manifest.json`, and `FI-03` names both model IDs. Zero of 9,000 planned trials ran, so this is a pre-data amendment: it claims no measurement, no model selection, and no gate closure.

All 300 model cells are blocked. The local matrix resolved Monty, Deno, Node, Lua 5.4, Luau, Bash, and CPython, and measured startup, RSS, host-call overhead, snapshot support, capability posture, and progressive disclosure. Those measurements are context only; they do not select a language.

Remaining blockers: no live provider adapter or model trials exist; `NEURALWATT_API_KEY` is not configured, so every model cell stays blocked; the sandbox profile is not frozen (`MI-04`, `sandboxPosture=unverified`); `FI-01` runtime versions and `FI-06` hardware and ceilings are unfrozen; `lang-v1` is not a §7.0-registered dataset. A single liveness probe of `deepseek-v4.1-flash` returned `OK` at temperature 0; the key was not stored, and the probe is not a trial.

### `DG-2` — control-plane runtime

Status: **closed**. Selected: **Deno 2.9.2**, conditional.

The pre-registered composite margin was 4.7%, inside the 10% margin, so the peak-RSS tie-break selected Deno. This is a near-tie: Node wins append throughput (+62%), event-loop p50, warm start, and worker startup. Deno wins cold start, peak RSS, and shutdown; its single-binary packaging was measured while Node packaging was blocked. The record must carry the conditions that canonical writes are serialized in the application, never by a SQLite busy handler, and that the protocol is re-run after the ledger sequencer and worker model exist.

### `DG-3` — deployment plane and broker identity

Status: **closed**. Selected: **variant E** — rootless user/PID/mount namespace + chroot + Landlock, broker outside the sandbox with memory-only credentials. Variant D is the fallback.

Pure user-space measured **no secret isolation**: a same-UID sibling read broker credential bytes through an open descriptor and 0600 files, and could signal and reconfigure sibling units. A2b then tested the composed rootless sandbox: a private user+PID+mount namespace, a minimal chroot root, and a Landlock allowlist. All checks passed: allowed reads/writes, denied-path `EACCES`, broker fd and signal isolation, `systemctl --user` denial, `/proc/self/root` host-escape denial, and broker socket reachability. The broker stays outside the sandbox and keeps credentials memory-only. This is option E. Variant D remains the fallback where user namespaces or a compatible Landlock ABI are unavailable.

Required conditions for option E: unprivileged user namespaces and a compatible Landlock ABI; a minimal root containing only runtime, `/etc`, `/proc`, allowed project roots, scratch, and the broker socket; memory-only broker credentials; io_uring/seccomp hardening; and S3 re-run under the chosen supervisor identity. Variant D conditions remain the existing dedicated system-user, loopback, `LimitCORE=0`, `docker`-group removal, and control-group kill requirements.

### `DG-4` — inter-agent isolation

Status: **blocked**. Recommendation: **process-cgroup**. The S3 re-run is complete; human sign-off is the only remaining gate action.

`process-cgroup` is the only eligible arm. S3 was re-run under `DG-3` option E: the supervisor runs outside the agent sandbox as the deployment user and owns one delegated cgroup root; each agent runs as the init of its own rootless user+PID+mount namespace with a chroot root, a Landlock ruleset applied before agent code, and a cgroup directory under that root. All nine containment checks passed, and every pre-registered budget held: p95 cold start 416 ms, 16.3 MiB per agent at 16 agents, IPC p95 0.52 ms, 700 implementation lines, and 2 host dependencies. `wasm-instance` is excluded because it has no CPU/fuel ceiling and cannot host the LANG-1 workload. `microvm` is blocked because no runtime or sealed guest image exists on this host. **No security property is claimed** for any arm. The run emitted an `EvidenceEnvelope` at `spikes/isolation/envelope.json` (testId `S3`, fixture `security-v1@1.0.0`, outcome `pass`).

Residuals carried to sign-off: agent, supervisor, and broker share one UID; no seccomp policy and io_uring remains enabled; `/sys` is intentionally unmounted and never granted by Landlock, which is what reverses `SD-10` for the agent itself; containment is measured for non-hostile agents only. A one-time systemd user cgroup delegation remains the single systemd call.

## Operator envelope

`src/spikes/gates/operator-envelope.ts` mirrors the `HardCeilings`, `LoopBudget`, `BudgetOverride`, and `OperatorEnvelope` shapes from `07 §4.7.3`, including `Duration` for `wall_clock`. No values are set here. `H4` measures the workload and proposes values; the human decides at `DG-6`.

## Baseline freeze

Not performed. A baseline cannot be frozen while `DG-1` and `DG-4` remain unsigned. The harness configuration that is fixed today is:

- Deno 2.9.2 and Effect 4.1.2.
- Sealed fixtures: `journey-v1`, `append-v1`, `cost-v1`, `ui-v1`, `security-v1`.
- Evidence contracts and virtual clock/synthetic network from A0.
- Pre-registrations in `src/spikes/runtime/plan.ts`, `src/spikes/deployment/matrix.ts`, `src/spikes/isolation/protocol.ts`, and `src/spikes/language/pre-registration.ts`.

## Evidence binding gaps

- `security-v1` is registered and sealed in this tree. The S3 re-run emits an `EvidenceEnvelope` bound to `security-v1@1.0.0` (testId `S3`, outcome `pass`); A4's security cells remain blocked with the model arm.
- A1's permission and fail-closed probes are bound via a second envelope, testId `NODE-DENO-1-SECURITY`, fixture `security-v1@1.0.0`, outcome `pass`. The A1 main `NODE-DENO-1` envelope stays `blocked` on the `cost-v1` binding.
- A2 now emits a `DEP-1` envelope bound to `security-v1@1.0.0` with outcome `fail` by design: pure user-space has no secret isolation. It is a finding, not a defect, and it does not change the `DG-3` record.
- A4 issued no envelope because the model arm is blocked.
- A3 emits an envelope as well (testId `S3`, outcome `pass`). A2b is a measurement record; it is not an `EvidenceEnvelope` and does not close `DG-3`.

These are recorded blockers, not waivers.

## Shared Nix store

A2c bootstrapped a single private store, mounted it at `/nix/store`, and built `nixpkgs#hello` without touching host `/nix/store`. Direct execution, `nix shell`, `nix run`, `nix profile add/list`, and execution from the installed profile all pass. The earlier `nix shell` ENOENT was a probe bug: Landlock denied `openat("/proc/self/root")`, which Nix uses to re-anchor its root after a mount-namespace round-trip; granting read on the sandbox root fixes it. `nix develop` remains unsupported because the sandbox mounts no `/dev` or `/dev/pts`.

## Cross-gate dependency

`DG-3` and `DG-4` are operationally coupled. S3 was re-run under `DG-3` option E: the supervisor is the deployment user outside the rootless agent sandbox and owns the delegated cgroup root, so the two recommendations now compose. `DG-3` variant D would still move the supervisor to a dedicated system unit; the S3 record would then need a re-run under that identity.

## Required human decisions

1. `DG-2` is signed as Deno 2.9.2; no further action.
2. `DG-3` is signed as option E; the S3 re-run under that supervisor identity is complete.
3. `DG-4` awaits human sign-off; `process-cgroup` is the measured recommendation and the envelope is emitted, with no further measurement action outstanding.
4. `DG-1` remains blocked: the two-provider requirement is amended (`AMD-1`), so the remaining work is a live NeuralWatt adapter, `NEURALWATT_API_KEY`, a frozen sandbox profile, and complete paired trials. No recommendation may be claimed before those trials run.
5. Re-run S1 and S2 so their security probes bind to the sealed `security-v1` dataset, or accept the evidence gap. S3 is already bound. A4 issues no envelope while every model cell is blocked. This is implementation work, not another gate decision.
