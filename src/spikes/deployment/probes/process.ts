import { Effect, Schema } from "effect"
import { describe, ensureDir, lastLine, readProcOr, requireProgram } from "../facts.ts"
import { recover } from "../recover.ts"
import type { ProbeUnavailable } from "../errors.ts"
import type { ChildHandle } from "../host.ts"
import { findings, probe, ProbeEnv, unavailable } from "../probe.ts"
import type { ProbeEnvShape } from "../probe.ts"

const CANARY_VALUE = "dep1-canary-9f3c"

const holderScript = new URL("../scripts/fd-holder.ts", import.meta.url).pathname
const attackerScript = new URL("../scripts/proc-attacker.ts", import.meta.url).pathname

const AttackerOutput = Schema.Record(Schema.String, Schema.Unknown)

interface Holder {
  readonly pid: number
  readonly secretPath: string
  readonly handle: ChildHandle
}

const pollReady = (
  readyPath: string,
): Effect.Effect<number, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const attempt = (remaining: number): Effect.Effect<number, unknown, ProbeEnvShape> =>
      Effect.gen(function* () {
        const exists = yield* env.fs.exists(readyPath)
        if (exists) {
          const raw = yield* env.fs.readFileString(readyPath)
          const parsed = yield* Schema.decodeUnknownEffect(
            Schema.Struct({ pid: Schema.Number }),
          )(JSON.parse(raw) as unknown).pipe(
            recover(() => Effect.succeed({ pid: -1 })),
          )
          return parsed.pid
        }
        if (remaining <= 0) {
          return yield* unavailable("holder process did not report readiness")
        }
        yield* Effect.sleep("40 millis")
        return yield* attempt(remaining - 1)
      })
    return yield* attempt(75)
  })

const startHolder = (probeId: string): Effect.Effect<Holder, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const dir = yield* ensureDir("same-uid", probeId)
    const secretPath = env.path.join(dir, "broker-secret.txt")
    const readyPath = env.path.join(dir, "ready.json")
    yield* env.fs.remove(readyPath).pipe(Effect.ignore)
    yield* env.fs.writeFileString(secretPath, `${CANARY_VALUE}\n`)
    yield* env.fs.chmod(secretPath, 0o600)
    const handle = yield* env.command.spawn(
      Deno.execPath(),
      ["run", "--allow-read", "--allow-write", holderScript, secretPath, readyPath],
      { env: { KANBEI_DEP1_CANARY: CANARY_VALUE } },
    )
    const pid = yield* pollReady(readyPath).pipe(
      Effect.tapError(() => handle.kill()),
    )
    return { pid, secretPath, handle }
  })

const stopHolder = (holder: Holder): Effect.Effect<void> => holder.handle.kill()

const withHolder = (probeId: string) =>
<A, E>(
  body: (holder: Holder) => Effect.Effect<A, E, ProbeEnvShape>,
): Effect.Effect<A, E | ProbeUnavailable | unknown, ProbeEnvShape> =>
  Effect.acquireRelease(startHolder(probeId), stopHolder).pipe(
    Effect.flatMap(body),
    Effect.scoped,
  )

const runAttacker = (
  mode: "inspect" | "mem" | "fd" | "signal",
  holder: Holder,
): Effect.Effect<Record<string, unknown>, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const deno = Deno.execPath()
    const env = yield* ProbeEnv
    const result = yield* env.command.run(
      deno,
      [
        "run",
        "-A",
        attackerScript,
        mode,
        String(holder.pid),
        holder.secretPath,
      ],
      { timeoutMs: 20_000 },
    )
    const line = lastLine(result.stdout)
    if (line === undefined) {
      return yield* unavailable(`attacker mode ${mode} produced no output`, describe(result))
    }
    const parsed = yield* Schema.decodeUnknownEffect(AttackerOutput)(JSON.parse(line)).pipe(
      recover((): Effect.Effect<Record<string, unknown>, never, never> => Effect.succeed({})),
    )
    if (parsed["ok"] !== true) {
      return yield* unavailable(`attacker mode ${mode} reported no measurement`, describe(result))
    }
    return parsed
  })

const booleanField = (record: Record<string, unknown>, key: string): boolean => record[key] === true

const cmdlineProbe = probe(
  {
    id: "PR-01",
    category: "same-uid-process",
    arm: "both",
    method: "same-UID sibling reads /proc/<broker>/cmdline, environ, maps, cgroup",
    enforcement: { pure: "conventional", dedicated: "kernel" },
    prerequisites: ["deno", "a same-UID child process"],
  },
  withHolder("PR-01")((holder) =>
    Effect.gen(function* () {
      const output = yield* runAttacker("inspect", holder)
      const readable = [
        booleanField(output, "cmdlineReadable") ? "cmdline" : undefined,
        booleanField(output, "environReadable") ? "environ" : undefined,
        booleanField(output, "mapsReadable") ? "maps" : undefined,
        booleanField(output, "cgroupReadable") ? "cgroup" : undefined,
      ].filter((part) => part !== undefined)
      const canaryLeaked = booleanField(output, "environHasCanary")
      return findings(
        readable.length === 0 ? "pass" : "fail",
        readable.length === 0
          ? "same-UID sibling could read no broker process metadata"
          : `same-UID sibling read broker ${readable.join(", ")}${
            canaryLeaked ? "; canary value from environ visible" : ""
          }`,
        `attacker inspect ${JSON.stringify(output)}`,
        readable.length === 0
          ? undefined
          : "process metadata is world-readable within a UID; no secret may live in argv or environ",
      )
    })
  ),
)

