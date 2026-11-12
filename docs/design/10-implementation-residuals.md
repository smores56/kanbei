# Implementation residuals (T22 hardening pass)

These are the gaps this pass did not close. Each is deliberate and bounded; none
is a silent fallback.

## Chat TLS listener

The deployed contract terminates TLS in the guest and consumes
`KANBEI-AUTH 1 <token>\n` **before** the TLS handshake
(`nix-config modules/lib/kanbei/ui/server.py`, `relay_auth.verify`). Deno 2.9 has
no server-side STARTTLS: `Deno.startTls` is client-only and `Deno.listenTls`
performs the handshake on accept. A Deno listener therefore cannot replay the
host's pre-TLS auth line without hand-rolling TLS.

What exists instead:

- `src/channels/chat/http.ts` mirrors the authority exactly: `GET /api/health`,
  `POST /api/chat {message}`, bearer passkey session, `application/json` only,
  64 KiB body, 8000-character message, the host's status codes and
  `{"error": ...}` bodies, and the `{reply, lane}` success shape.
- `src/adapters/chat-http-client.ts` is an HTTP/TLS client for the deployed
  authority, so a Deno caller never speaks the incompatible frame API.
- The length-prefixed frame API remains only for spike/property tests and is
  marked `EXPERIMENTAL` in every file.

Residual: a Deno process cannot replace the Python TLS listener until Deno
exposes server-side STARTTLS (or the relay moves the token into TLS). The
deployed Python listener remains the authority.

## CSR chain signature

The client validates what the host parser validates: fixed CN, fixed SAN, the
CSR's exact SPKI, and a non-expired leaf. It does not verify the chain's
signatures; the host ACME responder is the trust root and the chain crosses a
root-only vsock channel. Residual: no independent signature check.

## File lock

Deno 2.9 exposes no `flock`. `src/adapters/ledger-file.ts` uses an exclusive
create (`O_EXCL`) lock file, which stops a second writer in the same VM. It is
not an OS advisory lock and does not coordinate NFS or multiple kernels.
Residual: single-writer per VM only, matching the deployment.

## Total CAS accounting

`ContentStore` enforces per-object and total caps. The total is read from the
sink's `usage` before each publish; two concurrent `publish` calls can both pass
the check and briefly exceed the cap. Residual: worst case is one object over
the ceiling, never unbounded growth.

## Inbox rich-manifest compatibility

The reader accepts both the host envelope `{v,source,kind,seq,sent_at,items}`
and the sealed rich manifest (`envelopeVersion`/`manifest`/`blobs`). The host
envelope is converted to a manifest whose `encryption.recipient` is absent
because the writer does not put it on the wire. Residual: the derived manifest
is not byte-identical to a host-side manifest (the host writes only the
envelope).

## Stale lock recovery

The exclusive-create ledger lock is not stale-aware: a process killed between
lock creation and release leaves `<ledger>.lock` behind, and the next boot
fails closed rather than risking two writers. Operator action is to remove that
file after confirming no process holds the ledger. This is the safe direction
for a canonical store.

## Journey J2-CSP

The step renders a planted document to inert `text/plain` and asserts the shipped
policy bytes (`default-src 'none'`, no remote origin), but the browser that
enforces the policy is the host-owned static shell in nix-config. The step also
asserts statically that `src/ui/render.ts` has no fetch capability, and records
this limit in the envelope's `acceptedResiduals`.

## Journey J2-SEARCH

Searchd's judge and DLP run in the Search VM and are not in this repo. The step
drives the real `SearchClient` seam with a synthetic `SearchChannel` and asserts
the client refuses a blocked verdict, strips markup, refuses forged fields, and
enforces the persisted query ceiling. The judge itself is recorded as an
accepted residual on the envelope.

## Search budget store

Admit stamps live in one file with an atomic replace and one in-process permit.
A second writer on the same file would race; the deployment has one VM and one
kernel, matching the ledger's single-writer assumption.

## Vault realpath window

The adapter resolves the real path, requires it under the real root, opens that
path, and re-checks the opened inode against an lstat. A same-inode replacement
between realpath and open is not observable, and Deno 2.9 exposes no
`O_NOFOLLOW` per component; the lstat chain plus realpath is the closest the
runtime allows.

## Redteam evidence claims

Each redteam case declares its denial evidence (`receipt`, `quarantine`, or
`none`). Quarantine rows and receipts are read from the seam; cases that refuse
before any record exists claim `none` and fail the run if they report one.
