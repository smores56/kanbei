# A2c — single shared private Nix store

Status: **build/direct execution pass; `nix shell` package environment blocked.** No gate decision.

## Design

- One host-side store root, mounted at `/nix/store` inside every sandbox.
- Shared `nix/var` mounted at `/nix/var`; one DB and GC root set.
- Host `/nix/store` is not mounted.
- Bootstrap: `nix`, `bash`, `coreutils`, Deno, NixOS `/etc`, and the CA bundle are seeded into the shared store.
- Sandbox: rootless user/PID/mount namespaces, chroot, Landlock, private `/tmp`, shared store read/write.

## Result

Published record: `spikes/deployment/nix-shared-result.json`.

| Check                                                  | Result                                   |
| ------------------------------------------------------ | ---------------------------------------- |
| `nix build nixpkgs#hello`                              | pass                                     |
| Output present in shared store                         | pass                                     |
| Direct execution of built `hello`                      | pass, `Hello, world!`                    |
| Host `/nix/store` contains output                      | no                                       |
| `nix shell nixpkgs#hello --command <output>/bin/hello` | **blocked**: `No such file or directory` |
| Store path                                             | `tmp/kanbei-shared-nix` in this probe    |

The build fetched `hello` and its closure into the shared store; the host store remained unchanged. `hello` runs directly from the shared store.

## Why `nix shell` still fails

`nix shell` resolves the command through its generated environment and fails even when given the absolute output path. Direct execution of the same path succeeds. This is an environment-resolution issue in the sandbox, not a store or build failure. `nix profile` remains untested and may share the problem.

## Bootstrap notes

- NixOS `/etc` is a symlink farm into the Nix store. `/etc/static`, `/etc/ssl`, and the CA bundle must be seeded into the shared store or DNS/TLS fail.
- `/run/systemd/resolve` must be visible for the stub resolver.
- Deno is present in the host store but not registered in the host Nix DB; it and its `ldd` dependencies are copied directly.
- `build-users-group =` avoids chown to the unmapped `nixbld` group inside the user namespace.
- `sandbox = false` in Nix is acceptable only because the outer rootless sandbox is the boundary.

## Residuals

- `nix shell` environment lookup is blocked; direct output execution works.
- `nix profile` is untested.
- The probe store is under the worktree `tmp/`; production must use a local filesystem that supports Nix ownership and a quota/GC policy.
- All sandboxes share one mutable store; a compromised sandbox can poison another sandbox’s tools or fill the store.
- The store contains host `/etc` data and package closures; no secrets may be placed there.
- `io_uring` and the kernel remain in the attack surface.
- Bootstrap is host-specific: exact `/etc`, CA, Deno, and core tool closures must be refreshed on system changes.

## Next fix

To support `nix shell` and `nix profile`, either:

1. run Nix through a host-side private-store service that returns a command environment, or
2. add a launcher that executes the built output path directly from the shared store, with the package’s `bin` directories added explicitly.

Direct execution already covers build-and-run; package install/profile is the remaining gap.
