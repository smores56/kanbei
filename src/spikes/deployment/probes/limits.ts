import { Effect, Option } from "effect"
import { readProcOr, requireProgram, truncate } from "../facts.ts"
import { recover } from "../recover.ts"
import { findings, probe, ProbeEnv } from "../probe.ts"
import type { ProbeEnvShape } from "../probe.ts"
import { showProperties, stopUnit } from "../systemd-run.ts"

const allocScript = new URL("../scripts/alloc-child.ts", import.meta.url).pathname

interface UnitState {
  readonly activeState: string
  readonly result: string
  readonly memoryPeak: string
  readonly restarts: string
}

const unitState = (
  unit: string,
): Effect.Effect<UnitState, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const properties = yield* showProperties(unit, [
      "ActiveState",
      "Result",
      "MemoryPeak",
      "NRestarts",
    ])
    return {
      activeState: properties["ActiveState"] ?? "unknown",
      result: properties["Result"] ?? "unknown",
      memoryPeak: properties["MemoryPeak"] ?? "unknown",
      restarts: properties["NRestarts"] ?? "unknown",
    }
  })

const waitForInactive = (
  unit: string,
  attempts: number,
): Effect.Effect<UnitState, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const state = yield* unitState(unit)
    if (state.activeState !== "active" && state.activeState !== "activating") return state
    if (attempts <= 0) return state
    yield* Effect.sleep("250 millis")
    return yield* waitForInactive(unit, attempts - 1)
  })

const startUnit = (
  unit: string,
  properties: ReadonlyArray<string>,
  program: string,
  args: ReadonlyArray<string>,
): Effect.Effect<{ code: number; stderr: string }, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const systemdRun = yield* env.command.locate("systemd-run")
    if (Option.isNone(systemdRun)) {
      return { code: 127, stderr: "systemd-run is not installed" }
    }
    const result = yield* env.command.run(
      systemdRun.value,
      [
        "--user",
        `--unit=${unit}`,
        ...properties.flatMap((property) => ["--property", property]),
        program,
        ...args,
      ],
      { timeoutMs: 20_000 },
    )
    return { code: result.code, stderr: result.stderr }
  })

const memoryMax = probe(
  {
    id: "LM-01",
    category: "resource-limits",
    arm: "pure-user-space",
    method: "unit with MemoryMax=64M allocates 400 MiB and is observed until it dies",
    enforcement: { pure: "conventional", dedicated: "kernel" },
    prerequisites: ["systemd-run", "systemctl", "deno"],
  },
  Effect.gen(function* () {
    const unit = "kanbei-dep1-lm01"
    yield* stopUnit(unit)
    const started = yield* startUnit(unit, ["MemoryMax=64M"], Deno.execPath(), [
      "run",
      "--allow-read",
      allocScript,
      "400",
    ])
    const state = yield* waitForInactive(unit, 80)
    yield* stopUnit(unit)
    return findings(
      state.result === "oom-kill" ? "pass" : "fail",
      `unit result=${state.result}, MemoryPeak=${state.memoryPeak} bytes, ActiveState=${state.activeState}`,
      `systemd-run: exit=${started.code} ${started.stderr.slice(0, 200)} | final state ${
        JSON.stringify(state)
      }`,
      state.result === "oom-kill"
        ? "the ceiling is enforced by the kernel cgroup, but a same-UID process can rewrite it (see SD-10)"
        : "the memory ceiling did not kill the allocation; hard ceilings are not enforceable this way",
    )
  }),
)

const memoryHigh = probe(
  {
    id: "LM-02",
    category: "resource-limits",
    arm: "pure-user-space",
    method: "unit with MemoryHigh=32M allocates 400 MiB; read memory.events",
    enforcement: { pure: "conventional", dedicated: "kernel" },
    prerequisites: ["systemd-run", "systemctl", "deno"],
  },
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const unit = "kanbei-dep1-lm02"
    yield* stopUnit(unit)
    yield* startUnit(unit, ["MemoryHigh=32M"], Deno.execPath(), [
      "run",
      "--allow-read",
      allocScript,
      "400",
    ])
    yield* Effect.sleep("3 seconds")
    const cgroup = yield* showProperties(unit, ["ControlGroup"])
    const path = `/sys/fs/cgroup${cgroup["ControlGroup"] ?? ""}/memory.events`
    const events = yield* env.fs.readFileString(path).pipe(
      recover(() => Effect.succeed("unreadable")),
    )
    const state = yield* unitState(unit)
    yield* stopUnit(unit)
    const highCount = Number(/high (\d+)/u.exec(events)?.[1] ?? "0")
    return findings(
      highCount > 0 ? "pass" : "inconclusive",
      `memory.events high count=${highCount}, unit ActiveState=${state.activeState}, result=${state.result}`,
      `memory.events: ${truncate(events.replaceAll("\n", " "))}`,
      highCount > 0
        ? "MemoryHigh throttles (reclaim pressure) rather than terminating; MemoryMax is the terminating ceiling"
        : "no reclaim pressure observed within the window",
    )
  }),
)

