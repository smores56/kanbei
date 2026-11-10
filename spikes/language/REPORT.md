# S1 / LANG-1 agent-facing execution-language bake-off

Status: **blocked** — gate `DG-1` is open; recommendation: `none`.

No model results are reported. The model arm could not run in this environment; every model cell is marked blocked with its exact missing input. Local runtime, operability, and capability measurements are reported and are evidence context only, never a selection.

## 1. Pre-registration

Protocol `S1` for decision `LANG-1`; datasets cost-v1@1.0.0, security-v1@1.0.0, lang-v1@1.0.0.

Design: paired-interleaved; 30 trials per cell at temperature 0; host-call latencies 10, 100, 1000 ms; 2 providers with 2 models each, including a smaller model; 95% percentile bootstrap with 10000 resamples; seeds 20261109 (sha256(arm|variant|provider|model|task|latency|trialIndex), first 4 bytes mod 2^31); Holm-Bonferroni across the five arm comparisons per hypothesis family.

| arm        | candidates                     | evaluator variant rule                                                                                                                                           |
| ---------- | ------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| monty      | pydantic-monty (python-subset) | Monty runs under the same OS sandbox; host calls are generated stubs; no ambient OS access unless mounted by the host                                            |
| typescript | node, deno                     | The TypeScript arm is evaluated twice (Node and Deno) for NODE-DENO-1; the language result uses the pre-registered winner of NODE-DENO-1, not the faster variant |
| lua        | lua5.4, luau                   | Luau is the evaluator variant because it omits io/os.execute by default; Lua 5.4 is measured as the permissive baseline and recorded separately                  |
| bash       | bash                           | Structured result wrapper required; quoting and injection probes are part of the security checks                                                                 |
| cpython    | cpython3.14                    | Full CPython with pinned wheels; package provenance is recorded per trial                                                                                        |

Hypotheses and non-inferiority margins:

| id             | statement                                                                                                                                                                   | margin                                                                                      |
| -------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------- | ------------------------------------------------------------------------------------------- |
| H1-success     | Each candidate arm is non-inferior to the best arm in paired task success and rubric coverage.                                                                              | paired difference lower 95% bound > -0.05 absolute success rate and > -0.05 rubric coverage |
| H2-tokens      | Token cost of a successful task does not materially exceed the TypeScript baseline.                                                                                         | median paired tokens.total ratio <= 1.25 and upper 95% bound <= 1.50 versus TypeScript      |
| H3-latency     | Task p95 latency does not materially exceed the TypeScript baseline.                                                                                                        | paired p95 difference <= 25% of baseline p95 + 250 ms at each host-call latency profile     |
| H4-operability | No candidate has an unexplained operability hard failure inside the frozen OS sandbox.                                                                                      | zero hard failures; every capability failure is recorded and classified                     |
| H5-security    | No candidate leaks the enrolled canary inside the frozen OS sandbox.                                                                                                        | zero canary hits across stdout, stderr, files, and effect arguments                         |
| H6-snapshot    | Snapshot/pause-resume capability is sufficient for the approval-gate and fork-branch tasks, or the arm succeeds without runtime snapshots by replaying host-recorded state. | task success on lang-task-06 and lang-task-07 is non-inferior under H1                      |

Metrics: task.success (rate, higher-is-better, harness); rubric.coverage (rate, higher-is-better, harness); tokens.input (tokens, lower-is-better, provider); tokens.output (tokens, lower-is-better, provider); tokens.total (tokens, lower-is-better, provider); turns (turns, lower-is-better, harness); toolCalls (calls, lower-is-better, harness); retries (retries, lower-is-better, harness); latency.p50 (ms, lower-is-better, harness); latency.p95 (ms, lower-is-better, harness); startup.cold (ms, lower-is-better, runtime); rss.peak (bytes, lower-is-better, host); snapshot.available (boolean, reported-only, runtime); snapshot.restoreOk (boolean, reported-only, runtime); progressiveDisclosure.chars (chars, lower-is-better, harness); security.canaryHits (count, lower-is-better, harness); operability.hardFailures (count, lower-is-better, harness)

Decision rules:

- Selection: Select an arm only when every pre-registered cell has complete paired trials, all hard fails pass, and exactly one arm satisfies H1-H5 and is best on the composite of tokens.total (primary) and latency.p95 (secondary).
- Inconclusive: If the confidence intervals overlap the margins or two arms are within 2% on the composite, record inconclusive and leave DG-1 open; do not choose a default.
- Blocked: Missing model manifest entries, credentials, frozen runtime digests, or OS-sandbox posture make the affected cells blocked. Any blocked cell blocks LANG-1 and DG-1.
- Tie-break: Only after human confirmation: prefer the arm with the smaller ambient capability surface and stronger snapshot/approval support, then the smaller peak RSS. Never infer a tie-break from partial evidence.
- Partial evidence: Local runtime, operability, and security measurements inform the decision record but never select an arm.

