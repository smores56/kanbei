# A2b — rootless sandbox with Landlock and namespaces

Status: **measurement record**, not a gate decision. `DG-3` remains open.

## Question

Can a rootless, same-UID sandbox replace the system-wide dedicated-user split for broker secrecy, process control, and systemd isolation?

## Mechanism tested

- Rootless user + PID + mount namespaces via `unshare -Urpf --mount-proc`.
- A mounted new root (`chroot`) containing only `/nix/store`, `/run/current-system`, `/etc`, `/proc`, the allowed project dir, scratch, the broker socket dir, and the denied test dir.
- Landlock ABI 7 applied before the agent code runs.
- Broker outside the sandbox, same UID, holding the credential file descriptor.
- Broker Unix socket bind-mounted at `/broker/broker.sock`.
- No host `/dev`, no `/run/user`, and no host systemd socket in the sandbox view.

## Result

Published record: `spikes/deployment/rootless-result.json`.

| Check                         | Result                              |
| ----------------------------- | ----------------------------------- |
| Allowed project read          | pass                                |
| Denied path read              | blocked with `EACCES`               |
| Allowed scratch write         | pass                                |
| Denied path write             | blocked                             |
| Broker `/proc/<pid>/fd` read  | blocked (host PID absent)           |
| Broker signal                 | blocked                             |
| Broker Unix socket            | reachable, returns `ok`             |
| `systemctl --user`            | blocked                             |
| `/proc/self/root` host escape | blocked (new root has no host path) |
| Verdict                       | `pass`                              |

Five consecutive runs after removing a broker close/write race all passed. The earlier race produced `EPIPE` on the client write; the probe no longer writes to the broker before reading.

## What this establishes

The composed sandbox can hide the broker from the agent, deny credential-file access, prevent signals and user-systemd control, and still provide the intended broker socket. That is enough to make system-wide dedicated users an optional hardening choice rather than a requirement for a single-user install.

## Residuals

- Requires unprivileged user namespaces. A host that disables them must fall back to a dedicated UID or refuse to start.
- Requires Landlock with a compatible ABI. The probe used ABI 7; older kernels need a fallback.
- The broker and supervisor remain the same UID. The isolation is namespace/Landlock based, not UID based.
- `io_uring` is enabled on this host; Landlock has known bypass surface. Add a seccomp/io_uring policy before production.
- `/etc` and `/nix/store` are visible read-only. Do not place secrets there.
- No `/dev` is mounted in this probe. Production needs a minimal `/dev/null` and entropy policy.
- Cgroup delegation and ceiling control were not exercised in A2b; they remain owned by the deployment plane.
- The broker socket is reachable by design; broker request authorization is not provided by same-UID socket credentials.
- Kernel exploits and same-kernel attack surface remain accepted residuals.
- No reboot, backup, or cross-UID comparison was run.

## Relation to DG-3

This is **option E**: rootless sandbox with memory-only broker credentials and a mounted minimal root. Variant D — dedicated `kanbei-broker`/`kanbei-sandbox` system users — remains the fallback for hostile-multi-tenant or user-namespace-disabled hosts.

No decision is recorded. The human must choose E or D and decide whether A2b's residuals are acceptable.
