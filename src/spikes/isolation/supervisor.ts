import { Effect } from "effect"
import { ProbeError, readTextFile } from "./exec.ts"

/**
 * S3 runs inside one transient systemd user unit acquired with `Delegate=yes`.
 * The unit cgroup is process-free and the payload lives in `<unit>/supervisor`,
 * so the supervisor owns the delegated subtree. Per-agent limits are plain
 * cgroupfs directories below it; there is no per-agent unit.
 */
export const CGROUP_MOUNT = "/sys/fs/cgroup"
export const DELEGATED_SUBPATH = "/supervisor"

/** The two host dependencies the mechanism adds. Kernel substrate is one entry. */
export const HOST_DEPENDENCIES: readonly [string, string] = [
  "user systemd manager, used once to acquire a delegated cgroup root",
  "unprivileged user+PID+mount namespaces with chroot and Landlock",
]

export const DELEGATED_BY =
  "systemd-run --user --property=Delegate=yes --property=DelegateSubgroup=supervisor"

export interface Supervisor {
  /** Path relative to `/sys/fs/cgroup`, e.g. `/user.slice/.../run-abc.service`. */
  readonly cgroupRoot: string
  readonly uid: number
}

export interface AgentLimits {
  readonly memoryMaxMib: number
  readonly tasksMax: number
}

export interface CgroupStats {
  readonly memoryCurrentBytes: number
  readonly memoryOomKills: number
  readonly pidsCurrent: number
  readonly pidsMaxEvents: number
  readonly cpuUsageUsec: number
  readonly populated: number
}

/** `0::/user.slice/.../supervisor` -> the relative cgroup path. */
export const parseSelfCgroup = (text: string): string | null =>
  text.split("\n").find((line) => line.startsWith("0::"))?.slice(3).trim() ?? null

export const delegatedRootOf = (selfCgroup: string | null): string | null =>
  selfCgroup !== null && selfCgroup.endsWith(DELEGATED_SUBPATH)
    ? selfCgroup.slice(0, -DELEGATED_SUBPATH.length)
    : null

export const parseControllers = (text: string): ReadonlyArray<string> =>
  text.trim() === "" ? [] : text.trim().split(/\s+/u)

/** `oom_kill 3` lines shared by `memory.events`, `pids.events`, and `cgroup.events`. */
export const parseKeyedNumbers = (text: string): Readonly<Record<string, number>> =>
  Object.fromEntries(
    text.split("\n").flatMap((line) => {
      const [key = "", value = ""] = line.trim().split(/\s+/u)
      const parsed = Number(value)
      return key === "" || !Number.isFinite(parsed) ? [] : [[key, parsed] as const]
    }),
  )

export const parseCpuUsageUsec = (text: string): number =>
  parseKeyedNumbers(text)["usage_usec"] ?? 0

export const parseCgroupProcs = (text: string): ReadonlyArray<number> =>
  text.split("\n").map((line) => line.trim()).filter((line) => /^\d+$/u.test(line)).map(Number)

/** Parent pid from `/proc/<pid>/stat`; `comm` may contain spaces and parentheses. */
export const parsePpid = (stat: string): number | null => {
  const close = stat.lastIndexOf(")")
  const ppid = Number(stat.slice(close + 1).trim().split(/\s+/u)[1])
  return close < 0 || !Number.isFinite(ppid) ? null : ppid
}

/** `nsdelegate` is a cgroup2 superblock option, after the mountinfo ` - ` separator. */
export const cgroupMountOptions = (mountinfo: string): ReadonlyArray<string> => {
  const line = mountinfo.split("\n").find((entry) => entry.includes(" - cgroup2 "))
  return line?.split(" - ")[1]?.trim().split(/\s+/u)[2]?.split(",") ?? []
}
export const cgroupDirectory = (supervisor: Supervisor, name: string): string =>
  `${CGROUP_MOUNT}${supervisor.cgroupRoot}/${name}`

const fail = (operation: string, detail: string): ProbeError =>
  new ProbeError({ operation, detail })

const read = (directory: string, name: string): Effect.Effect<string | null, ProbeError> =>
  readTextFile(`${directory}/${name}`)

const write = (directory: string, name: string, value: string): Effect.Effect<void, ProbeError> =>
  Effect.tryPromise({
    try: () => Deno.writeTextFile(`${directory}/${name}`, value),
    catch: (error) => fail(`write ${name}`, `${directory}: ${String(error)}`),
  })

export const cgroupProcs = (directory: string): Effect.Effect<ReadonlyArray<number>, ProbeError> =>
  read(directory, "cgroup.procs").pipe(
    Effect.map((text) => (text === null ? [] : parseCgroupProcs(text))),
  )

export const cgroupEvents = (directory: string): Effect.Effect<number, ProbeError> =>
  read(directory, "cgroup.events").pipe(
    Effect.flatMap((text) =>
      text === null
        ? Effect.fail(fail("read cgroup.events", directory))
        : Effect.succeed(parseKeyedNumbers(text)["populated"] ?? 0)
    ),
  )

