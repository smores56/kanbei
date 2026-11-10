import { Effect } from "effect"
import { ensureDir, requireProgram } from "../facts.ts"
import { recover } from "../recover.ts"
import { findings, probe, ProbeEnv, unavailable } from "../probe.ts"
import type { ProbeEnvShape } from "../probe.ts"
import { showProperties, stopUnit } from "../systemd-run.ts"

const crashScript = new URL("../scripts/crash-mid-effect.ts", import.meta.url).pathname

const restartPolicy = probe(
  {
    id: "RC-01",
    category: "recovery",
    arm: "pure-user-space",
    method: "unit with Restart=on-failure and StartLimitBurst=2 exits 7 repeatedly",
    enforcement: { pure: "conventional", dedicated: "kernel" },
    prerequisites: ["systemd-run", "systemctl", "sh"],
  },
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const sh = yield* requireProgram("sh")
    const systemdRun = yield* requireProgram("systemd-run")
    const unit = "kanbei-dep1-rc01"
    yield* stopUnit(unit)
    const started = yield* env.command.run(
      systemdRun,
      [
        "--user",
        `--unit=${unit}`,
        "--property",
        "Type=exec",
        "--property",
        "Restart=on-failure",
        "--property",
        "RestartSec=1",
        "--property",
        "StartLimitBurst=2",
        "--property",
        "StartLimitIntervalSec=20",
        sh,
        "-c",
        "exit 7",
      ],
      { timeoutMs: 15_000 },
    )
    yield* Effect.sleep("7 seconds")
    const properties = yield* showProperties(unit, ["NRestarts", "Result", "ExecMainStatus"])
    yield* stopUnit(unit)
    const restarts = Number(properties["NRestarts"] ?? "0")
    const limited = properties["Result"] === "start-limit-hit"
    return findings(
      restarts >= 2 && limited ? "pass" : "fail",
      `NRestarts=${restarts}, Result=${properties["Result"] ?? "unknown"}, ExecMainStatus=${
        properties["ExecMainStatus"] ?? "unknown"
      }`,
      `systemd-run: exit=${started.code} ${started.stderr.slice(0, 160)}`,
      limited
        ? "restart discipline works, but the same UID can stop, disable, or reconfigure the unit (SD-10)"
        : "start limits did not engage; restart loops would be unbounded",
    )
  }),
)

const orphan = probe(
  {
    id: "RC-02",
    category: "recovery",
    arm: "pure-user-space",
    method: "background child outlives its parent shell",
    enforcement: { pure: "conventional", dedicated: "kernel" },
    prerequisites: ["sh", "setsid"],
  },
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const sh = yield* requireProgram("sh")
    const dir = yield* ensureDir("recovery")
    const pidFile = env.path.join(dir, "orphan.pid")
    const result = yield* env.command.run(sh, [
      "-c",
      `setsid sleep 30 >/dev/null 2>&1 & echo $! > ${pidFile}`,
    ], { timeoutMs: 10_000 })
    if (result.code !== 0) {
      return yield* unavailable("could not start the detached child", result.stderr)
    }
    const raw = yield* env.fs.readFileString(pidFile).pipe(
      recover(() => Effect.succeed("")),
    )
    const pid = Number(raw.trim())
    yield* Effect.sleep("500 millis")
    const alive = pid > 0 && (yield* env.fs.exists(`/proc/${pid}`))
    if (alive) {
      yield* env.command.run("sh", ["-c", `kill -KILL ${pid} 2>/dev/null || true`]).pipe(
        Effect.ignore,
      )
    }
    return findings(
      alive ? "fail" : "pass",
      alive
        ? `detached sandbox-shaped child ${pid} survived its parent`
        : "detached child did not survive its parent",
      `setsid sleep 30 & echo $! -> pid=${pid} alive=${alive}`,
      alive
        ? "killing a supervisor leaves orphans; a system-unit KillMode=control-group or a reaper is required"
        : undefined,
    )
  }),
)

const linger = probe(
  {
    id: "RC-03",
    category: "recovery",
    arm: "pure-user-space",
    method: "loginctl show-user <user> -p Linger",
    prerequisites: ["loginctl"],
  },
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const loginctl = yield* requireProgram("loginctl")
    const user = Deno.env.get("USER") ?? "unknown"
    const result = yield* env.command.run(loginctl, ["show-user", user, "-p", "Linger"], {
      timeoutMs: 10_000,
    })
    const lingerValue = result.stdout.trim().replace("Linger=", "")
    return findings(
      lingerValue === "yes" ? "pass" : "fail",
      `Linger=${lingerValue}`,
      `loginctl show-user ${user} -p Linger -> ${result.stdout.trim() || result.stderr.trim()}`,
      lingerValue === "yes"
        ? undefined
        : "the user manager stops at logout, so an always-on deployment dies with the session; enabling linger requires a privileged change",
    )
  }),
)

export const crashMidEffectProbe = probe(
  {
    id: "RC-04",
    category: "fail-closed",
    arm: "both",
    method: "kill a child after it records intent; classify the interrupted effect",
    prerequisites: ["deno"],
  },
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const dir = yield* ensureDir("recovery", "effect")
    const intentPath = env.path.join(dir, "intent")
    const completionPath = env.path.join(dir, "completion")
    yield* env.fs.remove(intentPath).pipe(Effect.ignore)
    yield* env.fs.remove(completionPath).pipe(Effect.ignore)
    const handle = yield* env.command.spawn(Deno.execPath(), [
      "run",
      "--allow-read",
      "--allow-write",
      crashScript,
      dir,
    ])
    const waited = yield* waitForFile(intentPath, 50)
    yield* handle.kill()
    const completed = yield* env.fs.exists(completionPath)
    const classification = waited && !completed ? "outcome_unknown" : "no-intent-observed"
    return findings(
      classification === "outcome_unknown" ? "pass" : "inconclusive",
      `intent recorded=${waited}, completion recorded=${completed}, classification=${classification}`,
      `kill -9 after intent; state dir ${dir}`,
      classification === "outcome_unknown"
        ? "an interrupted non-idempotent effect must be recorded as outcome_unknown and never retried blindly"
        : undefined,
    )
  }),
)

const hostReboot = probe(
  {
    id: "RC-05",
    category: "recovery",
    arm: "both",
    method: "host reboot and post-boot state verification",
  },
  Effect.succeed(
    findings(
      "blocked",
      "reboot is a destructive host action and was not performed",
      "",
      "post-reboot recovery of the ledger writer, watchdog, and sandbox reaping remains unverified",
    ),
  ),
)

const backupRestore = probe(
  {
    id: "RC-06",
    category: "recovery",
    arm: "both",
    method: "ledger/CAS writer recovery and verified backup restore",
  },
  Effect.succeed(
    findings(
      "blocked",
      "no ledger, CAS, or backup implementation exists yet in this task's scope",
      "",
      "writer recovery and restore verification must be exercised in the ledger/backup tasks",
    ),
  ),
)

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

export const recoveryProbes = Effect.all(
  [restartPolicy, orphan, linger, crashMidEffectProbe, hostReboot, backupRestore],
  { concurrency: 1 },
).pipe(Effect.map((results) => results))
