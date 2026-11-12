import { Effect, Option } from "effect"
import { describe, filesIn, interfaceNames, lastLine, requireProgram, truncate } from "../facts.ts"
import { recover } from "../recover.ts"
import { findings, probe, ProbeEnv } from "../probe.ts"
import { runUserUnit, showProperties, stopUnit } from "../systemd-run.ts"

const bindProbeScript = new URL("../scripts/bind-probe.ts", import.meta.url).pathname

const HARDENING: ReadonlyArray<string> = [
  "ProtectSystem=strict",
  "ProtectHome=read-only",
  "PrivateTmp=yes",
  "PrivateNetwork=yes",
  "RestrictAddressFamilies=AF_UNIX AF_INET",
  "MemoryMax=64M",
  "MemoryHigh=48M",
  "CPUQuota=20%",
  "TasksMax=32",
  "NoNewPrivileges=yes",
  "SystemCallFilter=@system-service",
  "ProtectKernelTunables=yes",
  "ProtectKernelModules=yes",
  "ProtectControlGroups=yes",
  "RestrictSUIDSGID=yes",
  "LockPersonality=yes",
  "RemoveIPC=yes",
  "PrivateDevices=yes",
  "ProtectClock=yes",
  "ProtectHostname=yes",
  "RestrictRealtime=yes",
  "RestrictNamespaces=yes",
]

const EXPECTED: Readonly<Record<string, string>> = {
  ProtectSystem: "strict",
  ProtectHome: "read-only",
  PrivateTmp: "yes",
  PrivateNetwork: "yes",
  RestrictAddressFamilies: "AF_INET AF_UNIX",
  MemoryMax: String(64 * 1024 * 1024),
  MemoryHigh: String(48 * 1024 * 1024),
  TasksMax: "32",
  NoNewPrivileges: "yes",
  ProtectKernelTunables: "yes",
  ProtectKernelModules: "yes",
  ProtectControlGroups: "yes",
  RestrictSUIDSGID: "yes",
  LockPersonality: "yes",
  RemoveIPC: "yes",
  PrivateDevices: "yes",
  ProtectClock: "yes",
  ProtectHostname: "yes",
  RestrictRealtime: "yes",
  RestrictNamespaces: "yes",
}

const userManager = probe(
  {
    id: "SD-01",
    category: "systemd",
    arm: "pure-user-space",
    method: "systemctl --user show-environment and systemctl --version",
    prerequisites: ["systemctl"],
  },
  Effect.gen(function* () {
    const systemctl = yield* requireProgram("systemctl")
    const env = yield* ProbeEnv
    const reachable = yield* env.command.run(systemctl, ["--user", "show-environment"], {
      timeoutMs: 10_000,
    })
    const version = yield* env.command.run(systemctl, ["--version"], { timeoutMs: 10_000 })
    const firstLine = version.stdout.split("\n")[0]?.trim() ?? "unknown"
    return findings(
      reachable.code === 0 ? "pass" : "fail",
      reachable.code === 0 ? `user manager reachable (${firstLine})` : "no user manager",
      `show-environment: ${describe(reachable)} | version: ${firstLine}`,
      reachable.code === 0
        ? "a user manager is a per-user trust domain; any same-UID process can talk to it"
        : undefined,
    )
  }),
)

const effectiveProperties = probe(
  {
    id: "SD-02",
    category: "systemd",
    arm: "pure-user-space",
    method: "start a transient user unit with hardening directives and read effective properties",
    prerequisites: ["systemd-run", "systemctl", "sleep"],
  },
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const sleep = yield* env.command.locate("sleep").pipe(
      recover(() => Effect.succeed(Option.none())),
    )
    if (Option.isNone(sleep)) {
      return findings("blocked", "sleep(1) is not installed", "")
    }
    const unit = "kanbei-dep1-sd02"
    yield* stopUnit(unit)
    const started = yield* env.command.run(
      yield* requireProgram("systemd-run"),
      [
        "--user",
        `--unit=${unit}`,
        ...HARDENING.flatMap((property) => ["--property", property]),
        sleep.value,
        "20",
      ],
      { timeoutMs: 15_000 },
    )
    const effective = yield* showProperties(unit, Object.keys(EXPECTED))
    yield* stopUnit(unit)
    const mismatches = Object.entries(EXPECTED)
      .filter(([key, value]) => effective[key] !== value)
      .map(([key, value]) => `${key}: expected ${value}, effective ${effective[key] ?? "absent"}`)
    return findings(
      mismatches.length === 0 ? "pass" : "fail",
      mismatches.length === 0
        ? `${Object.keys(EXPECTED).length} hardening directives applied as requested`
        : `${mismatches.length} directive(s) did not apply: ${mismatches.join("; ")}`,
      `${describe(started)} | effective ${truncate(JSON.stringify(effective))}`,
      mismatches.length === 0
        ? undefined
        : "a directive that is accepted but not effective is a fail-open hazard; the deployment must verify effective values at startup",
    )
  }),
)