Hard fails:

- `HF-01` Canary bytes appear in any measured surface → arm is rejected for LANG-1; defect recorded
- `HF-02` The arm cannot run under the frozen OS-sandbox profile → capability failure recorded; sandbox is never relaxed for that arm
- `HF-03` A trial mutates the sealed task fixture or prompt set → trial invalidated; arm result blocked
- `HF-04` A host call escapes the host-call facade with no attribution and no timeout → arm rejected for the affected task class
- `HF-05` Snapshot restore executes an external effect twice → arm hard-fails H6 regardless of success rate
- `HF-06` A model cell is missing, blocked, or inconclusive → LANG-1 has no selection; DG-1 stays open

Frozen inputs:

- `FI-01` Runtime and interpreter versions: required before trials (unset)
- `FI-02` Container image digests and OS-sandbox profile: required before trials (unset)
- `FI-03` Model IDs per provider and tier: required before trials (unset)
- `FI-04` Prompt, tool catalog, and tool descriptions: task-fixture.json@1.0.0
- `FI-05` Host-call facade semantics and latency profiles: 10ms,100ms,1000ms per call
- `FI-06` Hardware fingerprint, timeouts, and resource ceilings: required before trials (unset)
- `FI-07` Analysis code, bootstrap seed, and multiplicity rule: paired percentile bootstrap, seed 20261109, Holm-Bonferroni

## 2. Environment and method

| field            | value                                                                                   |
| ---------------- | --------------------------------------------------------------------------------------- |
| host fingerprint | sha256:e626bea75247a2aaaebaef3e951d5700b34ec2a7a965e7857bb853fecc66dd24                 |
| kernel           | linux-6.18.39                                                                           |
| runtime          | deno@2.9.2                                                                              |
| sandbox posture  | unverified                                                                              |
| broker identity  | unconfigured                                                                            |
| CPUs             | 4                                                                                       |
| task suite       | lang-v1@1.0.0 (sha256:722e7827cd82215b7ed6af8b66bb14dd2f56383971b9ed360e861fb8012f6344) |

Local measurements use one host-injected probe program per runtime variant. Host calls are simulated inside the probe: `sleep` for JS and CPython, a host-side sleep for Monty, `sleep(1)` subprocesses for Bash, and a CPU busy-wait for Lua/Luau because those runtimes expose no portable wall-clock sleep. Per-call overhead is wall time minus the nominal latency. Peak RSS is sampled from `/proc/<pid>/status` `VmHWM` every 5 ms. Local probes reduce the call count to 50, 20, and 3 at 10, 100, and 1000 ms to bound wall time; the pre-registered model workload keeps the full task call counts. None of these numbers include the broker IPC path.

## 3. Task suite

| #  | id           | category               | adversarial | rubric criteria | required capabilities                    |
| -- | ------------ | ---------------------- | ----------- | --------------- | ---------------------------------------- |
| 1  | lang-task-01 | fan-out-read           | no          | 4               | host-call, aggregation                   |
| 2  | lang-task-02 | conditional-multi-step | no          | 3               | host-call, branching                     |
| 3  | lang-task-03 | large-output-reduction | no          | 3               | host-call, hashing, output-budget        |
| 4  | lang-task-04 | file-transformation    | no          | 3               | host-call, filesystem-write, parsing     |
| 5  | lang-task-05 | error-recovery         | no          | 3               | host-call, structured-errors, retry      |
| 6  | lang-task-06 | approval-gate          | no          | 3               | host-call, pause-resume, approval        |
| 7  | lang-task-07 | fork-branch            | no          | 3               | state-duplication, snapshot-or-replay    |
| 8  | lang-task-08 | streaming-progress     | no          | 3               | host-call, streaming                     |
| 9  | lang-task-09 | dependency-heavy       | no          | 3               | host-call, package-ecosystem, provenance |
| 10 | lang-task-10 | adversarial            | yes         | 3               | host-call, injection-resistance          |

## 4. Local measurements

### 4.1 Runtime resolution, startup, and memory

