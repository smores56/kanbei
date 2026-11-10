import { Effect, Schema } from "effect"
import { ensureDir, lastLine, readSysctl } from "../facts.ts"
import { recover } from "../recover.ts"
import { findings, probe, ProbeEnv, unavailable } from "../probe.ts"
import type { ProbeEnvShape } from "../probe.ts"

const childScript = new URL("../scripts/landlock-child.ts", import.meta.url).pathname

const LandlockOutput = Schema.Record(Schema.String, Schema.Unknown)

const runChild = (
  args: ReadonlyArray<string>,
): Effect.Effect<Record<string, unknown>, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const result = yield* env.command.run(
      Deno.execPath(),
      ["run", "--allow-ffi", "--allow-net", "--allow-read", childScript, ...args],
      { timeoutMs: 25_000 },
    )
    const line = lastLine(result.stdout)
    if (line === undefined) {
      return yield* unavailable(
        "landlock child produced no output",
        `exit=${result.code} ${result.stderr.slice(0, 300)}`,
      )
    }
    return yield* Schema.decodeUnknownEffect(LandlockOutput)(JSON.parse(line)).pipe(
      Effect.mapError(() => new Error("unparseable landlock output")),
    )
  })

const abiProbe = probe(
  {
    id: "LL-01",
    category: "landlock",
    arm: "both",
    method: "landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION=1)",
    prerequisites: ["deno", "--allow-ffi"],
  },
  Effect.gen(function* () {
    const output = yield* runChild(["fs", "/nonexistent", "/nonexistent-file"])
    const abi = Number(output["abi"])
    return findings(
      abi >= 1 ? "pass" : "fail",
      `Landlock ABI=${abi}${abi >= 4 ? " (network rules available)" : " (no network rules)"}`,
      `landlock_create_ruleset version query -> ${JSON.stringify(output)}`,
      abi >= 4 ? undefined : "ABI < 4 cannot restrict TCP bind/connect",
    )
  }),
)

const fsProbe = probe(
  {
    id: "LL-02",
    category: "landlock",
    arm: "both",
    method: "apply a path-beneath ruleset, then open an allowed and a denied path",
    enforcement: { pure: "kernel", dedicated: "kernel" },
    prerequisites: ["deno", "--allow-ffi"],
  },
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const inside = yield* ensureDir("landlock", "inside")
    const outside = yield* ensureDir("landlock", "outside")
    const insideFile = env.path.join(inside, "allowed.txt")
    const outsideFile = env.path.join(outside, "secret.txt")
    yield* env.fs.writeFileString(insideFile, "allowed\n")
    yield* env.fs.writeFileString(outsideFile, "denied\n")
    const output = yield* runChild(["fs", inside, outsideFile])
    const created = output["created"] === true
    const restricted = created && Number(output["restrict"]) === 0
    const insideOpened = output["insideOpened"] === true
    const outsideOpened = output["outsideOpened"] === true
    const content = typeof output["insideContent"] === "string" ? output["insideContent"] : ""
    const enforced = restricted && insideOpened && !outsideOpened && content === "allowed"
    return findings(
      enforced ? "pass" : "fail",
      enforced
        ? "Landlock allowed the granted path and denied an unlisted path"
        : `Landlock did not enforce the ruleset: ${JSON.stringify(output)}`,
      `landlock child ${JSON.stringify(output)}`,
      enforced
        ? "the ruleset only binds processes that call landlock_restrict_self; the sandbox supervisor must do so before exec"
        : "Landlock is unavailable or ineffective on this kernel; declare the weaker posture or refuse to start",
    )
  }),
)

const netProbe = probe(
  {
    id: "LL-03",
    category: "landlock",
    arm: "both",
    method: "restrict LANDLOCK_ACCESS_NET_BIND_TCP, then Deno.listen on 127.0.0.1",
    enforcement: { pure: "kernel", dedicated: "kernel" },
    prerequisites: ["deno", "--allow-ffi", "Landlock ABI >= 4"],
  },
  Effect.gen(function* () {
    const output = yield* runChild(["net"])
    const created = output["created"] === true
    const bind = output["bind"]
    return findings(
      created && bind === "denied" ? "pass" : created ? "fail" : "blocked",
      created
        ? `TCP bind after Landlock net rule = ${String(bind)}`
        : "kernel refused the Landlock network ruleset",
      `landlock child net ${JSON.stringify(output)}`,
      created && bind === "denied"
        ? "Landlock restricts bind/connect but not UDP; UDP egress remains an accepted residual"
        : undefined,
    )
  }),
)

const ioUringProbe = probe(
  {
    id: "LL-04",
    category: "landlock",
    arm: "both",
    method: "read kernel.io_uring_disabled and kernel.unprivileged_bpf_disabled",
  },
  Effect.gen(function* () {
    const ioUring = yield* readSysctl("kernel.io_uring_disabled").pipe(
      recover(() => Effect.succeed("absent")),
    )
    const bpf = yield* readSysctl("kernel.unprivileged_bpf_disabled").pipe(
      recover(() => Effect.succeed("absent")),
    )
    return findings(
      ioUring !== "absent" && ioUring !== "0" ? "pass" : "fail",
      `kernel.io_uring_disabled=${ioUring}, kernel.unprivileged_bpf_disabled=${bpf}`,
      "cat /proc/sys/kernel/io_uring_disabled; cat /proc/sys/kernel/unprivileged_bpf_disabled",
      ioUring === "0"
        ? "io_uring is enabled; io_uring operations are known to bypass parts of Landlock and seccomp, so a supervisor must add its own syscall filter"
        : undefined,
    )
  }),
)

export const landlockProbes = Effect.all(
  [abiProbe, fsProbe, netProbe, ioUringProbe],
  { concurrency: 1 },
)