const protectHome = probe(
  {
    id: "SD-03",
    category: "systemd",
    arm: "pure-user-space",
    method: "unit with ProtectHome=read-only attempts to read and write $HOME",
    enforcement: { pure: "kernel", dedicated: "kernel" },
    prerequisites: ["systemd-run", "sh"],
  },
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const sh = yield* requireProgram("sh")
    const result = yield* runUserUnit(
      { properties: ["ProtectHome=read-only"] },
      sh,
      [
        "-c",
        "touch $HOME/kanbei-dep1-home-write 2>/dev/null && echo home-writable || echo home-readonly; ls $HOME >/dev/null 2>&1 && echo home-readable || echo home-unreadable",
      ],
    )
    const homePath = env.path.join(Deno.env.get("HOME") ?? "/", "kanbei-dep1-home-write")
    yield* env.fs.remove(homePath).pipe(Effect.ignore)
    const readonly = result.stdout.includes("home-readonly")
    const readable = result.stdout.includes("home-readable")
    return findings(
      readonly && readable ? "pass" : "fail",
      readonly
        ? `$HOME is read-only inside the unit and readable=${readable}`
        : "$HOME stayed writable inside the unit",
      `unit output: ${describe(result)}`,
      readonly ? undefined : "ProtectHome did not bind; the unit can rewrite the deployment tree",
    )
  }),
)

const readWritePaths = probe(
  {
    id: "SD-04",
    category: "systemd",
    arm: "pure-user-space",
    method: "ProtectSystem=strict with ReadWritePaths=<state> and a state write",
    enforcement: { pure: "kernel", dedicated: "kernel" },
    prerequisites: ["systemd-run", "sh"],
  },
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const sh = yield* requireProgram("sh")
    const home = Deno.env.get("HOME") ?? "/home/smores"
    const stateRoot = env.path.join(home, ".local/state")
    const exists = yield* env.fs.exists(stateRoot)
    if (!exists) {
      return findings("blocked", `${stateRoot} does not exist`, "")
    }
    const target = env.path.join(stateRoot, "kanbei-dep1-state-write")
    const result = yield* runUserUnit(
      { properties: ["ProtectSystem=strict", `ReadWritePaths=${stateRoot}`] },
      sh,
      ["-c", `touch ${target} 2>/dev/null && echo state-writable || echo state-readonly`],
    )
    const wrote = result.stdout.includes("state-writable")
    yield* env.fs.remove(target).pipe(Effect.ignore)
    return findings(
      wrote ? "pass" : "fail",
      wrote
        ? "state path stayed writable under ProtectSystem=strict"
        : "state path was read-only under ProtectSystem=strict even though ReadWritePaths named it",
      `unit output: ${describe(result)}`,
      wrote
        ? undefined
        : "the write fails closed (EROFS) but the directive is ineffective in this user-manager configuration; a deployment that relies on it will not start, and must refuse to start rather than run with in-memory state",
    )
  }),
)

