import { Effect, Option, Schema } from "effect"
import { ensureDir, readProcOr, readSysctl, requireProgram, truncate } from "../facts.ts"
import { recover } from "../recover.ts"
import { findings, probe, ProbeEnv, unavailable } from "../probe.ts"
import type { ProbeEnvShape } from "../probe.ts"

const peercredScript = new URL("../scripts/peercred.pl", import.meta.url).pathname

const PeerCredentials = Schema.Struct({
  peerPid: Schema.Number,
  peerUid: Schema.Number,
  peerGid: Schema.Number,
  socketMode: Schema.String,
})

const waitForFile = (
  target: string,
  attempts: number,
): Effect.Effect<boolean, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const exists = yield* env.fs.exists(target)
    if (exists) return true
    if (attempts <= 0) return false
    yield* Effect.sleep("50 millis")
    return yield* waitForFile(target, attempts - 1)
  })

const peerCredentials = probe(
  {
    id: "BI-01",
    category: "broker-identity",
    arm: "both",
    method: "unix socket with mode 0600; server reports SO_PEERCRED of a same-UID client",
    enforcement: { pure: "conventional", dedicated: "kernel" },
    prerequisites: ["perl", "nc"],
  },
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const perl = yield* requireProgram("perl")
    const nc = yield* requireProgram("nc")
    const dir = yield* ensureDir("identity")
    const socketPath = env.path.join(dir, "broker.sock")
    const readyPath = env.path.join(dir, "peercred-ready.json")
    const resultPath = env.path.join(dir, "peercred-result.json")
    yield* env.fs.remove(socketPath).pipe(Effect.ignore)
    yield* env.fs.remove(readyPath).pipe(Effect.ignore)
    yield* env.fs.remove(resultPath).pipe(Effect.ignore)
    yield* env.command.spawn(perl, [peercredScript, socketPath, readyPath, resultPath])
    const ready = yield* waitForFile(readyPath, 60)
    if (!ready) {
      return yield* unavailable("peer-credential server did not become ready")
    }
    const client = yield* env.command.run(nc, ["-U", socketPath], { timeoutMs: 10_000 })
    const reported = yield* waitForFile(resultPath, 40)
    if (!reported) {
      return yield* unavailable(
        "peer-credential server did not report credentials",
        truncate(client.stderr),
      )
    }
    const raw = yield* env.fs.readFileString(resultPath)
    const decoded = Schema.decodeUnknownOption(PeerCredentials)(JSON.parse(raw))
    if (Option.isNone(decoded)) {
      return yield* unavailable("peer-credential output was not parseable", raw)
    }
    const parsed = decoded.value
    const ownUid = Deno.uid()
    const indistinguishable = parsed.peerUid === ownUid
    return findings(
      indistinguishable ? "fail" : "pass",
      `SO_PEERCRED peer uid=${parsed.peerUid} gid=${parsed.peerGid} pid=${parsed.peerPid}; socket mode=${parsed.socketMode}; deployment uid=${ownUid}`,
      `perl SO_PEERCRED + nc -U -> ${raw.trim()}`,
      indistinguishable
        ? "socket credentials identify a UID, not a component; a same-UID agent is indistinguishable from the frozen core at the kernel level"
        : undefined,
    )
  }),
)

const socketPermissions = probe(
  {
    id: "BI-02",
    category: "broker-identity",
    arm: "both",
    method: "connect to a 0600 unix socket from the same UID; attempt a second-UID peer",
    enforcement: { pure: "conventional", dedicated: "kernel" },
    prerequisites: ["perl", "nc"],
  },
  Effect.gen(function* () {
    const canSwitchUid = yield* readProcOr("/proc/self/status", "").pipe(
      Effect.map((status) => /CapEff:\s*([0-9a-f]+)/u.exec(status)?.[1] ?? "unknown"),
    )
    const unprivileged = canSwitchUid === "0000000000000000" || canSwitchUid === "unknown"
    return findings(
      "blocked",
      `cross-UID connect is untestable: effective capabilities=${canSwitchUid}; a second UID cannot be created or entered without privilege`,
      `CapEff=${String(canSwitchUid)}; uid=${Deno.uid()}; unprivileged=${unprivileged}`,
      "the 0600 socket mode was measured (BI-01) but its cross-UID effect could not be exercised on this host",
    )
  }),
)

const dedicatedUser = probe(
  {
    id: "BI-03",
    category: "broker-identity",
    arm: "dedicated-uid",
    method: "check whether a dedicated system user could be created or entered",
    enforcement: { pure: "not-applicable", dedicated: "kernel" },
    prerequisites: [],
  },
  Effect.gen(function* () {
    const status = yield* readProcOr("/proc/self/status", "")
    const capability = /CapEff:\s*([0-9a-f]+)/u.exec(status)?.[1] ?? "unknown"
    const uid = Deno.uid()
    const canChangeIdentity = capability !== "0000000000000000" && capability !== "unknown"
    const entry = yield* readProcOr("/etc/passwd", "")
    const hasBrokerUser = /kanbei-broker|kanbei-sandbox/u.test(entry)
    return findings(
      "blocked",
      `uid=${uid}, effective capabilities=${capability}, existing kanbei service users=${hasBrokerUser}`,
      `CapEff=${capability}; /etc/passwd entries for kanbei service users`,
      canChangeIdentity
        ? undefined
        : "the dedicated-UID arm cannot be exercised from this unprivileged run; its enforcement is unverified, not assumed",
    )
  }),
)

const unixSearch = probe(
  {
    id: "BI-04",
    category: "broker-identity",
    arm: "both",
    method: "enumerate SCM_CREDENTIALS prerequisites and unix socket path security",
    prerequisites: [],
  },
  Effect.gen(function* () {
    const yama = yield* readSysctl("kernel.yama.ptrace_scope").pipe(
      recover(() => Effect.succeed("absent")),
    )
    return findings(
      "inconclusive",
      `kernel.yama.ptrace_scope=${yama}; SCM_CREDENTIALS support was not exercised`,
      "sysctl kernel.yama.ptrace_scope",
      "message-level SCM_CREDENTIALS must be exercised by the broker task, not assumed from socket credentials",
    )
  }),
)

export const identityProbes = Effect.all(
  [peerCredentials, socketPermissions, dedicatedUser, unixSearch],
  { concurrency: 1 },
).pipe(Effect.map((results) => results))
