import { Effect, Option } from "effect"
import { ensureDir, lastLine, requireProgram, truncate } from "../facts.ts"
import { recover } from "../recover.ts"
import { findings, probe, ProbeEnv, unavailable } from "../probe.ts"
import type { ProbeEnvShape } from "../probe.ts"

const listenScript = new URL("../scripts/listen-once.ts", import.meta.url).pathname
const connectScript = new URL("../scripts/connect-probe.ts", import.meta.url).pathname

const externalAddress = (): Effect.Effect<string | undefined, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const ip = yield* env.command.locate("ip")
    if (Option.isNone(ip)) return undefined
    const result = yield* env.command.run(
      ip.value,
      ["-o", "-4", "addr", "show", "scope", "global"],
      {
        timeoutMs: 8_000,
      },
    ).pipe(recover(() => Effect.succeed({ code: 1, stdout: "", stderr: "" })))
    const match = /inet (\d+\.\d+\.\d+\.\d+)/u.exec(result.stdout)
    return match?.[1]
  })

const connectOutcome = (
  host: string,
  port: number,
): Effect.Effect<{ connected: boolean; error: string }, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const result = yield* env.command.run(
      Deno.execPath(),
      ["run", "--allow-net", connectScript, host, String(port)],
      { timeoutMs: 12_000 },
    ).pipe(recover(() => Effect.succeed({ code: 1, stdout: "", stderr: "run failed" })))
    const line = lastLine(result.stdout)
    if (line === undefined) return { connected: false, error: result.stderr.slice(0, 120) }
    const parsed = JSON.parse(line) as { connected?: boolean; error?: string }
    return { connected: parsed.connected === true, error: parsed.error ?? "" }
  })

const startListener = (
  host: string,
): Effect.Effect<number, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const dir = yield* ensureDir("network")
    const ready = env.path.join(dir, `ready-${host.replaceAll(":", "_").replaceAll(".", "_")}.json`)
    yield* env.fs.remove(ready).pipe(Effect.ignore)
    yield* env.command.spawn(Deno.execPath(), [
      "run",
      "--allow-net",
      "--allow-write",
      listenScript,
      host,
      ready,
    ])
    const port = yield* waitForPort(ready, 60)
    if (port === undefined) {
      return yield* unavailable(`listener on ${host} did not report a port`)
    }
    return port
  })

const waitForPort = (
  ready: string,
  attempts: number,
): Effect.Effect<number | undefined, unknown, ProbeEnvShape> =>
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const exists = yield* env.fs.exists(ready)
    if (exists) {
      const raw = yield* env.fs.readFileString(ready).pipe(recover(() => Effect.succeed("")))
      const port = Number(/"port":(\d+)/u.exec(raw)?.[1] ?? "0")
      return port > 0 ? port : undefined
    }
    if (attempts <= 0) return undefined
    yield* Effect.sleep("50 millis")
    return yield* waitForPort(ready, attempts - 1)
  })

const loopbackOnly = probe(
  {
    id: "NW-01",
    category: "network",
    arm: "both",
    method: "bind 127.0.0.1, then connect via loopback and via the host's global IPv4 address",
    enforcement: { pure: "kernel", dedicated: "kernel" },
    prerequisites: ["deno", "ip"],
  },
  Effect.gen(function* () {
    const address = yield* externalAddress()
    if (address === undefined) {
      return findings(
        "blocked",
        "host has no global IPv4 address to test external reachability",
        "",
      )
    }
    const port = yield* startListener("127.0.0.1")
    const loopback = yield* connectOutcome("127.0.0.1", port)
    const external = yield* connectOutcome(address, port)
    return findings(
      loopback.connected && !external.connected ? "pass" : "fail",
      `loopback connect=${loopback.connected}, ${address} connect=${external.connected}`,
      `port ${port}; external error: ${truncate(external.error)}`,
      loopback.connected && !external.connected
        ? "D-6 holds only while every listener binds 127.0.0.1 explicitly; a wildcard bind is reachable"
        : "loopback-only binding did not isolate the service",
    )
  }),
)

const wildcardBind = probe(
  {
    id: "NW-02",
    category: "network",
    arm: "both",
    method: "bind 0.0.0.0 and connect through the host's global IPv4 address",
    prerequisites: ["deno", "ip"],
  },
  Effect.gen(function* () {
    const address = yield* externalAddress()
    if (address === undefined) {
      return findings("blocked", "host has no global IPv4 address", "")
    }
    const port = yield* startListener("0.0.0.0")
    const external = yield* connectOutcome(address, port)
    return findings(
      external.connected ? "fail" : "pass",
      `wildcard listener reachable on ${address}=${external.connected}`,
      `port ${port}; error ${truncate(external.error)}`,
      external.connected
        ? "a wildcard bind exposes the service to the host network; the UI and broker sockets must never bind 0.0.0.0"
        : undefined,
    )
  }),
)

const hostListeners = probe(
  {
    id: "NW-03",
    category: "network",
    arm: "both",
    method: "list host TCP listeners and their bind addresses",
    prerequisites: ["ss"],
  },
  Effect.gen(function* () {
    const env = yield* ProbeEnv
    const ss = yield* requireProgram("ss")
    const result = yield* env.command.run(ss, ["-ltnH"], { timeoutMs: 8_000 })
    const lines = result.stdout.split("\n").filter((line) => line.trim() !== "")
    const wildcard = lines.filter((line) => {
      const local = line.trim().split(/\s+/u)[3] ?? ""
      return local.startsWith("0.0.0.0:") || local.startsWith("*:") || local.startsWith("[::]:")
    })
    return findings(
      wildcard.length === 0 ? "pass" : "inconclusive",
      `${lines.length} TCP listeners; ${wildcard.length} on wildcard addresses`,
      truncate(lines.slice(0, 12).join("\n")),
      wildcard.length === 0
        ? undefined
        : "other host services listen on wildcard addresses; Kanbei must not add to that set and Access policy is the only ingress control",
    )
  }),
)

export const networkProbes = Effect.all(
  [loopbackOnly, wildcardBind, hostListeners],
  { concurrency: 1 },
).pipe(Effect.map((results) => results))