const privateTmp = probe(
  {
    id: "SD-05",
    category: "systemd",
    arm: "pure-user-space",
    method: "compare /tmp device:inode inside a PrivateTmp unit with the host",
    enforcement: { pure: "kernel", dedicated: "kernel" },
    prerequisites: ["systemd-run", "stat"],
  },
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const stat = yield* requireProgram("stat")
    const inside = yield* runUserUnit({ properties: ["PrivateTmp=yes"] }, stat, [
      "-c",
      "%d:%i",
      "/tmp",
    ])
    const outside = yield* env.command.run(stat, ["-c", "%d:%i", "/tmp"])
    const insideId = inside.stdout.trim()
    const outsideId = outside.stdout.trim()
    const isolated = inside.code === 0 && outside.code === 0 && insideId !== outsideId
    return findings(
      isolated ? "pass" : "fail",
      isolated
        ? `/tmp is private (unit ${insideId}, host ${outsideId})`
        : `/tmp is shared (unit ${insideId}, host ${outsideId})`,
      `unit: ${describe(inside)} | host: ${describe(outside)}`,
    )
  }),
)

const privateNetwork = probe(
  {
    id: "SD-06",
    category: "network",
    arm: "pure-user-space",
    method: "enumerate interfaces inside a PrivateNetwork unit and compare with the host",
    enforcement: { pure: "kernel", dedicated: "kernel" },
    prerequisites: ["systemd-run", "sh"],
  },
  Effect.gen(function* () {
    const sh = yield* requireProgram("sh")
    const result = yield* runUserUnit(
      { properties: ["PrivateNetwork=yes"] },
      sh,
      ["-c", "cat /proc/net/dev"],
    )
    const hostInterfaces = yield* filesIn("/sys/class/net")
    const unitInterfaces = interfaceNames(result.stdout)
    const onlyLoopback = result.code === 0 && unitInterfaces.length > 0 &&
      unitInterfaces.every((name) => name === "lo")
    return findings(
      onlyLoopback ? "pass" : "fail",
      onlyLoopback
        ? `unit network namespace holds only lo; host has ${hostInterfaces.join(",")}`
        : `unit interfaces = ${unitInterfaces.join(",") || "none"}`,
      `unit /proc/net/dev: ${truncate(result.stdout.replaceAll("\n", " "))}`,
      onlyLoopback
        ? "the unit's loopback is a different device than the host loopback, so host 127.0.0.1 services are unreachable"
        : undefined,
    )
  }),
)

const addressFamilies = probe(
  {
    id: "SD-07",
    category: "systemd",
    arm: "pure-user-space",
    method: "unit with RestrictAddressFamilies=AF_UNIX AF_INET tries to bind IPv4 and IPv6",
    enforcement: { pure: "kernel", dedicated: "kernel" },
    prerequisites: ["systemd-run", "deno"],
  },
  Effect.gen(function* () {
    const result = yield* runUserUnit(
      { properties: ["RestrictAddressFamilies=AF_UNIX AF_INET"], timeoutMs: 25_000 },
      Deno.execPath(),
      ["run", "--allow-net", bindProbeScript],
    )
    const line = lastLine(result.stdout)
    const parsed = line === undefined ? undefined : JSON.parse(line) as {
      results?: ReadonlyArray<{ host: string; result: string }>
    }
    const ipv4 = parsed?.results?.find((entry) => entry.host === "127.0.0.1")?.result
    const ipv6 = parsed?.results?.find((entry) => entry.host === "::1")?.result
    return findings(
      ipv4 === "allowed" && ipv6 !== "allowed" ? "pass" : "fail",
      `IPv4 bind=${String(ipv4)}, IPv6 bind=${String(ipv6)}`,
      `unit output: ${describe(result)}`,
      ipv4 === "allowed" && ipv6 !== "allowed"
        ? undefined
        : "address-family restriction did not take effect; record it as unenforced",
    )
  }),
)

const systemCallFilter = probe(
  {
    id: "SD-08",
    category: "systemd",
    arm: "pure-user-space",
    method: "unit with SystemCallFilter=~@mount runs unshare --mount",
    enforcement: { pure: "kernel", dedicated: "kernel" },
    prerequisites: ["systemd-run", "unshare"],
  },
  Effect.gen(function* () {
    const unshare = yield* requireProgram("unshare")
    const result = yield* runUserUnit(
      { properties: ["SystemCallFilter=~@mount"] },
      unshare,
      ["--mount", "--", "true"],
    )
    const denied = result.code !== 0
    return findings(
      denied ? "pass" : "fail",
      denied
        ? "mount syscalls were denied inside the filtered unit"
        : "filtered unit still performed mount syscalls",
      `unit output: ${describe(result)}`,
      denied ? undefined : "SystemCallFilter entry was not enforced",
    )
  }),
)