const memProbe = probe(
  {
    id: "PR-02",
    category: "same-uid-process",
    arm: "both",
    method: "same-UID sibling opens /proc/<broker>/mem",
    enforcement: { pure: "kernel", dedicated: "kernel" },
    prerequisites: ["deno", "kernel.yama.ptrace_scope >= 1"],
  },
  withHolder("PR-02")((holder) =>
    Effect.gen(function* () {
      const output = yield* runAttacker("mem", holder)
      const opened = booleanField(output, "opened")
      const yama = yield* readProcOr("/proc/sys/kernel/yama/ptrace_scope", "absent")
      return findings(
        opened ? "fail" : "pass",
        opened
          ? "same-UID sibling opened /proc/<broker>/mem"
          : "same-UID sibling was denied /proc/<broker>/mem",
        `attacker mem ${JSON.stringify(output)}; kernel.yama.ptrace_scope=${yama}`,
        opened ? undefined : "protection is provided by Yama; verify the sysctl remains >= 1",
      )
    })
  ),
)

const ptraceProbe = probe(
  {
    id: "PR-03",
    category: "same-uid-process",
    arm: "both",
    method: "strace -p <broker> from a sibling; strace -f on an own child as control",
    enforcement: { pure: "kernel", dedicated: "kernel" },
    prerequisites: ["strace", "kernel.yama.ptrace_scope >= 1"],
  },
  withHolder("PR-03")((holder) =>
    Effect.gen(function* () {
      const strace = yield* requireProgram("strace")
      const env = yield* ProbeEnv
      const sibling = yield* env.command.run(
        strace,
        ["-p", String(holder.pid), "-e", "trace=none", "-o", "/dev/null"],
        { timeoutMs: 10_000 },
      )
      const child = yield* env.command.run(
        strace,
        ["-f", "-e", "trace=none", "-o", "/dev/null", "sh", "-c", "true"],
        { timeoutMs: 10_000 },
      )
      const deniedPattern = /Operation not permitted|EPERM|ptrace_scope/u
      const siblingDenied = sibling.code !== 0 && deniedPattern.test(sibling.stderr)
      const childAllowed = child.code === 0
      return findings(
        siblingDenied ? "pass" : "fail",
        siblingDenied
          ? `PTRACE attach on a same-UID sibling denied; own child attach allowed=${childAllowed}`
          : "PTRACE attach on a same-UID sibling was not denied",
        `sibling: ${describe(sibling)} | own child: ${describe(child)}`,
        siblingDenied
          ? "denial is ancestry-based; an agent that is an ancestor of the broker could attach"
          : undefined,
      )
    })
  ),
)

const signalProbe = probe(
  {
    id: "PR-04",
    category: "same-uid-process",
    arm: "both",
    method: "same-UID sibling sends SIGTERM to the broker process",
    enforcement: { pure: "conventional", dedicated: "kernel" },
    prerequisites: ["deno"],
  },
  withHolder("PR-04")((holder) =>
    Effect.gen(function* () {
      const output = yield* runAttacker("signal", holder)
      const delivered = booleanField(output, "delivered")
      return findings(
        delivered ? "fail" : "pass",
        delivered
          ? "same-UID sibling delivered SIGTERM; broker liveness is not protected"
          : "same-UID sibling could not signal the broker process",
        `attacker signal ${JSON.stringify(output)}`,
        delivered ? undefined : "signal delivery is a kernel function of UID equality",
      )
    })
  ),
)

const fdProbe = probe(
  {
    id: "PR-05",
    category: "secret-reachability",
    arm: "both",
    method: "same-UID sibling reads a secret file through /proc/<broker>/fd/<n>",
    enforcement: { pure: "conventional", dedicated: "kernel" },
    prerequisites: ["deno", "a broker holding a credential file open"],
  },
  withHolder("PR-05")((holder) =>
    Effect.gen(function* () {
      const output = yield* runAttacker("fd", holder)
      const found = booleanField(output, "found")
      const content = typeof output["content"] === "string" ? output["content"] : ""
      const leaked = found && content.includes(CANARY_VALUE)
      return findings(
        leaked ? "fail" : "pass",
        leaked
          ? "same-UID sibling read broker credential bytes through the broker's open file descriptor"
          : "same-UID sibling could not read the broker's open descriptor",
        `attacker fd found=${found} contentMatchesCanary=${leaked}`,
        leaked
          ? "pure user-space cannot protect secrets a broker keeps open; closing descriptors immediately only shortens the window"
          : undefined,
      )
    })
  ),
)

const yamaProbe = probe(
  {
    id: "PR-06",
    category: "same-uid-process",
    arm: "both",
    method: "read kernel.yama.ptrace_scope and fs.suid_dumpable",
    prerequisites: [],
  },
  Effect.gen(function* () {
    const yama = yield* readProcOr("/proc/sys/kernel/yama/ptrace_scope", "absent")
    const dumpable = yield* readProcOr("/proc/sys/fs/suid_dumpable", "absent")
    const scope = Number(yama)
    return findings(
      scope >= 1 ? "pass" : "fail",
      `kernel.yama.ptrace_scope=${yama}, fs.suid_dumpable=${dumpable}`,
      "cat /proc/sys/kernel/yama/ptrace_scope; cat /proc/sys/fs/suid_dumpable",
      scope >= 1
        ? undefined
        : "ptrace_scope=0 lets any same-UID process attach; broker memory is unprotected",
    )
  }),
)

export const processProbes = Effect.all(
  [cmdlineProbe, memProbe, ptraceProbe, signalProbe, fdProbe, yamaProbe],
  { concurrency: 1 },
)