const tasksMax = probe(
  {
    id: "LM-03",
    category: "resource-limits",
    arm: "pure-user-space",
    method: "unit with TasksMax=16 spawns 40 children",
    enforcement: { pure: "conventional", dedicated: "kernel" },
    prerequisites: ["systemd-run", "sh"],
  },
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const sh = yield* requireProgram("sh")
    const systemdRun = yield* requireProgram("systemd-run")
    const result = yield* env.command.run(
      systemdRun,
      [
        "--user",
        "--wait",
        "--pipe",
        "--quiet",
        "--property",
        "TasksMax=16",
        "--property",
        "TimeoutStopSec=2",
        sh,
        "-c",
        "n=0; while [ $n -lt 40 ]; do sleep 5 & n=$((n+1)); done; echo spawned=$n",
      ],
      { timeoutMs: 20_000 },
    )
    const limited = /Resource temporarily unavailable|Interrupted system call/u.test(result.stderr)
    return findings(
      limited ? "pass" : "fail",
      limited
        ? "fork was denied at the pid ceiling; pids.max is enforced by the kernel"
        : `all children spawned: ${result.stdout.trim()}`,
      `unit output: exit=${result.code} ${truncate(result.stderr)}`,
      limited
        ? "a same-UID process can raise TasksMax on its own cgroup subtree (see SD-10)"
        : undefined,
    )
  }),
)

const cpuQuota = probe(
  {
    id: "LM-04",
    category: "resource-limits",
    arm: "pure-user-space",
    method: "unit with CPUQuota=10% runs a busy loop; read cpu.stat nr_throttled",
    enforcement: { pure: "conventional", dedicated: "kernel" },
    prerequisites: ["systemd-run", "systemctl", "sh"],
  },
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const sh = yield* requireProgram("sh")
    const unit = "kanbei-dep1-lm04"
    yield* stopUnit(unit)
    yield* startUnit(unit, ["CPUQuota=10%"], sh, [
      "-c",
      "while true; do :; done",
    ])
    yield* Effect.sleep("2 seconds")
    const cgroup = yield* showProperties(unit, ["ControlGroup"])
    const base = `/sys/fs/cgroup${cgroup["ControlGroup"] ?? ""}`
    const cpuMax = yield* env.fs.readFileString(`${base}/cpu.max`).pipe(
      recover(() => Effect.succeed("unreadable")),
    )
    const cpuStat = yield* env.fs.readFileString(`${base}/cpu.stat`).pipe(
      recover(() => Effect.succeed("")),
    )
    yield* stopUnit(unit)
    const throttled = Number(/nr_throttled (\d+)/u.exec(cpuStat)?.[1] ?? "0")
    return findings(
      throttled > 0 ? "pass" : "inconclusive",
      `cpu.max=${cpuMax.trim()}, nr_throttled=${throttled}`,
      `cpu.stat: ${truncate(cpuStat.replaceAll("\n", " "))}`,
      throttled > 0
        ? "CPU quota is enforced by the kernel; same-UID code can raise it (see SD-10)"
        : undefined,
    )
  }),
)

const ioController = probe(
  {
    id: "LM-05",
    category: "resource-limits",
    arm: "pure-user-space",
    method: "inspect delegated controllers and io.weight availability for the user manager",
  },
  Effect.gen(function* () {
    const uid = Deno.uid()
    const controllers = yield* readProcOr(
      `/sys/fs/cgroup/user.slice/user-${uid}.slice/user@${uid}.service/cgroup.controllers`,
      "absent",
    )
    const ioWeight = yield* readProcOr(
      `/sys/fs/cgroup/user.slice/user-${uid}.slice/user@${uid}.service/io.weight`,
      "absent",
    )
    const hasIo = controllers.split(/\s+/u).includes("io")
    return findings(
      hasIo ? "pass" : "fail",
      `delegated controllers=[${controllers.trim()}], io.weight=${ioWeight}`,
      `cat .../user@${uid}.service/cgroup.controllers`,
      hasIo
        ? undefined
        : "IOWeight cannot be applied: the io controller is not delegated to the user manager, so IO ceilings are unavailable",
    )
  }),
)

export const limitProbes = Effect.all(
  [memoryMax, memoryHigh, tasksMax, cpuQuota, ioController],
  { concurrency: 1 },
).pipe(Effect.map((results) => results))
