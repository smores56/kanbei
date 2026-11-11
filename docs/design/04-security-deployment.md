# Threat model, sandbox, channels, and deployment

Status: normative implementation contract for this section. It is the rebase of the pre-split security draft onto the deployed split design (SDLC #509). The VM boundary replaces the earlier broker/sandbox plane; `06-validation-plan.md` is superseded and the earlier `06` section 6 broker is no longer part of Kanbei.

## 1. Security invariants

| ID | Invariant |
|---|---|
| S-1 | The host kernel, the root-only host artifacts, the Nix-built image and its frozen trust root, the human, and the audit sink are trusted. All agent, plugin, tool, and ingested-content-derived code is untrusted. |
| S-2 | Mutable code never executes inside a process that holds canonical-ledger write authority or frozen-root control. |
| S-3 | The Kanbei VM holds no provider credential. The only remote provider key is the TinyFish key in the Search VM; ingest credentials live only in their own ingest VMs. |
| S-4 | No process in the Kanbei VM can open an outbound connection. The VM has no NIC and its device surface is exactly the six channels of §4. |
| S-5 | Agent-visible data access is limited to the Inbox and Vault mounts (read-only), the Todo mount (read-write within its cap), per-sandbox scratch, and agent-mutable Kanbei paths. Image content is read-only and not project data. |
| S-6 | All canonical writes pass through the frozen append API. No agent or mutable plugin opens the canonical SQLite file. |
| S-7 | The six channels are the only byte paths into, out of, or across the VM: Inbox read-only, Vault read-only, Todo read-write size-capped, Chat over vsock, CSR over vsock, Search over vsock. Any other path is a channel leak. |
| S-8 | GPU-lane occupancy, ingest volume, CAS growth, Todo bytes, chat rate, search budget, writable-root, canonical-append, audit, and activation boundaries are hard human-owned limits. Approvals never override them. |
| S-9 | Every mediated external effect produces an attempt and a receipt through the frozen append path. Replay never repeats an external effect. |
| S-10 | Channel operations fail closed. There is no fallback path, no environment-variable credential, no host share, and no undeclared port. |
| S-11 | The frozen trust root changes only by a human-merged commit built into a new image through a human-approved Nix generation. Agents may propose but cannot live-edit it. |
| S-12 | A sandbox starts only after the supervisor verifies the declared enforcement class and the declared channel set. A weaker-than-declared posture or an extra channel is a start failure, not a silent downgrade. |
| S-13 | Inter-agent isolation inside the VM is fault and resource containment only. It is not a security boundary; the VM boundary is. |
| S-14 | The UI is a static, human-owned bundle with a strict CSP; it binds loopback inside the VM and is reached only through the host relay and the WARP private route. It issues no request to any origin but the Kanbei API. |
| S-15 | All mutable Kanbei state lives on the in-guest LUKS volume under `/var/lib/kanbei/`; image content and secret material are outside it and are covered by the image backup. |
| S-16 | Agent-authored code runs only under Monty, with no ambient filesystem, network, or subprocess access. Host calls are the only side effects and are effects under §7. |
| S-17 | Ingest output enters only as age-encrypted batches written by the host writer. The VM cannot write the Inbox, and no plaintext source credential is present in the VM. |

## 2. Trust boundaries and process identities

### 2.1 Adversary statement

The adversary is the agent process inside Kanbei: prompt-injected or fully compromised, with arbitrary code execution under Monty and, if Monty or the harness is broken, arbitrary code in the VM. It controls its program, written files inside the writable mounts, and every byte it sends to a channel. It can read any file its OS identity can read inside the VM and can attempt a VM escape or a kernel exploit. Ingest batches, vault content, and search results are hostile input. The host, the human, the image, and the frozen trust root are trusted.

The adversary cannot open a socket, resolve a name, or reach the host filesystem beyond the declared mounts: there is no interface to do so. Its residual reach is the Search channel, the Chat channel, the Todo channel, and a VM escape.

### 2.2 Process identity contract

Every in-VM process has a declared role; roles are fixed by the image and are not representable by mutable code.

```ts
type ComponentKind =
  | "core" | "sequencer" | "llm" | "monty_host"
  | "channel" | "agent" | "plugin_host" | "ui" | "consent" | "watchdog";

type ProcessIdentity = {
  component: ComponentKind;
  instanceId: string;            // unique per process start
  osUid: number;
  generation: NixGenerationId;
  cgroupPath: string;
  channels: ChannelId[];         // exactly the manifest for this role
  sandboxId?: SandboxId;         // agent and plugin hosts only
  agentId?: AgentId;
  loopId?: LoopId;
  scopeId?: IsolationScope;
  startToken: Sha256;            // supervisor-issued, bound to sandboxId + cgroup
};
```

The channel host accepts a caller only after all of the following:

1. Unix-socket peer credentials identify the expected in-VM user for that channel.
2. The presented `startToken` matches a live sandbox issued by the supervisor.
3. The peer's cgroup path matches the sandbox record.
4. The sandbox generation matches the running generation.
5. The requested operation is in the channel's schema.

Inside one VM these checks are attribution and accident prevention, not a security boundary between agents: every process shares the VM. S-13 applies. The security boundary is the VM and its channel set; a same-VM process that breaks Monty and the supervisor can read what the VM can read, and still cannot leave except through §4.

### 2.3 Process table

| Component | Trust | Loads mutable code | Holds secrets | Writable state |
|---|---|---|---|---|
| `kanbei-core` | trusted | no | no | canonical ledger and scoped-state metadata, via frozen APIs |
| `kanbei-sequencer` | trusted | no | no | ledger write-ahead state |
| `llama.cpp` (`kanbei-llm`) | trusted image code, untrusted model output | no | no | KV cache and lane state only |
| `kanbei-monty-host` | trusted | hosts untrusted programs | no | per-sandbox scratch |
| `kanbei-channel-<name>` | trusted | no | no | channel spool and redacted audit spool |
| `kanbei-agent-<sandbox>` | untrusted | yes | no | own agent state and scratch |
| `kanbei-plugin-<scope>` | untrusted | yes | no | own scoped store and candidate directory |
| `kanbei-ui` | frozen static bundle | no (declarative data only) | no | none canonical; read-only API client |
| `kanbei-consent` | trusted, frozen | no | no | none; origin-isolated renderer that mints the single-use consent token and `consentDigest` |
| `kanbei-watchdog` | trusted, in-VM and host-side counterpart | no | no | none agent-writable |

`kanbei-core` holds no provider credential. Model calls are local `llama.cpp` calls on a declared lane; the model's outputs are untrusted content that only the append path may turn into observations. `kanbei-ui` is a static bundle: agents may change declarative view data it renders, never its code. `kanbei-consent` serves from a separate origin inside the VM behind the same verified human identity and holds a host-only `SameSite=Strict` session with no shared DOM or storage.

## 3. Sandbox and writable roots

### 3.1 Sandbox specification

```ts
type AbsolutePath = string;
type EnforcementClass = "monty_wasm" | "process_cgroup" | "none";

type SandboxMount = {
  path: AbsolutePath;
  access: "ro" | "rw";
  class: "image" | "inbox" | "vault" | "todo" | "scratch" | "agent_state" | "mutable_code" | "runtime";
};

type ChannelId = "inbox" | "vault" | "todo" | "chat" | "csr" | "search";

type SandboxSpec = {
  sandboxId: SandboxId;
  agentId: AgentId;
  loopId: LoopId;
  generation: NixGenerationId;
  enforcementRequired: EnforcementClass;
  mounts: SandboxMount[];
  writableRoots: AbsolutePath[];
  channels: {
    allowed: ChannelId[];        // the only host-call surface; no network member exists
    network: "absent";           // the VM has no NIC; this is not configurable
    dns: "absent";
    rawSockets: "impossible";
  };
  limits: {
    wallMs: number;
    memoryBytes: number;
    pidsMax: number;
    cpuQuota: number;
    openFiles: number;
    todoBytes: number;           // per-write and per-mount cap
  };
  capabilitySockets: {
    channels: AbsolutePath[];    // one socket per allowed channel
    supervisor: AbsolutePath;
  };
  credentialAccess: "none";
};
```

### 3.2 Mount classes

| Class | Default access | Contents | Backup |
|---|---|---|---|
| `image` | `ro` | `/nix/store`, static UI bundle, model weights, fixtures | part of the image |
| `inbox` | `ro` | virtiofs of the age-encrypted inbox directory; batches and the Syncthing-encrypted phone folder | no; source of truth is the host writer |
| `vault` | `ro` | virtiofs of `/var/lib/data/vault` | yes, via the vault's own Syncthing backup |
| `todo` | `rw` capped | virtiofs of `/var/lib/data/todo` mounted on a size-capped filesystem | yes, via the vault Syncthing set |
| `scratch` | `rw` | `/var/lib/kanbei/scratch/<sandboxId>/`, unique per sandbox | no; deleted on termination |
| `agent_state` | `rw` | `/var/lib/kanbei/agents/<agentId>/` | yes, in the image |
| `mutable_code` | `ro` except own candidate | `/var/lib/kanbei/mutable/`; agents write candidates, never the active pointer | yes, in the image |
| `runtime` | `ro` socket access | supervisor socket and the allowed channel sockets only | no |

The Todo mount is the only read-write human data path. It is a size-capped filesystem: the write fails at the cap rather than truncating, and every write is receipted with path, digest, and bytes. `tuxedo` outside refuses symlinks; the same rule holds inside the mount handling.

The active mutable-code pointer is core-owned and read-only to agents:

```
/var/lib/kanbei/mutable/candidates/<scopeId>/<candidateId>/
/var/lib/kanbei/mutable/scopes/<scopeId>/current -> ../candidates/<scopeId>/<candidateId>
```

The canonical ledger, image content that carries trust-root code, frozen-root control files, other agents' state, and the audit sink are **not mounted** into the sandbox. Channel sockets are mounted only as socket paths.

### 3.3 Enforcement and start verification

The supervisor records the actual `EnforcementClass` and compares it with `enforcementRequired`. If the actual class is weaker, the sandbox does not start; the refusal is an effect receipt with `sandbox_posture_failed`.

Verification after start, before handing control to agent code:

- the mount namespace exists and contains exactly the admitted mounts, with `inbox` and `vault` read-only and `todo` capped;
- the cgroup exists with the requested memory, pids, CPU, and open-file limits;
- exactly the allowed channel sockets are reachable and no other capability socket is;
- the process cannot see forbidden paths by DAC or Landlock;
- the sandbox has no network interface (`ip link` shows only `lo`), no resolver configuration, and no reachable host path outside the mounts;
- the sandbox's `startToken` is registered with the supervisor.

If any check fails, kill the cgroup and emit `sandbox_posture_failed`. Do not run with a claimed stronger posture.

Known limits carry into the contract:

- Landlock network rules are port-only and are not used as the boundary; the boundary is the absence of a NIC.
- Landlock does not revoke file descriptors opened before enforcement.
- A virtiofs share that is not in the manifest is a channel leak even if the guest never mounts it.
- Monty bounds one program; the VM process tree is not per-agent isolated.

### 3.4 Inter-agent fault isolation

One sandbox instance exists per loop execution attempt, not one per long-lived agent concept. The supervisor owns the instance cgroup, the process group, and termination.

Invariants:

- an agent crash, hang, OOM, or fork bomb cannot stop `kanbei-core`, the channel hosts, the supervisor, `llama.cpp`, or the UI;
- each sandbox has separate memory, pids, CPU, scratch, Todo-write, and wall-clock limits;
- termination is `SIGTERM`, grace period, then cgroup `SIGKILL`;
- no scratch directory is shared between sandboxes;
- supervisor restart reaps all surviving sandboxes and re-launches only from ledger checkpoints;
- a hung sandbox is killed by the supervisor's deadline, not by a cooperative agent callback.

Mechanism is fixed: Monty for agent-authored programs on a cgroup-contained process tree. A container per agent is excluded. The mechanism improves fault isolation and MUST NOT be described as a security boundary for canonical state or approvals.

### 3.5 Monty isolation

Agent-authored programs run under Monty, the Python-subset interpreter with an in-process WASM backend. The spike evidence recorded on `smores56/kanbei`:

- the WASM backend starts on Deno 2.9.2 and executes programs;
- async host calls are mediated: the program sees only the generated stubs, and a stub call is an effect receipt;
- ambient filesystem access raises `PermissionError`; `urllib` and `subprocess` imports fail with `ModuleNotFoundError`; zero network requests occur;
- a `while True` program is bounded and reported as `MontyCrashedError`; an unbounded allocation is bounded and reported as `MemoryError`; a fresh session afterwards succeeds;
- a child timeout is reported as an explicit error and never claims a bounded allocation.

Monty is a language-level boundary and a crash/resource boundary. It is not the security boundary: an engine compromise gives the agent the VM, and a process-wide OOM can take the VM's other components with it. Both are recorded residuals (§9), and both are contained by the VM boundary.

Host calls are generated from the channel manifest. A stub that is not in the manifest does not exist. Monty cannot construct one, cannot reach a socket, and cannot read the canonical ledger.

## 4. Channel policy

The channel set is closed. Adding a channel, port, or share is an image change through a human-merged commit, a Nix generation, and a new deployment — not a runtime configuration.

| Channel | Transport | Direction | Content | Enforcement |
|---|---|---|---|---|
| Inbox | virtiofs, read-only | in | age-encrypted source batches; Syncthing-encrypted phone folder | host writer owns the directory; VM cannot write it; reader decrypts in-VM and records batch, source, sequence, and coverage |
| Vault | virtiofs, read-only | in | `/var/lib/data/vault` | read-only mount; symlink refusal; content is observations only |
| Todo | virtiofs, size-capped rw | both | `/var/lib/data/todo/todo.txt` | per-write and per-mount byte cap; write receipts; no symlinks; human-only at rest |
| Chat | vsock, in-VM TLS | both | JSON API | TLS terminates in the VM; host relay is a TCP pipe under its own UID; JSON schema per route |
| CSR | vsock, schema-locked | out | fixed-CN CSR for ACME renewal; certificate returns | one CN, one key algorithm, one SAN set; any other field rejects |
| Search | vsock, schema-capped | out/in | capped query JSON, stripped text back | length cap, rate limit, daily budget, DLP, judge; the key lives in the Search VM |

Rules:

1. **Inbox is one-way.** The host writer drops age-encrypted batches into a root-owned directory the VM mounts read-only. A batch is immutable once written. The reader records the batch digest, source id, sequence, produced time, and coverage before any observation becomes visible. Re-reading a batch is idempotent by digest.
2. **Vault is read-only and is not a channel to write back.** Kanbei may cite vault content and may propose todo items; it cannot modify the vault.
3. **Todo is capped.** `todoBytes` is a hard ceiling in §5. A write beyond the cap fails with `todo_cap_exceeded` and appends a denial receipt. The Todo mount is never used for agent state, scratch, or bulk data.
4. **Chat is a JSON API over in-VM TLS.** The host relay forwards bytes and holds no plaintext. It is human-owned, runs under its own UID, and is separate from the cluster tunnel. Only the API routes in the manifest exist; there is no static file serving, no proxy route, and no upgrade to arbitrary bytes.
5. **CSR is schema-locked.** The request carries one fixed common name for `kanbei.sammohr.dev`, one key algorithm, and one SAN set. Anything else is refused before the CA is asked. The returned certificate and chain are written to the image-independent state path and recorded as a kernel event.
6. **Search is automatic but capped.** Each query is length-capped (~100 characters), rate-limited, budgeted daily, and scanned by DLP regexes (digit runs, emails, base64/hex blobs, high-entropy tokens). A Granite Guardian model on CPU scores the query against a smuggled-data criterion; any failure blocks the query. Only stripped text returns. Every query is logged to a root-only host log the owner reads through a touch.
7. **Search results are hostile input.** They become observations, never authority, and are subject to the normal poisoning path.
8. **Failure is closed.** A missing socket, an undecryptable batch, a schema violation, a cap breach, or a judge error denies the operation and records it. There is no fallback channel.

Chat is not an ingress path for anything but the owner: the host relay accepts only the tunnel that carries the WARP private route, and the in-VM API requires the Access identity plus the in-VM passkey.

## 5. Hard global ceilings versus discretionary approvals

### 5.1 Boundary classes

| Boundary | Owner | Enforcement point | Agent override | Approval override |
|---|---|---|---|---|
| Writable roots and mounts | human | supervisor / mount manifest | no | no |
| Channel set | human | image manifest and supervisor | no | no |
| GPU lanes and model pool | human | frozen lane governor | no | no |
| Ingest volume per source | human | host writer quota and reader admission | no | no |
| CAS and state growth | human | frozen admission and ceiling governor | no | no |
| Todo bytes | human | mount cap and Todo writer | no | no |
| Chat rate and Search budget | human | channel host | no | no |
| Canonical append | frozen kernel | append API | no | no |
| Audit sink | frozen kernel | append/audit path | no | no |
| Activation and rollback | frozen kernel | activation supervisor | no | no |
| Ingress | human | WARP route, relay, in-VM TLS, passkey | no | no |
| Autonomy tier | human | policy / decision queue | no self-grant | can tighten, not widen hard limits |
| Per-loop budget | soft, agent-owned | loop scheduler | yes, with rationale | n/a |
| Tool choice, plan interpretation | agent | loop | yes | n/a |

### 5.2 Hard ceiling schema and reservation state machine

```ts
type HardCeilings = {
  generation: NixGenerationId;
  window: "rolling_1h" | "calendar_day" | "calendar_month";
  gpuLanes: { chat: 1; background: 3 };        // 128k + 3x32k from one KV pool
  laneSeconds: number;                          // total per window
  ingestBytesPerSourcePerDay: number;
  ingestBatchesPerSourcePerHour: number;
  casBytesTotal: number;
  stateBytesTotal: number;
  todoBytes: number;
  chatMessagesPerHour: number;
  chatResponseBytes: number;
  searchQueriesPerDay: number;
  searchQueryChars: number;
  concurrentLoops: number;
};

type Reservation = {
  reservationId: ReservationId;
  loopId: LoopId;
  effectClass: "lane" | "ingest" | "search" | "chat" | "sandbox";
  amount: number;              // max expected units; lane-seconds, bytes, queries
  state: "held" | "committed" | "released" | "expired";
  createdAt: UtcUs;
  expiresAt: UtcUs;
};
```

Invariants:

- `sum(held + committed)` never exceeds the configured ceiling.
- A model call reserves lane-seconds before the call; the response commits actual usage and releases the remainder.
- An interrupted effect with unknown usage leaves the reservation `held` until the audit path reconciles it or `expiresAt` passes.
- A search query reserves one query unit and its character budget before the Search channel call.
- An inbox batch reserves its bytes and batch count before the reader decrypts it; an over-quota batch is left in place and alerted, never partially applied.
- CAS and state admission reserve expected bytes before a blob write or a record append; exhaustion produces `ceiling_blocked` and suspends the producing loop.
- A concurrency ceiling queues by loop priority and deadline; no busy waiting.
- Human changes only by a new image generation. Agents cannot read or modify the ceiling object.

Concrete values are mandatory operator configuration but remain deferred to burst measurement on the deployed VM: `[SPIKE: LIM-1]`.

### 5.3 Approvals do not override hard limits

An approval binds to an interpretation/plan hash, subject, scope, constraints, and expiry. If the approved action would exceed a hard ceiling, violate a writable root, use a non-manifest channel, or lack audit, it fails. The agent may request a human configuration change; it may not reinterpret the ceiling as a soft budget.

Approval constraints can be overridden by the agent with a recorded rationale, because they express delegated intent, not security. Hard limits never use that override path. Candidate approval (§6.4) is itself an approval receipt and never widens a hard limit.

### 5.4 Failure behavior

| Condition | Behavior |
|---|---|
| Ceiling governor unavailable | fail closed for new lane/search/ingest/sandbox effects; record `ceiling_unavailable` |
| Ceiling exhausted | `ceiling_blocked`; suspend loop; alert; no new calls |
| Reservation stuck | expiry releases; audit reconciler resolves actual usage |
| Soft budget exhausted | `soft_budget_exceeded`; agent may append an override with rationale; hard ceiling still checked |
| Approval expired | new approval required for the bound plan hash |
| Todo cap exceeded | `todo_cap_exceeded`; write denied; no partial write |

## 6. Channel mediation and where the broker went

The pre-split design put a credential-injecting broker between agent code and provider APIs. The split removes both the credential and the API from Kanbei's VM, so the broker has no work left inside Kanbei. Surviving mediation moved to the boundary where the credential already lives.

### 6.1 No broker in Kanbei

- The Kanbei VM has no HTTP client, no provider endpoint list, and no credential store.
- The model is local; the only "provider" call is a `llama.cpp` request on a lane.
- The only remote key (TinyFish) is in the Search VM. The Search channel client in Kanbei never sees the key, never sees the endpoint, and cannot vary the request shape beyond the schema.
- Ingest credentials exist only inside their own ingest VMs, arrive on a read-only block device, and are never written to a batch.

There is no enrollment record, no logical hostname mapping, no redirect policy, and no response redaction in Kanbei, because there is no credential to inject. `credential_enrollment` is removed from the schema.

### 6.2 Ingest VMs

Each source VM holds only its own source's credentials, parses that source's hostile input first, and emits one age-encrypted batch per scan to the host writer over vsock. Egress is that VM's allowlist through the host proxy and resolver. A credential never leaves its VM, never appears in a batch, and is never readable by Kanbei.

The host writer validates batch shape, writes it atomically with an immutable name, and drops it into the inbox directory. It never decrypts. A write that fails quota is refused and alerted.

### 6.3 Search VM

The Search VM holds the TinyFish key, applies DLP and the judge before the call, applies provider rate limits as backpressure, strips the response to text, logs every query to a root-only log, and returns over vsock. The Kanbei-side search client is a schema-capped client with no credential and no URL control: it sends a query string and a limit, and receives stripped text plus provenance fields.

### 6.4 Candidate approval is the remaining human gate

The one pre-split broker-like control that survives is approval. Executable candidates require a human approval receipt that binds the candidate digest, capability diff, scope, and expiry before the activation supervisor flips a pointer. Approval is not a credential path: it authorizes activation of code that is already in the image-independent candidate store and that cannot widen a hard limit.

## 7. Effect audit

### 7.1 Records

```ts
type EffectAttempt = {
  effectId: EffectId;
  requestId: RequestId;          // minted by the channel host or supervisor, not the agent
  kind: "inbox_batch" | "vault_read" | "todo_write" | "chat_request"
      | "csr_request" | "search_query" | "model_call" | "process_spawn"
      | "activation" | "ceiling";
  actor: ProcessIdentity;
  loopId?: LoopId;
  subject: string;               // batch digest, path, route, lane, candidate
  startedAt: UtcUs;
  idempotencyKey?: string;
  requestDigest: Sha256;
  redactionIntent: "full" | "partial" | "none";
};
```

The receipt envelope is the single canonical `EffectReceiptRecord` of §4.7.5, appended through the frozen append API and participating in the hash chain.

### 7.2 Coverage

| Effect | Audited | Notes |
|---|---|---|
| Inbox batch read | yes | batch digest, source, sequence, byte count, coverage |
| Vault read | yes | path class, digest, bytes; content citation is the observation |
| Todo write | yes | path, digest, bytes, cap remaining |
| Chat request/response | yes | route, request digest, response digest, byte count |
| CSR request | yes | CN, key algorithm, SAN set, issuance result |
| Search query/result | yes | query digest, length, DLP verdict, judge verdict, stripped result digest |
| Local model call | yes | lane, exact post-redaction request/response, usage, latency |
| Capability-mediated process spawn | yes | argv class, exit, duration |
| Activation/rollback | yes | candidate, capability diff, pointer flip, approval receipt |
| Ceiling block/reset | yes | ceiling generation and reason |
| Direct syscall inside the VM | no | cannot leave the VM; see §7.4 |
| Hidden model reasoning | no | not promised |

The audit gap shrinks materially compared with the pre-split design: a direct syscall has nowhere to go. The remaining unattributable surface is *content* — a Todo write or a Search query is attributable but its meaning is not.

### 7.3 Tamper evidence and sink failure

The audit sink is outside agent-writable roots and is append-only from the frozen process's perspective. If the sink cannot record an audited effect, the effect is denied. A missing receipt is a zero-tolerance failure, not a log gap.

### 7.4 Verification points

- An agent cannot mint or alter a `requestId` that a channel host accepts.
- A denied operation produces an attempt and a denial receipt, not only a log line.
- A channel host crash leaves `outcome_unknown`, not a fabricated success.
- Replaying a recorded effect never opens a socket or re-reads a batch for effect.
- The VM's interface list, resolver configuration, and mount table match the manifest at start and on every watchdog probe.

## 8. Deployment

### 8.1 Deployment invariants

| ID | Invariant |
|---|---|
| D-1 | The image, including the frozen trust root, static UI, and model weights, is Nix-built from the flake-pinned `smores56/kanbei` commit and immutable at runtime. |
| D-2 | Mutable code lives under `/var/lib/kanbei/` and is activated by pointer flip; executable candidates require a human approval receipt. |
| D-3 | The VM holds no secret material. Host-side artifacts (images, swtpm state, relay, vsock sockets, ingest secrets, `/dev/vfio`, logs, consoles) are root-only. |
| D-4 | The audit sink and canonical ledger are not agent-writable. |
| D-5 | The watchdog has an in-VM probe and a host-side freshness and liveness probe outside the VM; neither can be disabled by mutable code. |
| D-6 | The UI binds loopback inside the VM and is reached only through the host relay and the WARP private hostname route. |
| D-7 | Backup is an encrypted image snapshot; restore verifies the ledger checkpoint, every CAS reference, and the derived-index rebuild, and unlocks with the vTPM or the offline recovery key. |
| D-8 | The kernel generation is recorded as a `kernel_generation` receipt by the frozen startup verifier; host/image drift is alerted. |
| D-9 | The guest disk is LUKS sealed to a per-VM swtpm vTPM; the seal's binding is recorded by `[SPIKE: LUKS-1]` and the recovery key is offline. |

### 8.2 Deployment plane (decided)

The plane is fixed. Kanbei is one qemu microVM under microvm.nix, 8 GB RAM, no NIC, with the 3090 passed through by VFIO. The host runs no NVIDIA driver; the 3090 binds to `vfio-pci` at boot. Guest state is on the in-guest LUKS volume; the host holds the image, the swtpm state, the relay, and the vsock sockets as root-only artifacts.

The pre-split deployment alternatives (pure user-space unit, NixOS-plane unit, dedicated system units, hybrid helper) and the `[SPIKE: DEP-1]` broker-identity matrix are retired: there is no broker, no host-side agent process, and no host-side mutable Kanbei code to isolate from. See §8.7 for the retirement record.

### 8.3 Code entry and trusted updates

- Code enters only through a human-merged `smores56/kanbei` commit pinned in nix-config's `flake.lock`. The image build fails if the pin is missing, unpinned, or dirty.
- Agents may open pull requests against the repository. Development worktrees are never mounted into the VM and no build tool runs inside it.
- No automatic upgrade exists. Deployment is a human `nixos-rebuild switch` on the host that rebuilds the image and restarts the VM.
- Rollback is a host generation rollback to a previous pinned commit. Roll back only when existing data remains readable by the older kernel; otherwise roll forward or branch from a verified checkpoint. No destructive automatic down migration.
- Startup records the pinned commit, the image digest, and the Nix generation as a `kernel_generation` receipt, and verifies that core, channel hosts, the supervisor, and `llama.cpp` run from the image.
- The watchdog alerts on drift between the flake pin, the running image digest, and the recorded generation.

### 8.4 Ingress

- The static UI binds `127.0.0.1:<port>` inside the VM. The host relay forwards the API and serves nothing else.
- `kanbei.sammohr.dev` is a Cloudflare private hostname route reachable only from devices running the WARP client. Gateway TLS decryption stays off and the Cloudflare root CA is never installed, so Cloudflare carries ciphertext.
- TLS terminates in the VM over the Chat channel. The host relay is a TCP pipe under its own UID and sees no plaintext.
- The host tunnel is human-owned in nix and separate from the cluster tunnel; `cluster` cannot reach the relay.
- Kanbei authenticates with the Access identity and posture plus an in-VM passkey. The spike confirms Access still gates the route without TLS decryption: `[SPIKE: ING-1]`.
- Authority endpoints require the verified Access identity plus the passkey even on loopback. A request that appears on loopback is not human consent.
- Approval evidence and rulings are rendered and submitted only by the frozen consent surface and its gesture endpoint; the static shell may deep-link to it and cannot supply evidence, scope text, or `consentDigest`.
- A CAA record with `accounturi` limits who can issue certificates for the domain. The CSR channel is the only renewal path.

### 8.5 Watchdog, freshness, and alerting

`kanbei-watchdog` runs inside the VM and cannot be disabled by mutable code. It probes:

- core append API and sequencer liveness;
- channel host availability, schema conformance, and audit-sink writability;
- supervisor liveness and active-sandbox count;
- `llama.cpp` lane availability and KV-pool health;
- UI loopback response and CSP header presence;
- Todo mount headroom and CAS/state growth against ceilings;
- the last successful image snapshot, reported to the host.

The host-side counterpart probes VM liveness, the relay, the vsock sockets, the swtpm, and source freshness. Freshness is per source: last batch sequence, batch time, and coverage. A source past its window is labelled stale in the UI and raises a host alert. Staleness is never silent.

Watchdog behavior: restart the failed component with bounded start limits; kill orphaned sandboxes and record receipts; suppress duplicate alerts; include component, generation, last committed sequence, and error class; never include content or unredacted query text in alerts.

### 8.6 Image backup and restore

1. The host takes an online SQLite snapshot through the core's backup endpoint and verifies its checkpoint.
2. The guest disk image is frozen (the VM is briefly stopped or the filesystem is quiesced) and snapshotted.
3. The frozen image ciphertext is copied to `/var/backup` and to R2 by host restic. swtpm state is excluded.
4. The host records the image digest, the ledger checkpoint digest, and the snapshot time in the backup manifest.
5. Restore reimages the VM, unlocks with the vTPM or the recovery key, verifies every ledger reference and blob digest, and rebuilds derived indexes before serving reads.
6. A snapshot whose checkpoint digest does not match is refused; a backup older than its threshold is alerted.

The recovery key is shown once at first boot and held offline by the owner. It is never stored in the image, in state, or in the repository.

### 8.7 Retired spikes and their replacements

| Retired | Why | Replacement |
|---|---|---|
| `DEP-1` deployment plane and broker identity | no broker, no host-side agent process | fixed VM plane in §8.2; host artifacts root-only |
| `ISO-1` lightweight inter-agent isolation | Monty plus the VM contains agents; no OS-per-agent claim | Monty isolation evidence §3.5; VM boundary |
| `LANG-1` agent-facing language bake-off | one fixed runtime; no provider choice exists | Monty fixed; recorded in `07 §4.9.5` |
| `NODE-DENO-1` TypeScript runtime | decided by the runtime spike | Deno 2.9.2 |
| Earlier host-security matrix (userns, Landlock ABI, cgroup delegation) | the host runs no agent and holds no credential reachable from the VM | VM channel manifest checks §3.3, host artifact permissions §8.1 |
| `BAK-1` backup hook wiring | backup is an image snapshot plus restic | §8.6; `[SPIKE: IMG-1]` covers only quiesce mechanics |

## 9. Residuals introduced by this section

1. **Engine compromise.** A Monty or supervisor compromise yields the VM: the ledger, the mounts, and the channels are reachable. Containment is the VM boundary.
2. **Process-wide OOM.** A memory bug in the Monty host can take other in-VM components with it; the host watchdog restarts the VM and recovery is ledger-based.
3. **Search leakage.** Capped, filtered, judged queries still leak a few dozen bits per query to TinyFish.
4. **Todo reach.** A compromised Kanbei can write arbitrary text up to the cap into the owner's todo list; the owner sees it and the vault-side diff remains the audit.
5. **Chat reach.** A compromised Kanbei can send the owner arbitrary text; the CSP shell prevents it from making the browser fetch anything.
6. **Todo content attribution.** Writes are receipted; their human meaning is not machine-verified.
7. **vTPM seal.** Under direct kernel boot the seal may bind to no measured state; the recovery key is the durable path.
8. **In-VM same-UID inspection.** Processes inside the VM share a UID space; isolation between them is containment, not secrecy.
