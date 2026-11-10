# A5 — Gate consolidation and baseline freeze

Status: `input-ready-human-signature-required`. No decision gate is closed by this record.

## Inputs

| Task  | Protocol             | Result                                   | Gate   | Recommendation                                      |
| ----- | -------------------- | ---------------------------------------- | ------ | --------------------------------------------------- |
| `A1`  | `S1` / `NODE-DENO-1` | `spikes/runtime/result.json`             | `DG-2` | `deno@2.9.2`, conditional                           |
| `A2`  | `S2` / `DEP-1`       | `spikes/deployment/result.json`          | `DG-3` | variant D, risk-based; no pre-registered DEP-1 rule |
| `A2b` | `S2` extension       | `spikes/deployment/rootless-result.json` | `DG-3` | variant E rootless sandbox candidate                |
| `A3`  | `S3` / `ISO-1`       | `spikes/isolation/result.json`           | `DG-4` | `process-cgroup`                                    |
| `A4`  | `S1` / `LANG-1`      | `spikes/language/result.json`            | `DG-1` | none; model arm blocked                             |

## Gate records

`spikes/gates/records.json` contains the four open records. Every record is `owner=human`, has no `selected` value, and has no `signedBy` or `signedAt`.

### `DG-1` — execution language

Status: **open**. Recommendation: **none**.

All 600 model cells (18,000 planned trials) are blocked. The local matrix resolved Monty, Deno, Node, Lua 5.4, Luau, Bash, and CPython, and measured startup, RSS, host-call overhead, snapshot support, capability posture, and progressive disclosure. Those measurements are context only; they do not select a language.

Blocking inputs: frozen model manifest, `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, frozen OS-sandbox profile, sealed `security-v1`, `FI-01` runtime/interpreter versions, `FI-06` hardware fingerprint/timeouts/ceilings, a signed `DG-2` to fix the TypeScript evaluator variant, and a §7.0-registered dataset for the A4 task suite (`lang-v1` is not registered).

### `DG-2` — control-plane runtime

Status: **open**. Recommendation: **Deno 2.9.2**, conditional.

The pre-registered composite margin was 4.7%, inside the 10% margin, so the peak-RSS tie-break selected Deno. This is a near-tie: Node wins append throughput (+62%), event-loop p50, warm start, and worker startup. Deno wins cold start, peak RSS, and shutdown; its single-binary packaging was measured while Node packaging was blocked. The record must carry the conditions that canonical writes are serialized in the application, never by a SQLite busy handler, and that the protocol is re-run after the ledger sequencer and worker model exist.

### `DG-3` — deployment plane and broker identity

Status: **open**. Recommendation: **variant E candidate** — rootless user/PID/mount namespace + chroot + Landlock, broker outside the sandbox with memory-only credentials. Variant D is the fallback.

Pure user-space measured **no secret isolation**: a same-UID sibling read broker credential bytes through an open descriptor and 0600 files, and could signal and reconfigure sibling units. A2b then tested the composed rootless sandbox: a private user+PID+mount namespace, a minimal chroot root, and a Landlock allowlist. All checks passed: allowed reads/writes, denied-path `EACCES`, broker fd and signal isolation, `systemctl --user` denial, `/proc/self/root` host-escape denial, and broker socket reachability. The broker stays outside the sandbox and keeps credentials memory-only. This is option E. Variant D remains the fallback where user namespaces or a compatible Landlock ABI are unavailable.

Required conditions for option E: unprivileged user namespaces and a compatible Landlock ABI; a minimal root containing only runtime, `/etc`, `/proc`, allowed project roots, scratch, and the broker socket; memory-only broker credentials; io_uring/seccomp hardening; and S3 re-run under the chosen supervisor identity. Variant D conditions remain the existing dedicated system-user, loopback, `LimitCORE=0`, `docker`-group removal, and control-group kill requirements.

### `DG-4` — inter-agent isolation

Status: **open**. Recommendation: **process-cgroup**.

`process-cgroup` is the only eligible arm. It passed crash, hang, OOM, fork-bomb, cleanup, orphan-reaping, IPC, resource-ceiling, and workload checks, with p95 cold start 429 ms, 15.8 MiB per agent at 16 agents, and IPC p95 0.35 ms. `wasm-instance` is excluded because it has no CPU/fuel ceiling and cannot host the LANG-1 workload. `microvm` is blocked because no runtime or sealed guest image exists on this host. **No security property is claimed** for any arm. DG-4 is also not closed: DG-3 must fix which UID owns the delegated cgroup subtree, and S3 must be re-run under that supervisor identity. SD-10 means a same-UID agent can raise its own `memory.max`, so the containment pass holds only for non-hostile agents.

## Operator envelope

`src/spikes/gates/operator-envelope.ts` mirrors the `HardCeilings`, `LoopBudget`, `BudgetOverride`, and `OperatorEnvelope` shapes from `07 §4.7.3`, including `Duration` for `wall_clock`. No values are set here. `H4` measures the workload and proposes values; the human decides at `DG-6`.

## Baseline freeze

Not performed. A baseline cannot be frozen while `DG-1` through `DG-4` remain unsigned and `security-v1` is unsealed. The harness configuration that is fixed today is:

- Deno 2.9.2 and Effect 4.1.2.
- Sealed fixtures: `journey-v1`, `append-v1`, `cost-v1`, `ui-v1`.
- Evidence contracts and virtual clock/synthetic network from A0.
- Pre-registrations in `src/spikes/runtime/plan.ts`, `src/spikes/deployment/matrix.ts`, `src/spikes/isolation/protocol.ts`, and `src/spikes/language/pre-registration.ts`.

## Evidence binding gaps

- `security-v1` is not registered or sealed. A1’s permission probes, A2’s host probes, A3’s isolation probes, and A4’s security cells therefore lack a dataset-bound `EvidenceEnvelope`.
- A4 issued no envelope because the model arm is blocked.
- A2 and A3 emitted measurement records, not envelopes, for the same `security-v1` reason.
- A2b is a measurement record; it is not an `EvidenceEnvelope` and does not close `DG-3`.
- A1 emitted a measurement envelope bound to `append-v1`; it is now marked `blocked` because `cost-v1` and `security-v1` bindings are absent.

These are recorded blockers, not waivers.

## Shared Nix store

A2c bootstrapped a single private store, mounted it at `/nix/store`, and built `nixpkgs#hello` without touching host `/nix/store`. Direct execution of the built path passed. `nix shell` package-environment lookup still fails, so `nix profile` is not yet proven. This is a build/direct path, not a complete package-install path.

## Cross-gate dependency

`DG-3` and `DG-4` are operationally coupled. A3 measured `process-cgroup` under the deployment user systemd user manager. Variant E keeps the supervisor outside the rootless sandbox but still under the deployment user; variant D moves it to a dedicated system unit. Until DG-3 chooses E or D and fixes which identity owns the delegated cgroup subtree, DG-4 cannot be treated as measured in the target posture. S3 must be re-run under that identity before the recommendations compose.

## Required human decisions

1. Accept, reject, or amend the `DG-2` Deno recommendation and its conditions.
2. Choose `DG-3` option E (rootless sandbox, recommended) or option D (dedicated system users), and accept or amend its residuals.
3. Accept, reject, or amend the `DG-4` process+cgroup recommendation.
4. For `DG-1`, either provide the model manifest and credentials for a live `LANG-1` run, or explicitly accept that no language is selected and block dependent `F4`.
5. Decide whether to seal a real `security-v1` fixture and re-run S1–S3 binding, or accept the current evidence as input with the gap recorded.