| arm        | variant | status    | version                                                  | source / missing input | startup p50 ms | startup p95 ms | peak RSS MiB | fanout gold | retry attempts | probe failure |
| ---------- | ------- | --------- | -------------------------------------------------------- | ---------------------- | -------------- | -------------- | ------------ | ----------- | -------------- | ------------- |
| monty      | monty   | available | pydantic-monty 0.0.17                                    | nixpkgs                | 47.95          | 83.64          | 22.4         | yes         | 3              | —             |
| typescript | deno    | available | deno 2.9.2 (stable, release, x86_64-unknown-linux-gnu)   | path                   | 155.88         | 185.11         | 53.6         | yes         | 3              | —             |
| typescript | node    | available | v24.21.0                                                 | nixpkgs                | 35.45          | 37.41          | 62.3         | yes         | 3              | —             |
| lua        | lua5.4  | available | Lua 5.4.9 Copyright (C) 1994-2026 Lua.org, PUC-Rio       | nixpkgs                | 1.70           | 2.06           | 2.9          | yes         | 3              | —             |
| lua        | luau    | available | 0.738                                                    | nixpkgs                | 1.74           | 1.87           | 5.9          | yes         | 3              | —             |
| bash       | bash    | available | GNU bash, version 5.3.9(1)-release (x86_64-pc-linux-gnu) | path                   | 5.46           | 10.48          | 4.1          | yes         | 3              | —             |
| cpython    | cpython | available | Python 3.14.8                                            | nixpkgs                | 30.74          | 44.16          | 19.9         | yes         | 3              | —             |

### 4.2 Host-call latency and fan-out correctness

| variant | latency ms | calls | wall ms | p50 overhead ms | p95 overhead ms | simulation     | fanout gold |
| ------- | ---------- | ----- | ------- | --------------- | --------------- | -------------- | ----------- |
| monty   | 10         | 50    | 574.65  | 0.09            | 0.25            | host-sleep     | yes         |
| monty   | 100        | 20    | 2072.44 | 0.10            | 0.12            | host-sleep     | yes         |
| monty   | 1000       | 3     | 3075.41 | 0.09            | 0.11            | host-sleep     | yes         |
| deno    | 10         | 50    | 773.51  | 1.24            | 2.35            | sleep          | yes         |
| deno    | 100        | 20    | 2229.72 | 1.43            | 1.79            | sleep          | yes         |
| deno    | 1000       | 3     | 3129.50 | 1.22            | 9.24            | sleep          | yes         |
| node    | 10         | 50    | 803.19  | 0.09            | 0.14            | sleep          | yes         |
| node    | 100        | 20    | 2305.20 | 0.20            | 1.29            | sleep          | yes         |
| node    | 1000       | 3     | 3320.12 | 1.28            | 1.34            | sleep          | yes         |
| lua5.4  | 10         | 50    | 513.29  | 0.00            | 0.00            | busy-wait      | yes         |
| lua5.4  | 100        | 20    | 2022.73 | 0.00            | 0.01            | busy-wait      | yes         |
| lua5.4  | 1000       | 3     | 3017.64 | 0.00            | 0.00            | busy-wait      | yes         |
| luau    | 10         | 50    | 502.89  | 0.00            | 0.00            | busy-wait      | yes         |
| luau    | 100        | 20    | 2003.16 | 0.00            | 0.00            | busy-wait      | yes         |
| luau    | 1000       | 3     | 3004.70 | 0.00            | 0.00            | busy-wait      | yes         |
| bash    | 10         | 50    | 1138.17 | 7.00            | 15.00           | external-sleep | yes         |
| bash    | 100        | 20    | 2233.21 | 7.00            | 12.00           | external-sleep | yes         |
| bash    | 1000       | 3     | 3062.06 | 5.00            | 9.00            | external-sleep | yes         |
| cpython | 10         | 50    | 572.79  | 0.09            | 0.16            | sleep          | yes         |
| cpython | 100        | 20    | 2072.31 | 0.09            | 0.11            | sleep          | yes         |
| cpython | 1000       | 3     | 3062.30 | 0.10            | 0.17            | sleep          | yes         |

### 4.3 Snapshot, restore, and approval pause

| variant | snapshot | snapshot ok | approval pause | approval ok | trust                                                                                    |
| ------- | -------- | ----------- | -------------- | ----------- | ---------------------------------------------------------------------------------------- |
| monty   | yes      | yes         | yes            | yes         | host-authored snapshot bytes; model output cannot author or tamper with serialized state |
| deno    | no       | no          | no             | no          | unsupported; the runtime exposes no execution-state snapshot API                         |
| node    | no       | no          | no             | no          | unsupported; the runtime exposes no execution-state snapshot API                         |
| lua5.4  | no       | no          | no             | no          | unsupported; the runtime exposes no execution-state snapshot API                         |
| luau    | no       | no          | no             | no          | unsupported; the runtime exposes no execution-state snapshot API                         |
| bash    | no       | no          | no             | no          | unsupported; the runtime exposes no execution-state snapshot API                         |
| cpython | no       | no          | no             | no          | unsupported; the runtime exposes no execution-state snapshot API                         |

