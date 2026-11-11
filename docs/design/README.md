# Kanbei design documents

This directory is the rebased design set for the split deployment described in SDLC
#509: a NIC-less Kanbei VM, offline `llama.cpp` inference, six closed channels, a static
CSP shell, and human-approved executable self-modification.

## Carried documents

| File | Role |
|---|---|
| `00-requirements.md` | Requirements baseline. Product target, authority, records, memory, intent/approval/work, loops, replay, self-modification, security, tools/providers, interface, persistence/freshness/backup, validation. |
| `04-security-deployment.md` | Normative threat model, security invariants (S-1–S-17), process identities, sandbox and mount classes, Monty isolation, channel policy, hard ceilings, effect audit, deployment (LUKS/swtpm, flake pin, WARP/in-VM TLS, freshness, image backup). |
| `07-technical-specification.md` | Full technical specification. Section 7 is the sole normative validation authority: gates, invariants, scenarios, spikes, journey, traceability. |
| `08-implementation-plan.md` | Implementation plan: task completion contract, decision gates, traceability, phases A–I, and the registry mapping. |
| `09-sdlc-issue.md` | Registry-facing summary with the artifact hash binding. |

## Provenance and what the rebase changed

Source: `~/wip/kanbei-design/` (`spec/`, `tasks/`, `review/`), the pre-split design.
Rebased onto #509 and the deployed implementation on `smores56/kanbei`.

Carried and rebased:

- `06-validation-plan.md` is **deleted**. It was a superseded early draft and is a
  source for nothing; `07 §7` is the only validation authority.
- `01-state-memory.md`, `02-agent-execution.md`, `03-plugin-runtime.md`, and
  `05-interface-ingestion.md` are **absorbed into `07`** and not carried separately, so
  there is one normative copy of each contract.
- `tasks/` packets A0–I5 are superseded by `08` plus the registry tasks under #509.

Substantive changes:

1. **Product and journey.** Managed LLM/search APIs and unrestricted network are gone.
   The target is offline life admin (ingest, reconcile, advise, maintain todos) with a
   sealed `journey-v2` fixture. Records, canonicalization, memory, approvals, loops, and
   replay keep their pre-split semantics.
2. **Trust root.** No broker. The trust root is the append path and sequencer, the six
   channel hosts and their code, the Monty host boundary and supervisor, the ceiling
   governor, receipt verification, the activation supervisor, the audit sink, the
   startup verifier, and the frozen consent surface. The VM and its channels are the
   security boundary; Monty is a language-level boundary inside it.
3. **Channels.** Exactly six, closed by manifest: Inbox (ro age batches), Vault (ro),
   Todo (rw capped), Chat (vsock, TLS in the VM), CSR (vsock, schema-locked), Search
   (vsock, capped). Channel leak, CSP no-request, Todo cap, and stale heartbeat are new
   zero-tolerance gates (`ZT-15`–`ZT-18`).
4. **Self-modification split.** Declarative lenses, views, themes, and settings activate
   live at the next turn. Executable tools, plugins, and projections require a human
   approval receipt binding the candidate digest, capability diff, scope, and expiry.
5. **State and deployment.** State lives on the in-guest LUKS volume under
   `/var/lib/kanbei/`; the image is Nix-built from the flake-pinned commit; backup is an
   encrypted image snapshot; ingress is a WARP private route with edge TLS decryption
   off and TLS terminating in the VM.
6. **Ingest.** The adapter contract becomes an ingest batch contract: per-source VMs
   hold credentials, parse hostile input, and write immutable age-encrypted batches to a
   host writer. Kanbei reads them read-only, records coverage and a freshness heartbeat,
   and treats Git as material (no repository is writable from the VM).
7. **Decisions recorded, not deferred.** Monty (agent code) and Deno 2.9.2 (control
   plane) are fixed; `DG-1`, `DG-2`, and `DG-4` are closed and `DG-3` is retired. The
   model-dependent language bake-off is retired because the deployment has one fixed
   local model.
8. **Naming.** The ledger table `inbox_item` is renamed `agent_inbox_item` so the agent
   message queue is never confused with the ingest Inbox channel.

## Reading rules

- Conflicts resolve in favor of `00-requirements.md`, then `07`, then `04` for its own
  section, then `08`.
- Requirement anchors in `07 §7.6`, `08`, and `09` are section names, not line numbers.
- Validation identifiers (`ZT-*`, `I-*`, `V*`, `AS-*`, `C-*`, `R-*`, `P-*`, `B-*`,
  `U-*`, `S3`–`S8`) and every dataset are defined only in `07 §7`.
- `04`, `07`, and `08` are hash-bound from `09`. Editing one requires updating the
  binding in the same commit.
- `07 §7` and `04` are normative for the deployed system; `08` orders the work and the
  registry mapping in #509 tracks it.

## Not carried

- Any document that assumes a broker, managed model or search provider, unrestricted
  egress, live arbitrary plugin activation, or project-root mounts.
- `06-validation-plan.md`, `tasks/`, and the `review/` corpus; review findings that still
  apply are folded into the normative text and the `ZT-*` gates.
