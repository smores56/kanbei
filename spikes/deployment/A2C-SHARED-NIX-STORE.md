# A2c — single shared private Nix store

Status: **all six checks pass — one shared store serves build, shell, run, and profile.** No gate decision.

## Design

- One host-side store root, mounted at `/nix/store` inside every sandbox.
- Shared `nix/var` mounted at `/nix/var`; one DB and GC root set.
- Host `/nix/store` is not mounted.
- Bootstrap: `nix`, `bash`, `coreutils`, Deno, NixOS `/etc`, and the CA bundle are seeded into the shared store.
- Sandbox: rootless user/PID/mount namespaces, chroot, Landlock, private `/tmp`, shared store read/write.

## Result

Published record: `spikes/deployment/nix-shared-result.json`.

| Check                                                   | Result                                |
| ------------------------------------------------------- | ------------------------------------- |
| `nix build nixpkgs#hello`                               | pass                                  |
| Direct execution of built `hello`                       | pass, `Hello, world!`                 |
| `nix shell nixpkgs#hello --command <output>/bin/hello`  | pass, `Hello, world!`                 |
| `nix run nixpkgs#hello`                                 | pass, `Hello, world!`                 |
| `nix profile add --profile /scratch/prof nixpkgs#hello` | pass                                  |
| `nix profile list --profile /scratch/prof --json`       | pass, one `hello` entry               |
| Execute `/scratch/prof/bin/hello`                       | pass, `Hello, world!`                 |
| Output present in shared store                          | pass                                  |
| Host `/nix/store` contains output                       | no                                    |
| Store path                                              | `tmp/kanbei-shared-nix` in this probe |

Verdict is `pass` and is derived from the whole check record: every check must hold for it to pass, so a single red check cannot be masked.

## Why `nix shell` and `nix run` failed before

Nix re-anchors its own root after a mount-namespace round-trip: it opens `/proc/self/root` and then `chroot(".")`. Landlock resolves that magic link to the sandbox root `/`, and the ruleset had no rule beneath `/`, so the open returned `EACCES`; Nix then skipped the chroot and later failed with `error: unable to execute '<output>/bin/hello': No such file or directory` on a path that exists. Adding `{ path: "/", access: "read" }` fixes it.

Measured control: with the identical configuration and only that rule removed, `nix shell` and `nix run` reproduce the `ENOENT` while direct execution still succeeds.

## Nix configuration

The probe no longer leans on implicit defaults:

- `NIX_CONF_DIR=/scratch/nix-conf` with a generated `nix.conf` containing `store = local`, `build-users-group =`, `sandbox = false`, and `experimental-features = nix-command flakes`.
- `NIX_STATE_DIR=/nix/var/nix`.
- `/etc/nix/nix.conf` inside the sandbox is a dangling symlink into the host store, so it is bypassed rather than read.

Two facts about Nix 2.34.8 that shaped that file:

- `store` takes a store URI, not a directory. `store = /nix/store` parses as a chroot store rooted at `/nix/store`, which then tries `mount '/nix/store/nix/store' on '/nix/store'` and fails with `Operation not permitted`. `store = local` expresses the intended default local store.
- There is no `state` or `state-dir` setting (both warn `unknown setting`); the state directory is only reachable through `NIX_STATE_DIR` or the compiled-in default.

## Bootstrap notes

- NixOS `/etc` is a symlink farm into the Nix store. `/etc/static`, `/etc/ssl`, and the CA bundle must be seeded into the shared store or DNS/TLS fail.
- `/run/systemd/resolve` must be visible for the stub resolver.
- Deno is present in the host store but not registered in the host Nix DB; it and its `ldd` dependencies are copied directly.
- `build-users-group =` avoids chown to the unmapped `nixbld` group inside the user namespace.
- `sandbox = false` in Nix is acceptable only because the outer rootless sandbox is the boundary.

## Residuals

- `nix develop` is expected-unsupported. Measured: `error: cannot open '/dev/null': No such file or directory`, reported while reading the build-hook response, so no dev shell is created. It needs `/dev`, `/dev/pts`, and `/bin` inside the sandbox; mounting host device files does not help, because Landlock follows the source inodes and bind-mounted host devices stay denied. No device files are mounted.
- The root read rule grants read/execute over the whole filesystem, because Landlock can only grant beneath an anchored path and Nix needs the anchor `/`. Writes remain restricted to the ruleset's writable paths; reads are not a boundary in this probe.
- The probe store is under the worktree `tmp/`; production must use a local filesystem that supports Nix ownership and a quota/GC policy.
- All sandboxes share one mutable store; a compromised sandbox can poison another sandbox’s tools or fill the store.
- The store contains host `/etc` data and package closures; no secrets may be placed there.
- `io_uring` and the kernel remain in the attack surface.
- Bootstrap is host-specific: exact `/etc`, CA, Deno, and core tool closures must be refreshed on system changes.
- `nix profile` writes the profile under `/scratch`; a real deployment needs a durable per-sandbox profile location and a GC policy for the shared store.