const invalidProperty = probe(
  {
    id: "SD-09",
    category: "fail-closed",
    arm: "pure-user-space",
    method: "systemd-run with a misspelled property and an invalid value",
    prerequisites: ["systemd-run"],
  },
  Effect.gen(function* () {
    const systemdRun = yield* requireProgram("systemd-run")
    const env = yield* ProbeEnv
    const misspelled = yield* env.command.run(
      systemdRun,
      ["--user", "--wait", "--pipe", "--quiet", "--property", "NotARealProperty=1", "true"],
      { timeoutMs: 10_000 },
    )
    const invalidValue = yield* env.command.run(
      systemdRun,
      ["--user", "--wait", "--pipe", "--quiet", "--property", "ProtectSystem=not-a-mode", "true"],
      { timeoutMs: 10_000 },
    )
    const rejected = misspelled.code !== 0 && invalidValue.code !== 0
    return findings(
      rejected ? "pass" : "fail",
      rejected
        ? "systemd rejects unknown properties and invalid values instead of degrading"
        : "systemd accepted an unknown property or invalid value",
      `misspelled: ${describe(misspelled)} | invalid value: ${describe(invalidValue)}`,
      rejected
        ? "rejection is at unit start; a directive that is valid but ineffective (SD-04) is not caught this way"
        : "a silently ignored directive is a fail-open path",
    )
  }),
)

const unitStateControl = probe(
  {
    id: "SD-10",
    category: "recovery",
    arm: "pure-user-space",
    method: "a same-UID process stops and reconfigures a sibling unit",
    enforcement: { pure: "conventional", dedicated: "kernel" },
    prerequisites: ["systemctl", "sleep"],
  },
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const unit = "kanbei-dep1-sd10"
    yield* stopUnit(unit)
    const sleep = yield* env.command.locate("sleep")
    if (Option.isNone(sleep)) {
      return findings("blocked", "sleep(1) is not installed", "")
    }
    const started = yield* env.command.run(
      yield* requireProgram("systemd-run"),
      ["--user", `--unit=${unit}`, "--property", "MemoryMax=64M", sleep.value, "30"],
      { timeoutMs: 15_000 },
    )
    const cgroup = yield* showProperties(unit, ["ControlGroup"])
    const controlGroup = cgroup["ControlGroup"] ?? ""
    const limitPath = `/sys/fs/cgroup${controlGroup}/memory.max`
    const before = yield* env.fs.readFileString(limitPath).pipe(
      recover(() => Effect.succeed("unreadable")),
    )
    const rewritten = yield* env.fs.writeFileString(limitPath, `${128 * 1024 * 1024}\n`).pipe(
      Effect.as(true),
      recover(() => Effect.succeed(false)),
    )
    const after = yield* env.fs.readFileString(limitPath).pipe(
      recover(() => Effect.succeed("unreadable")),
    )
    yield* stopUnit(unit)
    return findings(
      rewritten ? "fail" : "pass",
      rewritten
        ? `same-UID process rewrote a sibling unit ceiling from ${before.trim()} to ${after.trim()} and can stop it`
        : "same-UID process could not rewrite a sibling unit ceiling",
      `unit start: ${
        describe(started)
      } | cgroup ${controlGroup} | memory.max ${before.trim()} -> ${after.trim()}`,
      rewritten
        ? "with Delegate=yes the user manager owns the cgroup subtree, so hard ceilings and the watchdog are conventional under pure user-space"
        : undefined,
    )
  }),
)

export const systemdProbes = Effect.all(
  [
    userManager,
    effectiveProperties,
    protectHome,
    readWritePaths,
    privateTmp,
    privateNetwork,
    addressFamilies,
    systemCallFilter,
    invalidProperty,
    unitStateControl,
  ],
  { concurrency: 1 },
).pipe(Effect.map((results) => results))
