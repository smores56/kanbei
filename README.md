# Kanbei

Kanbei is a self-hosted, single-user, always-thinking ledger agent. It runs offline in
one NIC-less microVM, owns the GPU, and reaches the world only through six closed
channels. The ledger is canonical; semantic memory, views, and disclosure are
rebuildable projections.

## Shape

- **Host**: root-only VM artifacts (image, swtpm state, relay, vsock sockets, ingest
  secrets, `/dev/vfio`, logs). The 3090 binds to `vfio-pci` at boot; the host loads no
  NVIDIA driver.
- **Guest**: qemu microVM, 8 GB, no NIC, 3090 by VFIO, in-guest LUKS sealed to a per-VM
  swtpm vTPM with an offline recovery key. `llama.cpp` serves the owner's model: one
  128k chat lane and three 32k background lanes from a shared q8_0 KV pool.
- **Channels** (the entire byte surface):

  | Channel | Transport               | Direction                               |
  | ------- | ----------------------- | --------------------------------------- |
  | Inbox   | virtiofs read-only      | in: age-encrypted source batches        |
  | Vault   | virtiofs read-only      | in: `/var/lib/data/vault`               |
  | Todo    | virtiofs size-capped rw | both: `/var/lib/data/todo/todo.txt`     |
  | Chat    | vsock, TLS in the VM    | both: JSON API behind the WARP route    |
  | CSR     | vsock, schema-locked    | out: fixed-CN CSR for ACME renewal      |
  | Search  | vsock, capped           | out/in: capped queries to the Search VM |

- **Ingress**: `kanbei.sammohr.dev` is a Cloudflare private hostname route reachable
  only from WARP clients. Edge TLS decryption is off and the Cloudflare root CA is never
  installed; a host relay under its own UID carries ciphertext and TLS terminates in
  the VM.
- **UI**: a static, human-owned bundle with a strict CSP, JSON API only, no remote
  resources, no auto-linking. Model text renders as text nodes.
- **Self-modification**: declarative lenses, view catalogs, themes, and settings go live
  at the next turn; executable tools, plugins, and projections require a human approval
  receipt before the activation supervisor flips a scope pointer.
- **Agent-authored code** runs under Monty, a Python-subset interpreter with an
  in-process WASM backend and no ambient filesystem, network, or subprocess access.

## The rule: code enters the VM only through a human-merged commit

Kanbei's code enters the VM only through a human-merged `smores56/kanbei` commit pinned
in nix-config's `flake.lock` and built by Nix into the VM image.

- Agents may open pull requests; a human merges.
- Nix pins the merged commit; the image build fails on a missing or dirty pin.
- The VM never builds, fetches, or installs. A lockfile change is a dependency diff and
  a new image.
- Development worktrees are never mounted into the VM.
- The frozen trust root (append path, channel hosts, supervisor, ceilings, receipt
  verification, activation supervisor, audit sink, startup verifier, consent surface)
  changes only by the same path.

## Requirements

- Deno 2.9.2
- Git

## Commands

```sh
deno task hooks:install   # enable the committed pre-commit gate
deno task fmt             # format
deno task lint            # deno lint + oxlint
deno task typecheck       # strict TypeScript check
deno task test            # test suite
deno task test:unit       # deterministic, unprivileged unit suite
deno task verify          # all merge-blocking checks
deno task verify:fix      # format and auto-fix, then verify
deno task fixtures:seal   # regenerate the sealed fixture manifest
deno task journey:seal    # reseal the journey-v2 age batches (changes digests)
deno task journey         # run journey-v2 and record docs/evidence/journey-v2.json
deno task journey:check   # rerun, assert identical digests, assert the record is current
deno task redteam         # run the channel-leak/CSP suite, record docs/evidence/redteam-v1.json
deno task redteam:check   # rerun, assert identical digests, assert the record is current
deno task evidence:dry-run
deno task evidence:determinism
deno task evidence:failure
```

The pre-commit hook and CI run `deno task verify`, which includes `journey:check` and
`redteam:check`. Recorded runs live in `docs/evidence/`.

## Packaging

`flake.nix` builds the Deno control plane as `packages.default` and runs
`deno task test:unit` in the derivation's check phase. Remote modules are vendored into
a fixed-output derivation, so the build, the tests, and the runtime use no network
(`--no-remote`).

```sh
nix build .#default
nix flake check
```

nix-config consumes this package from a pinned `git+ssh://` flake input and copies the
store path into the guest image; the exact input, `kanbeiVm.package` wiring, and the
private-repository auth caveat are in `docs/design/04-security-deployment.md` §8.3.1.

## Design

- `docs/design/00-requirements.md` — requirements baseline.
- `docs/design/04-security-deployment.md` — threat model, channels, sandbox, ceilings,
  deployment.
- `docs/design/07-technical-specification.md` — full specification and the sole
  normative validation authority (`§7`).
- `docs/design/08-implementation-plan.md` — plan, gates, and traceability.
- `docs/design/09-sdlc-issue.md` — registry-facing summary with artifact hashes.

`docs/design/README.md` records provenance and the rebase.

## Engineering contract

See `AGENTS.md` for the Effect-first coding rules, architecture boundaries, and
definition of done.