export const cgroupStats = (directory: string): Effect.Effect<CgroupStats, ProbeError> =>
  Effect.gen(function* () {
    const memory = yield* read(directory, "memory.current")
    const memoryEvents = yield* read(directory, "memory.events")
    const pids = yield* read(directory, "pids.current")
    const pidsEvents = yield* read(directory, "pids.events")
    const cpu = yield* read(directory, "cpu.stat")
    const populated = yield* cgroupEvents(directory)
    if (memory === null || pids === null || cpu === null) {
      return yield* Effect.fail(fail("read cgroup stats", directory))
    }
    return {
      memoryCurrentBytes: Number(memory.trim()),
      memoryOomKills: parseKeyedNumbers(memoryEvents ?? "")["oom_kill"] ?? 0,
      pidsCurrent: Number(pids.trim()),
      pidsMaxEvents: parseKeyedNumbers(pidsEvents ?? "")["max"] ?? 0,
      cpuUsageUsec: parseCpuUsageUsec(cpu),
      populated,
    }
  })

export const migratePid = (directory: string, pid: number): Effect.Effect<void, ProbeError> =>
  write(directory, "cgroup.procs", String(pid))

export const killCgroup = (directory: string): Effect.Effect<void, ProbeError> =>
  write(directory, "cgroup.kill", "1")

export const cgroupExists = (directory: string): Effect.Effect<boolean> =>
  Effect.sync(() => {
    try {
      Deno.statSync(directory)
      return true
    } catch {
      return false
    }
  })

export const removeCgroup = (directory: string): Effect.Effect<void, ProbeError> =>
  Effect.tryPromise({
    try: () => Deno.remove(directory),
    catch: (error) => fail("rmdir cgroup", `${directory}: ${String(error)}`),
  })

/** Polls `cgroup.events populated` until the cgroup is empty or the deadline passes. */
export const awaitEmpty = (
  directory: string,
  timeoutMs: number,
): Effect.Effect<boolean, ProbeError> =>
  Effect.gen(function* () {
    const deadline = Date.now() + timeoutMs
    const poll = (): Effect.Effect<boolean, ProbeError> =>
      Effect.gen(function* () {
        if ((yield* cgroupEvents(directory)) === 0) return true
        if (Date.now() >= deadline) return false
        yield* Effect.sleep("10 millis")
        return yield* poll()
      })
    return yield* poll()
  })

const statUid = (path: string): Effect.Effect<number | null, ProbeError> =>
  Effect.tryPromise({
    try: async () => (await Deno.stat(path)).uid ?? null,
    catch: (error) => fail(`stat ${path}`, String(error)),
  }).pipe(Effect.catch(() => Effect.succeed(null)))

export const acquireSupervisor = (): Effect.Effect<Supervisor, ProbeError> =>
  Effect.gen(function* () {
    const selfCgroup = parseSelfCgroup((yield* readTextFile("/proc/self/cgroup")) ?? "")
    const root = delegatedRootOf(selfCgroup)
    const uid = Deno.uid()
    if (root === null || uid === null) {
      return yield* Effect.fail(
        fail(
          "acquire delegated cgroup",
          `expected /proc/self/cgroup to end in ${DELEGATED_SUBPATH}, got ${
            selfCgroup ?? "<empty>"
          }`,
        ),
      )
    }
    const directory = `${CGROUP_MOUNT}${root}`
    const owner = yield* statUid(directory)
    if (owner !== uid) {
      return yield* Effect.fail(
        fail("acquire delegated cgroup", `${directory} is owned by uid ${owner}, not ${uid}`),
      )
    }
    const procs = yield* cgroupProcs(directory)
    if (procs.length > 0) {
      return yield* Effect.fail(
        fail("acquire delegated cgroup", `${directory} is not process-free: ${procs.join(",")}`),
      )
    }
    const available = parseControllers((yield* read(directory, "cgroup.controllers")) ?? "")
    const missing = ["memory", "pids", "cpu"].filter((name) => !available.includes(name))
    if (missing.length > 0) {
      return yield* Effect.fail(
        fail("acquire delegated cgroup", `missing controllers: ${missing.join(",")}`),
      )
    }
    yield* write(directory, "cgroup.subtree_control", "+memory +pids +cpu")
    const enabled = parseControllers((yield* read(directory, "cgroup.subtree_control")) ?? "")
    if (["memory", "pids", "cpu"].some((name) => !enabled.includes(name))) {
      return yield* Effect.fail(
        fail(
          "enable controllers",
          `expected memory pids cpu, got ${enabled.join(" ") || "<none>"}`,
        ),
      )
    }
    return { cgroupRoot: root, uid }
  })

const mib = 1024 * 1024

export const createAgentCgroup = (
  supervisor: Supervisor,
  slot: number,
  limits: AgentLimits,
): Effect.Effect<string, ProbeError> =>
  Effect.gen(function* () {
    const directory = cgroupDirectory(supervisor, `agent-${slot}`)
    yield* Effect.tryPromise({
      try: () => Deno.mkdir(directory),
      catch: (error) => fail("mkdir cgroup", `${directory}: ${String(error)}`),
    })
    yield* write(directory, "memory.max", String(limits.memoryMaxMib * mib))
    yield* write(directory, "memory.swap.max", "0")
    yield* write(directory, "pids.max", String(limits.tasksMax))
    yield* write(directory, "cpu.max", "100000 100000")
    return directory
  })