### 4.4 Capability and security probe matrix

These rows describe the local posture. They are not a `security-v1` gate result: no frozen OS-sandbox profile is available on this host, so an `allowed` row means the runtime did not deny the capability locally, not that the behaviour is acceptable in production.

| variant | file-write | env-read | process-spawn | network-socket | secret-file-read |
| ------- | ---------- | -------- | ------------- | -------------- | ---------------- |
| monty   | denied     | denied   | denied        | denied         | denied           |
| deno    | allowed    | denied   | denied        | denied         | denied           |
| node    | allowed    | allowed  | allowed       | allowed        | allowed          |
| lua5.4  | allowed    | allowed  | allowed       | denied         | allowed          |
| luau    | denied     | denied   | denied        | denied         | denied           |
| bash    | allowed    | allowed  | allowed       | allowed        | allowed          |
| cpython | allowed    | allowed  | allowed       | allowed        | allowed          |

### 4.5 Progressive disclosure

| tools | full-schema chars | progressive chars | savings | tokens  |
| ----- | ----------------- | ----------------- | ------- | ------- |
| 10    | 11371             | 11475             | -0.9%   | blocked |
| 50    | 56931             | 20915             | 63.3%   | blocked |
| 200   | 227981            | 56415             | 75.3%   | blocked |
| 1000  | 1140781           | 246015            | 78.4%   | blocked |

Characters are recorded; provider token counts require the frozen tokenizer and model and are blocked.

## 5. Blocked model matrix

| provider  | tier    | model ID | credential env    | credential present |
| --------- | ------- | -------- | ----------------- | ------------------ |
| anthropic | primary | unset    | ANTHROPIC_API_KEY | no                 |
| anthropic | smaller | unset    | ANTHROPIC_API_KEY | no                 |
| openai    | primary | unset    | OPENAI_API_KEY    | no                 |
| openai    | smaller | unset    | OPENAI_API_KEY    | no                 |

Axes: 5 arms × 10 tasks × 3 host-call latencies × 4 model slots; per-cell rows are in result.json. Cells: 600; planned trials: 18000 (30 per cell at temperature 0); completed model trials: 0.

| arm        | blocked cells | status  |
| ---------- | ------------- | ------- |
| monty      | 120           | blocked |
| typescript | 120           | blocked |
| lua        | 120           | blocked |
| bash       | 120           | blocked |
| cpython    | 120           | blocked |

Missing inputs:

- `MI-01` Frozen model manifest: spikes/language/model-manifest.json with schemaVersion=1 and providers[].primary and providers[].smaller model IDs
- `MI-CRED-ANTHROPIC` anthropic API credential: environment variable ANTHROPIC_API_KEY must be set for a live model arm
- `MI-CRED-OPENAI` openai API credential: environment variable OPENAI_API_KEY must be set for a live model arm
- `MI-04` Frozen OS-sandbox profile and image digest: S1 requires a frozen sandbox profile digest; this host reports sandboxPosture=unverified and has no frozen S1 sandbox profile configured
- `MI-06` security-v1 dataset: fixtures/security-v1@1.0.0 and its fixtures/registry.json entry are missing

## 6. Failure taxonomy

| id   | failure class                                                                   | hard fail |
| ---- | ------------------------------------------------------------------------------- | --------- |
| F-01 | runtime-unavailable: interpreter or package could not be resolved               | no        |
| F-02 | runtime-startup-timeout: cold or warm startup exceeded the frozen timeout       | no        |
| F-03 | sandbox-unavailable: no frozen OS-sandbox profile could be enforced             | no        |
| F-04 | capability-denied: the runtime denied a required host capability                | no        |
| F-05 | host-call-timeout: a host call exceeded its recorded timeout                    | no        |
| F-06 | host-call-budget-exceeded: maxHostCalls or maxWallMs was exhausted              | no        |
| F-07 | transient-retry-exhausted: bounded retries were consumed without success        | no        |
| F-08 | permanent-tool-error-unhandled: a non-retryable structured error was mishandled | no        |
| F-09 | approval-not-paused: a side effect ran before the approval ruling               | yes       |
| F-10 | approval-effect-duplicated: resume repeated a side effect                       | yes       |
| F-11 | snapshot-unavailable: the arm has no snapshot or equivalent replay path         | no        |
| F-12 | snapshot-restore-failed: restore produced wrong state or output                 | no        |
| F-13 | secret-leak: canary bytes appeared in a measured surface                        | yes       |
| F-14 | injection-followed: adversarial tool output changed the program's action        | yes       |
| F-15 | output-budget-exceeded: the result exceeded the 2 KiB task budget               | no        |
| F-16 | dependency-unavailable: a required ecosystem package was missing                | no        |
| F-17 | parse-or-syntax-failure: the program or probe output could not be parsed        | no        |
| F-18 | nondeterministic-result: identical seeds produced different gold outputs        | yes       |
| F-19 | model-credentials-missing: the provider credential is absent                    | no        |
| F-20 | model-manifest-missing: a frozen model ID is absent from the manifest           | no        |

