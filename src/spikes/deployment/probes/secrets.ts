import { Effect } from "effect"
import { canReadFile, ensureDir, fileMode, readProcOr, requireProgram } from "../facts.ts"
import { recover } from "../recover.ts"
import { findings, probe, ProbeEnv } from "../probe.ts"

const sameUidMode = probe(
  {
    id: "SC-01",
    category: "secret-reachability",
    arm: "both",
    method: "read a 0600 file from a 0700 directory as the same UID",
    enforcement: { pure: "conventional", dedicated: "kernel" },
    prerequisites: ["deno"],
  },
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const dir = yield* ensureDir("secrets")
    yield* env.fs.chmod(dir, 0o700).pipe(Effect.ignore)
    const target = env.path.join(dir, "credential")
    yield* env.fs.writeFileString(target, "credential-bytes\n")
    yield* env.fs.chmod(target, 0o600)
    const readable = yield* canReadFile(target)
    const mode = yield* fileMode(target)
    return findings(
      readable ? "fail" : "pass",
      readable
        ? `0600 file in a 0700 directory is readable by the same UID (mode ${mode})`
        : `0600 file remained unreadable (mode ${mode})`,
      `open ${target} with mode ${mode} -> readable=${readable}`,
      readable
        ? "file modes separate UIDs, not processes of one UID; secrets must be unreachable from the agent UID, not merely 0600"
        : undefined,
    )
  }),
)

const conventionalSecretPaths = probe(
  {
    id: "SC-02",
    category: "secret-reachability",
    arm: "both",
    method: "stat conventional secret locations without reading contents",
    prerequisites: [],
  },
  Effect.gen(function* () {
    const home = Deno.env.get("HOME") ?? ""
    const candidates = [
      `${home}/.ssh`,
      `${home}/.config/sops`,
      `${home}/.config/age`,
      `${home}/.config/kanbei/secrets`,
    ]
    const inspected: Array<string> = []
    for (const candidate of candidates) {
      const mode = yield* fileMode(candidate)
      inspected.push(`${candidate}=${mode}`)
    }
    const stateMode = yield* fileMode(`${home}/.local/state`)
    const present = inspected.filter((entry) => !entry.endsWith("=absent"))
    return findings(
      present.length === 0 ? "pass" : "fail",
      present.length === 0
        ? "no conventional secret material exists on this host"
        : `${present.length} conventional secret location(s) present; every one is same-UID readable`,
      `${inspected.join(" | ")} | state-tree=${stateMode}`,
      present.length === 0
        ? undefined
        : "contents were not read; presence alone means a same-UID agent can read them",
    )
  }),
)

const auditSink = probe(
  {
    id: "SC-03",
    category: "fail-closed",
    arm: "both",
    method:
      "create a 0444 audit sink in a user-owned tree, then truncate and unlink it as the same UID",
    enforcement: { pure: "conventional", dedicated: "kernel" },
    prerequisites: ["deno"],
  },
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const dir = yield* ensureDir("audit")
    const sink = env.path.join(dir, "audit.log")
    yield* env.fs.writeFileString(sink, "append-only-intent\n")
    yield* env.fs.chmod(sink, 0o444)
    const truncated = yield* env.fs.writeFileString(sink, "rewritten\n").pipe(
      Effect.as(true),
      recover(() => Effect.succeed(false)),
    )
    const unlinked = yield* env.fs.remove(sink).pipe(
      Effect.as(true),
      recover(() => Effect.succeed(false)),
    )
    return findings(
      truncated || unlinked ? "fail" : "pass",
      `same-UID truncate=${truncated}, unlink=${unlinked} on a 0444 audit file`,
      `${sink} mode 0444 owned by uid ${Deno.uid()}`,
      truncated || unlinked
        ? "append-only audit is conventional, not kernel-enforced, under one UID; an agent can erase evidence of its own denied attempt"
        : undefined,
    )
  }),
)

const dockerGroup = probe(
  {
    id: "SC-04",
    category: "secret-reachability",
    arm: "both",
    method: "check supplementary groups and the container-runtime socket ownership",
    prerequisites: ["id"],
  },
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const id = yield* requireProgram("id")
    const groups = yield* env.command.run(id, ["-nG"], { timeoutMs: 8_000 })
    const groupNames = groups.stdout.trim().split(/\s+/u)
    const socketMode = yield* fileMode("/var/run/docker.sock")
    const privileged = groupNames.includes("docker") && socketMode !== "absent"
    return findings(
      privileged ? "fail" : "pass",
      `groups=[${groupNames.join(" ")}], /var/run/docker.sock mode=${socketMode}`,
      `id -nG -> ${groups.stdout.trim()}`,
      privileged
        ? "membership in the container-runtime group is root-equivalent; the deployment user must not hold it or every UID separation on this host is bypassable"
        : undefined,
    )
  }),
)

const coreDumps = probe(
  {
    id: "SC-05",
    category: "secret-reachability",
    arm: "both",
    method: "read fs.suid_dumpable and kernel.core_pattern",
  },
  Effect.gen(function* () {
    const dumpable = yield* readProcOr("/proc/sys/fs/suid_dumpable", "absent")
    const pattern = yield* readProcOr("/proc/sys/kernel/core_pattern", "absent")
    const captured = pattern.startsWith("|")
    return findings(
      captured ? "inconclusive" : "pass",
      `fs.suid_dumpable=${dumpable}, core_pattern=${pattern.slice(0, 60)}`,
      "cat /proc/sys/fs/suid_dumpable; cat /proc/sys/kernel/core_pattern",
      captured
        ? "core dumps are piped to the system coredump handler; a broker crash could capture credential bytes, so the broker unit needs LimitCORE=0"
        : undefined,
    )
  }),
)

export const secretProbes = Effect.all(
  [sameUidMode, conventionalSecretPaths, auditSink, dockerGroup, coreDumps],
  { concurrency: 1 },
).pipe(Effect.map((results) => results))
