# ISO-1 retirement note

`ISO-1` / `DG-4` is **retired** by the rebased design (`04-security-deployment.md`
§8.7, `08-implementation-plan.md` decision gates). The replacement is Monty for
agent-authored programs inside the NIC-less VM: containment only, no per-agent OS
security claim. This directory is retained as historical evidence and is not a
gate.

## What the retained artifacts mean

- `result.json` / `REPORT.md` are a post-retirement rerun of the same harness,
  not the run that closed `DG-4`. They record schema version 3 and add scope
  strings, pre-kill liveness proofs for the fork-bomb and orphan probes, and an
  explicit process exit verdict.
- The rerun reaches `no-selection` because the supervision surface is now 838
  lines against the pre-registered 700-line budget. The budget is not moved
  after the fact, so this run does not reopen or reverse the recorded `DG-4`
  decision. The earlier `schemaVersion: 2` artifact remains in git history.
- Process-arm containment now passes all nine checks, including
  `hosts-agent-workload`. The WASM arm still fails `resource-ceilings` and
  `hosts-agent-workload`; the microVM arm remains inconclusive.

## Harness defects fixed while completing the pending edits

- The pre-exec barrier checked `/scratch/.kanbei-release` before `chroot`, where
  the scratch bind mount does not exist; it now checks the future root's path.
  Without this the process arm hung during cold-start measurement.
- Marker children were spawned with `stdio: "null"`, but the chroot has no
  `/dev`, so every `spawn` failed with `ENOENT`; they now use piped stdio.
- The workload probe reads back `/scratch/workload.txt` and spawns `true`
  under the same chroot constraints; both paths are covered by the agent
  permissions and no longer fail on `/dev/null`.
- The host-wide process count is recorded but no longer gates orphan
  containment: unrelated system activity moves it between samples, while the
  exact orphan pid plus start time, marker survival, and cgroup emptiness are
  the falsifiable signals.