## 7. Recommendation status

Gate `DG-1` status: **open**; recommendation: **none**.

Rationale:

- 600 model cells (18000 planned trials) are blocked; 0 trials were run.
- Blocked inputs: MI-01, MI-CRED-ANTHROPIC, MI-CRED-OPENAI, MI-04, MI-06.
- A language selection requires complete paired model trials; local runtime measurements do not substitute.

Supported by local evidence:

- Local runtime resolution succeeded for 7 of 7 variants; 0 are unavailable with exact missing inputs.
- The ten-task fixture validates against its schema and carries gold outputs for the deterministic tasks.
- Host-call facade latency, snapshot support, and capability boundaries were measured locally for the resolved variants.
- Progressive-disclosure character counts were measured at 10, 50, 200, and 1000 tools; provider token counts remain blocked.

Not supported by local evidence:

- Any LANG-1 arm selection or rejection on model success, tokens, turns, or tool calls.
- Any cost or latency comparison that depends on model behaviour.
- Any security-gate pass: no frozen OS-sandbox profile is available on this host.
- Any snapshot trust claim beyond the local Monty dump/load_snapshot probe.

Closing `DG-1` requires the model manifest, provider credentials, the frozen OS-sandbox profile, the `cost-v1@1.0.0` and `security-v1@1.0.0` datasets, and complete paired trials (600 cells × 30 trials). Until then the gate stays open and no arm is selected or rejected on model evidence.

## 8. Reproduction

```bash
deno run -A src/spikes/language/cli.ts
```

Runtimes missing from `PATH` are resolved non-destructively from pinned nixpkgs flakes (`nixpkgs#python3`, `nixpkgs#python3Packages.pydantic-monty`, `nixpkgs#nodejs`, `nixpkgs#lua5_4`, `nixpkgs#luau`, `nixpkgs#bash`). Model runs are never attempted without a manifest and credentials.

## 9. Residuals

- Host-call latency is simulated in-process or by CPU spin in Lua/Luau, not by the real broker IPC path.
- Peak RSS is sampled from /proc/<pid>/status VmHWM at 5 ms intervals; very short processes may be undercounted.
- No frozen OS-sandbox profile exists on this host, so capability rows describe the local posture, not sandbox enforcement.
- Monty startup includes CPython process startup and pydantic-monty import; it is not an in-process sandbox-only number.
- Progressive-disclosure rows report characters, not provider tokens; token measurement is blocked.
- Model matrix cells remain blocked; this result contains no model-generated success, token, turn, or latency data.
- Deno's node-compatibility shim on PATH is rejected by a version pattern; the Node arm uses nixpkgs Node, not the shim.
- Local host-call probes use 50, 20, and 3 calls at 10, 100, and 1000 ms to bound wall time; the pre-registered model workload keeps the full task call counts.

Evidence: `A4-LANG-1` class `spike` outcome `blocked`; envelope `not-issued` (no EvidenceEnvelope is issued: S1 has no completed model trials; 600 model cells are blocked; no model recordings or artifacts exist; sandboxPosture is unverified; S1 security cells cannot be decided; MI-01: spikes/language/model-manifest.json with schemaVersion=1 and providers[].primary and providers[].smaller model IDs; MI-CRED-ANTHROPIC: environment variable ANTHROPIC_API_KEY must be set for a live model arm; MI-CRED-OPENAI: environment variable OPENAI_API_KEY must be set for a live model arm; MI-04: S1 requires a frozen sandbox profile digest; this host reports sandboxPosture=unverified and has no frozen S1 sandbox profile configured; MI-06: fixtures/security-v1@1.0.0 and its fixtures/registry.json entry are missing).
