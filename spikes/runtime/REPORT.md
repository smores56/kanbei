# S1 — `NODE-DENO-1` TypeScript control-plane runtime bake-off

Task `A1`. Protocol `S1` (`07 §7.4`), decision `NODE-DENO-1` (`07 §8.1`), evidence contract `07 §7.0`.
Machine-readable arm: [`result.json`](./result.json). Pre-registration is frozen in
[`src/spikes/runtime/plan.ts`](../../src/spikes/runtime/plan.ts) and republished inside `result.json` as
`preRegistration`, so this document can be checked against the run.

Status: **complete for the assigned measurement set; the decision gate is blocked on two protocol bindings that
this task cannot create** (see [Protocol gaps](#protocol-gaps-and-blockers)). Recommendation in
[Recommendation for `DG-2`](#recommendation-for-dg-2).

---

## 1. Pre-registration (frozen before measurement)

| Item                          | Value                                                                                                                                                                                                                                                                                  |
| ----------------------------- | -------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| Protocol                      | `S1`                                                                                                                                                                                                                                                                                   |
| Decision                      | `NODE-DENO-1`                                                                                                                                                                                                                                                                          |
| Dataset binding               | `append-v1@1.0.0` (the only sealed dataset this workload can bind; see protocol gaps)                                                                                                                                                                                                  |
| Arms                          | `node@24.21.0` (nix `nixpkgs#nodejs_24`), `deno@2.9.2`                                                                                                                                                                                                                                 |
| Cold-start runs per arm       | 11 (spawn → ready, spawn → exit)                                                                                                                                                                                                                                                       |
| Warm-start iterations per arm | 40 (in-process bring-up: open DB, prepare, insert, read, close)                                                                                                                                                                                                                        |
| Ledger load per arm           | 1000 durable appends, one append per event-loop turn, 1 point read per append, WAL + `synchronous=FULL`                                                                                                                                                                                |
| Event-loop sampler            | `setTimeout` reschedule every 1 ms, lag = actual − due                                                                                                                                                                                                                                 |
| Worker probe                  | 5 ping round-trips, then an intentional uncaught worker exception                                                                                                                                                                                                                      |
| Decision rule                 | Hard fails disqualify. Otherwise the lower weighted composite wins if the relative margin is ≥ 10 %; otherwise the tie-breakers apply in order: lower peak RSS → lower cold start → inner capability model. A blocked decision measurement yields `inconclusive`; no value is imputed. |
| Composite weights             | cold-ready 0.25, peak RSS 0.15, event-loop p95 0.20, append p95 0.20, warm start 0.10, worker startup 0.10                                                                                                                                                                             |
| Frozen revision               | `a1-node-deno-1-prereg-v1`                                                                                                                                                                                                                                                             |

Hard-fail list (condition → rationale), all pre-registered:

| ID                 | Condition                                                                                                                                                           | Rationale                                                         |
| ------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ----------------------------------------------------------------- |
| `HF-CORRECTNESS`   | any SQLite correctness check fails                                                                                                                                  | a driver that returns wrong rows cannot hold the canonical ledger |
| `HF-SINGLE-WRITER` | a second connection commits while the first holds `BEGIN IMMEDIATE`, or the refused write is visible later, or the ledger cannot recover after the lock is released | `I-02` requires one sequencer                                     |
| `HF-INTEGRITY`     | `PRAGMA integrity_check` ≠ `ok` after the load                                                                                                                      | `ZT-09`                                                           |
| `HF-DATA-LOSS`     | stored rows < issued appends                                                                                                                                        | `I-01`                                                            |
| `HF-TAIL-LATENCY`  | event-loop lag p95 > 250 ms under load                                                                                                                              | the loop also arbitrates approvals and sandbox supervision        |
| `HF-APPEND-TAIL`   | append p95 > 100 ms                                                                                                                                                 | canonical durable write path                                      |
| `HF-ISOLATION`     | a crashing worker kills the parent, or the parent does not observe the crash                                                                                        | agent work runs off-loop                                          |
| `HF-SIGNAL`        | `SIGTERM` handler not invoked, shutdown > 3000 ms, or non-zero exit                                                                                                 | systemd stop contract                                             |
| `HF-PERMISSION`    | an operation the runtime was told to deny succeeds                                                                                                                  | fail-closed (`ZT-04`)                                             |

## 2. Methodology

**Same code.** Both arms execute the identical program, `src/spikes/runtime/arm.ts`. It imports no
runtime-specific module. The runtime-specific surface is a single interface,
`src/spikes/runtime/adapters/types.ts` (`ArmHost`), with two implementations
(`adapters/deno.ts`, `adapters/node.ts`). SQLite, the worker probe, the ledger workload, the event-loop
sampler, the correctness suite and the statistics are shared source.

**Same SQLite binding.** Both arms use `node:sqlite` (`adapters/sqlite-driver.ts`); Deno 2.9.2 implements the
same module. Deno reports SQLite 3.53.2, Node 3.53.3 — the same major/minor line but not the same build
(recorded as a confound).

**Same workload and filesystem.** Each arm writes its ledger under
`tmp/spikes/runtime/`, on the same filesystem, with WAL and `synchronous=FULL`, one
`BEGIN IMMEDIATE … COMMIT` per record. Payload bodies are the sealed `append-v1` cases, cycled. Hash chaining is
precomputed outside the measured region; the chain is re-verified after the load.

**Same measurement script.** The harness host (`src/spikes/runtime/driver.ts`, run by
`src/spikes/runtime/cli.ts` under Deno) spawns each arm as a fresh child process and times it from outside:
spawn → `@@READY@@` marker, spawn → exit, and signal → exit. Per-arm internals (warm start, lag samples,
append/read latency) are measured inside the arm with `Clock.monotonicTimeNanos`, then reduced by the same
statistics module (`src/spikes/runtime/metrics.ts`).

**Arms do not type-check at run time.** Deno runs with `--no-check`; Node 24 strips types without checking.
Deno's type checking is a build gate (`deno check`), not a start-up cost, so this keeps the comparison honest.

**Boundary adapters (documented, isolated).**

- `node/register-hooks.mjs` — Node has no import map; the hook resolves the bare `effect` specifier to a pinned
  install under `tmp/spikes/runtime/node-deps` (installed automatically from the version in `deno.json`). The
  resolution is computed before hooks are registered, because resolving from inside the hook re-enters it.
- `adapters/sqlite-driver.ts`, `adapters/worker.ts`, `adapters/worker-entry.ts` — `node:*` imports.
- `adapters/launch.ts` — `Deno.Command`, `/proc`, `du`; the harness host is Deno.
- `adapters/deno.ts` / `adapters/node.ts` — runtime identity, RSS, permission probes, signal registration.

**OS sandbox.** Both arms ran with the operator's normal OS permissions. The comparison is runtime-only and
claims nothing about containment; Deno permissions are inner-only and the OS sandbox remains mandatory
(`07 §8.1`).

**Cold / warm definition.** Cold = fresh process, timed to the `@@READY@@` marker printed after the module graph
loads and the first database open completes. Warm = re-running app-layer bring-up inside one process.

## 3. Raw tables (published run, `result.json`)

All units are milliseconds unless noted; `p50`/`p95` come from the run's own sample vectors. Sample counts:
cold 11 per arm, warm 40 per arm, appends 1000 per arm, event-loop 1088 (node) / 1072 (deno), worker 5 per arm.

| Metric                            |                node |                deno |
| --------------------------------- | ------------------: | ------------------: |
| cold start to ready p50 / p95     |       566.8 / 657.5 |       288.8 / 409.4 |
| cold start to exit p50 / p95      |       580.5 / 678.0 |       299.1 / 425.5 |
| warm start p50 / p95              |         20.0 / 25.1 |         24.8 / 29.0 |
| RSS after boot p50 (MiB)          |               144.7 |               103.7 |
| RSS after load (MiB)              |               146.1 |                93.7 |
| peak RSS (`VmHWM`) (MiB)          |               146.4 |               108.3 |
| event-loop lag p50 / p95 / max    | 1.43 / 6.41 / 15.34 | 5.88 / 8.10 / 34.38 |
| durable append p50 / p95 / mean   |  2.77 / 7.16 / 4.37 |  6.74 / 8.64 / 7.08 |
| point read p50 / p95              |       0.016 / 0.052 |       0.024 / 0.073 |
| durable append throughput (ops/s) |                 229 |                 141 |
| worker startup                    |               108.0 |               234.9 |
| worker round-trip p50 / p95       |       0.068 / 0.165 |         0.28 / 1.39 |
| `SIGTERM` shutdown                |                18.9 |                14.7 |
| runtime binary (MiB)              |                74.6 |               136.0 |
| compiled single binary (MiB)      |             blocked |               199.4 |
| SQLite version                    |              3.53.3 |              3.53.2 |

Correctness and isolation, both arms: **11/11 checks pass**, single writer refuses the concurrent writer
(`concurrentWriterCommitted=false`), the refused write is not visible (`failClosed=true`), the ledger recovers
after the holder commits (`recoveryOk=true`), WAL is active, read-your-writes holds, the hash chain verifies,
`integrity_check=ok`, 1000/1000 appends stored, worker crash observed by the parent and the parent survived.

### 3.1 SQLite driver parity and single-writer behaviour

| Probe                                           | node                                                                | deno                                                          |
| ----------------------------------------------- | ------------------------------------------------------------------- | ------------------------------------------------------------- |
| `journal_mode` / `synchronous` / `foreign_keys` | wal / 2 / 1                                                         | wal / 2 / 1                                                   |
| FK violation rejected                           | yes                                                                 | yes                                                           |
| rollback leaves no row                          | yes                                                                 | yes                                                           |
| `INTEGER PRIMARY KEY` monotonic                 | 1,2,3,4,5                                                           | 1,2,3,4,5                                                     |
| WAL visible to a second connection              | yes                                                                 | yes                                                           |
| `wal_checkpoint(TRUNCATE)`                      | busy=0                                                              | busy=0                                                        |
| concurrent `BEGIN IMMEDIATE` while held         | `database is locked`                                                | `database is locked`                                          |
| busy-handler timeout 250 ms, holder keeps lock  | blocks 252 ms, own 40 ms timer never fires                          | blocks 253 ms, own 40 ms timer never fires                    |
| error metadata on lock contention               | `code=ERR_SQLITE_ERROR`, `errcode=5`, `errstr="database is locked"` | `code=ERR_SQLITE_ERROR`, message only (no `errcode`/`errstr`) |

The last row is a real, if small, driver-parity difference: Deno's `node:sqlite` does not populate
`errstr`/`errcode`, so code that classifies lock contention must match on the message or `code` alone.

### 3.2 Permission behaviour

| Probe                                | node (`--permission`)                                      | deno (no permission flags) |
| ------------------------------------ | ---------------------------------------------------------- | -------------------------- |
| write outside scope                  | denied `ERR_ACCESS_DENIED`                                 | denied `NotCapable`        |
| read outside scope (`/etc/hostname`) | denied `ERR_ACCESS_DENIED`                                 | denied `NotCapable`        |
| spawn a process                      | denied `ERR_ACCESS_DENIED`                                 | denied `NotCapable`        |
| connect to a socket                  | **allowed** (`ECONNREFUSED`, no network capability exists) | denied `NotCapable`        |

Deno's model is inner and default-deny; Node's is an opt-in process flag and has no network capability, so
network containment for the Node arm stays an OS-sandbox obligation. No probe failed open.

### 3.3 Run-to-run stability (5 runs, same machine, same worktree)

| Run | composite node | composite deno | margin | decision | rule               | cold-ready node/deno | peak RSS node/deno | append ops/s node/deno |
| --- | -------------: | -------------: | -----: | -------- | ------------------ | -------------------: | -----------------: | ---------------------: |
| 1   |         1.2120 |         1.1594 |  4.5 % | deno     | peak-RSS tie-break |            464 / 299 |      147.1 / 103.8 |              220 / 134 |
| 2   |         1.1847 |         1.1381 |  4.1 % | deno     | peak-RSS tie-break |            519 / 365 |       147.3 / 96.3 |              225 / 145 |
| 3   |         1.1656 |         1.1485 |  1.5 % | deno     | peak-RSS tie-break |            589 / 414 |      146.5 / 104.8 |              213 / 136 |
| 4   |         1.2880 |         1.2507 |  3.0 % | deno     | peak-RSS tie-break |            602 / 299 |      146.9 / 119.0 |              230 / 142 |
| 5   |         1.2933 |         1.2358 |  4.7 % | deno     | peak-RSS tie-break |            567 / 289 |      146.4 / 108.3 |              229 / 141 |

The composite is inside the pre-registered 10 % margin in every run, so the tie-breaker decides every time.
Per-metric signs are mixed and stable: Deno wins cold start and peak RSS, Node wins warm start, event-loop tail,
append tail and throughput. That the weighted sum stays within 1.5–4.7 % is exactly why the margin exists.
Peak RSS and cold start separate the arms consistently across runs; append throughput consistently favours Node.

## 4. Decision matrix

Normalized cost = own value ÷ better value (lower is better). Composite = Σ weight × cost.

| Metric              | Weight |      node |      deno |  node cost |  deno cost | Winner                     |
| ------------------- | -----: | --------: | --------: | ---------: | ---------: | -------------------------- |
| cold start to ready |   0.25 |  566.8 ms |  288.8 ms |     1.9622 |     1.0000 | deno                       |
| peak RSS            |   0.15 | 146.4 MiB | 108.3 MiB |     1.3516 |     1.0000 | deno                       |
| event-loop lag p95  |   0.20 |   6.41 ms |   8.10 ms |     1.0000 |     1.2627 | node                       |
| durable append p95  |   0.20 |   7.16 ms |   8.64 ms |     1.0000 |     1.2077 | node                       |
| warm start          |   0.10 |   20.0 ms |   24.8 ms |     1.0000 |     1.2415 | node                       |
| worker startup      |   0.10 |  108.0 ms |  234.9 ms |     1.0000 |     2.1756 | node                       |
| **Composite**       |   1.00 |           |           | **1.2933** | **1.2358** | deno (margin 4.7 % < 10 %) |

Tie-breaker applied: **peak RSS** — node 146.4 MiB versus deno 108.3 MiB (deno lower) → **Deno**.

Secondary (not weighted, recorded for the gate): point-read p95 0.052 vs 0.073 ms (node);
worker round-trip p95 0.165 vs 1.39 ms (node); `SIGTERM` shutdown 18.9 vs 14.7 ms (deno); runtime binary
74.6 vs 136.0 MiB (node); compiled single binary — node blocked, deno 199.4 MiB.

## 5. Hard-fail list — result: none tripped

| ID                 | node  | deno  | Evidence                                            |
| ------------------ | ----- | ----- | --------------------------------------------------- |
| `HF-CORRECTNESS`   | clear | clear | 9 driver checks pass on both arms                   |
| `HF-SINGLE-WRITER` | clear | clear | concurrent writer refused, not visible, recovery ok |
| `HF-INTEGRITY`     | clear | clear | `integrity_check=ok` after 1000 appends             |
| `HF-DATA-LOSS`     | clear | clear | 1000 issued, 1000 stored                            |
| `HF-TAIL-LATENCY`  | clear | clear | p95 6.7 / 8.4 ms, threshold 250 ms                  |
| `HF-APPEND-TAIL`   | clear | clear | p95 7.4 / 9.2 ms, threshold 100 ms                  |
| `HF-ISOLATION`     | clear | clear | crash observed, parent survived, heartbeat ran      |
| `HF-SIGNAL`        | clear | clear | handler invoked, 23.6 / 7.2 ms, exit 0              |
| `HF-PERMISSION`    | clear | clear | no configured denial succeeded                      |

Hazards recorded but not disqualifying (both arms): a synchronous SQLite busy handler parks the caller's own
timer queue for the whole timeout. The ledger must therefore serialize writes at the application level
(one sequencer + queue) and must not rely on SQLite busy waiting; this is a design constraint for `B2`, not a
runtime difference.

## 6. Rejected alternative — Node 24

Node is rejected **on this protocol and this host**, not "in general":

- cold start to ready is 1.96× slower (567 ms vs 289 ms p50; 1.61× on p95);
- peak RSS is 1.35× higher; RSS after boot 1.40× higher (145 vs 104 MiB);
- network containment is not expressible inside the runtime (no network capability in `--permission`);
- single-binary packaging is unavailable in this environment (SEA + postject), so deployment would carry the
  full `node_modules` closure (54 MiB for the pinned `effect` tree) as well as the 74.6 MiB runtime;
- it wins on durable append (p95 7.16 vs 8.64 ms, 1.62× throughput), event-loop p50 (1.43 vs 5.88 ms), warm
  start (0.81× the time) and worker isolation (startup 2.18× faster, round-trip p95 8.4× faster).

Those Node advantages are real and matter for the append-heavy path and for per-task worker isolation. They are
weighted at 0.6 of the composite and do not offset the cold-start/RSS gap by the pre-registered margin.

## 7. Residual risks

1. **The composite is inside the margin in every run (1.5–4.5 %).** The outcome rests on a pre-registered
   tie-breaker, not on a clear performance win. A future change in worker model, or a different filesystem,
   can flip the weighted composite.
2. **One full workload run per arm per session.** Cold start has 11 samples per arm; append/read and event-loop
   distributions have 1000+ samples, but each arm's full run happened once. The four archived runs bound the
   run-to-run spread (tables above) rather than a confidence interval.
3. **SQLite micro-version differs** (Node 3.53.3, Deno 3.53.2). Same API surface, not the same build.
4. **Environment is unverified for containment.** `sandboxPosture=unverified`, `brokerIdentity=unconfigured`.
   Both arms ran with the operator's OS permissions; no OS sandbox was applied or measured.
5. **Warm start is app-layer bring-up**, not a second process launch; the process-launch cost is in the cold
   numbers.
6. **The harness host is Deno.** Timing is taken from outside the arm, but the spawn path, `/proc` sampling and
   `du` calls run on Deno. This is a harness choice, not a runtime advantage: it is identical for both arms.
7. **Node carries a boundary-adapter cost** (resolve hook + `tmp` install). That cost is part of running this
   repo's Effect-first code on Node and is included in the cold-start numbers; a production Node build could
   bake the module graph and remove it.
8. **Filesystem-specific durability.** Append latency reflects this host's filesystem. Absolute numbers will
   move on other hardware; the _ratio_ is the transferable part.
9. **Deno's `node:sqlite` lacks `errcode`/`errstr`**, so error classification must not depend on them.

## 8. Protocol gaps and blockers

These are gaps in the _protocol binding_, not silent defaults. They are listed in `result.json`
(`unboundProbes`, `blocked`) and must be resolved before `DG-2` is signed as fully conformant.

| Gap                                     | Detail                                                                                                                                                                                                                                                                                                                                                                                                                                                                  | Owner                                                                  |
| --------------------------------------- | ----------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ---------------------------------------------------------------------- |
| `security-v1` is not a sealed dataset   | `S1` cites `cost-v1` + `security-v1`. The runtime arm binds `append-v1@1.0.0` (its ledger workload) and does not consume `cost-v1`, which is a model/broker workload owned by the `LANG-1` arm (`A4`). `security-v1` is absent from `fixtures/registry.json`, so the permission and fail-closed probes are evidence-only and unbound. (`cost-v1@1.0.0` and `ui-v1@1.0.0` were registered by concurrent work after this run started; `append-v1` hashes did not change.) | `A0` extension (cannot be created here: `fixtures/**` is out of scope) |
| Node single-binary packaging unmeasured | Node 24 single-executable applications require a SEA blob injected with postject; that toolchain is not in this sandbox. Runtime binary and dependency closure are measured instead. Marked `blocked` in `result.json`.                                                                                                                                                                                                                                                 | `A5` / packaging task                                                  |
| systemd integration not measured        | `A1`'s scope lists systemd integration. The user manager here reports `degraded` and is shared with other work; installing and starting units was judged unsafe for a paired measurement in this session. Signal handling is measured instead (the property systemd actually consumes).                                                                                                                                                                                 | `A2` (deployment plane) is the better owner                            |
| OpenTelemetry/tracing not measured      | Requires a third dependency stack (`@opentelemetry/*`) for both arms; not installed, not measured.                                                                                                                                                                                                                                                                                                                                                                      | `A5` / observability task                                              |
| Security probes not red-team grade      | Permission fail-closed behaviour is measured; secret-canary reachability, redirect and broker-abuse probes belong to `security-v1` and were not run.                                                                                                                                                                                                                                                                                                                    | `E*` red-team tasks                                                    |

## 9. Recommendation for `DG-2`

**Select Deno 2.9.2 as the TypeScript control-plane runtime, subject to the human accepting the protocol gaps
above.**

Rationale, in the order the pre-registration ranks it:

1. no hard fail on either arm, so the choice is a preference, not a disqualification;
2. the weighted composite is inside the 10 % margin (4.7 % in the published run), so the decision is made by the
   pre-registered tie-breaker;
3. the first tie-breaker, peak RSS, favours Deno in all five runs (≈ 1.23–1.53× lower), which is the property
   the control plane shares with agents on one host;
4. Deno's cold start to ready is 1.4–2.0× faster in every run, which shortens supervisor restarts;
5. Deno's inner permission model denies file, network and process access by default, while Node's model has no
   network capability at all — the OS sandbox remains mandatory either way, but Deno's inner model fails closed
   on a wider surface;
6. Deno compiles to a single 199 MiB binary in this environment; Node cannot, and would deploy a 74.6 MiB
   runtime plus a 54 MiB dependency closure.

Conditions the gate record should carry:

- **Do not treat this as a performance win.** Re-run the paired protocol after the worker model and the ledger
  sequencer exist (`B2`), because append throughput (Node +62 %) and worker startup (Node 2.2× faster) are the
  two places where the current result could invert.
- **Serialize canonical writes in the application**, never with a SQLite busy handler (hazard measured on both).
- **Keep the runtime-swap seam.** `ArmHost` + the runtime entrypoints show the minimum surface a swap touches;
  keep domain code free of `node:*` and `Deno.*`.
- **Node's fallback cost is real but bounded:** ~280 ms extra cold start to ready and ~38 MiB extra peak RSS,
  with a resolve hook standing in for the import map.

Rejected alternative for the record: **Node 24** (reasons in §6). If the gate rejects Deno, the same evidence
supports Node with no re-measurement, at the cost of the RSS/cold-start/network-containment/packaging
differences above.

## 10. Reproduction

```sh
# Everything below runs from the repository root.
# 1. Node dependency install for the arm (the CLI does this automatically on first run):
#    npm install --prefix tmp/spikes/runtime/node-deps effect@4.1.2
# 2. Paired run; writes spikes/runtime/result.json and prints the tables:
deno run -A src/spikes/runtime/cli.ts
# 3. Targeted checks:
deno check src/spikes/runtime/cli.ts src/spikes/runtime/deno/main.ts src/spikes/runtime/node/main.ts \
  tests/spikes/runtime/*.test.ts
deno test --allow-read --allow-write --allow-env --allow-run --allow-net --allow-sys tests/spikes/runtime
```

Node binary resolution: `KANBEI_NODE_BIN=/path/to/node` overrides the default `nix build nixpkgs#nodejs_24`
lookup. Node dependencies are installed under `tmp/` (git-ignored) at the version pinned in `deno.json`.

`deno task verify` was deliberately not run for this task: several agents write to this worktree concurrently, so
verification was scoped to the files above (`deno check`, `deno lint`, `oxlint`, `deno test`, `deno fmt --check`
on `src/spikes/runtime`, `tests/spikes/runtime`, `spikes/runtime`). Run the full gate once the worktree is quiet.

## 11. Files

| Path                                                                         | Role                                                            |
| ---------------------------------------------------------------------------- | --------------------------------------------------------------- |
| `src/spikes/runtime/arm.ts`                                                  | the shared measured program (both arms)                         |
| `src/spikes/runtime/ledger.ts`                                               | durable append/read workload + hash chain                       |
| `src/spikes/runtime/sqlite.ts`, `sqlite-checks.ts`                           | driver interface, correctness and single-writer probes          |
| `src/spikes/runtime/event-loop.ts`                                           | lag sampler                                                     |
| `src/spikes/runtime/metrics.ts`                                              | percentile/summary reduction                                    |
| `src/spikes/runtime/decision.ts`                                             | pre-registered hard fails, composite, decision rule             |
| `src/spikes/runtime/driver.ts`                                               | harness host: spawns, times, packages, aggregates               |
| `src/spikes/runtime/envelope.ts`                                             | `EvidenceEnvelope` binding through the A0 harness               |
| `src/spikes/runtime/cli.ts`                                                  | entrypoint that writes `result.json`                            |
| `src/spikes/runtime/plan.ts`                                                 | frozen pre-registration                                         |
| `src/spikes/runtime/adapters/*`                                              | `Deno`/`node`/`node:sqlite`/`worker_threads`/process boundaries |
| `src/spikes/runtime/deno/main.ts`, `node/main.ts`, `node/register-hooks.mjs` | runtime entrypoints and the Node resolve hook                   |
| `tests/spikes/runtime/*.test.ts`                                             | 28 targeted tests                                               |
| `spikes/runtime/result.json`                                                 | machine-readable result, decision matrix and envelope           |
