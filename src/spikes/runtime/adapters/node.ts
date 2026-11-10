import { spawnSync } from "node:child_process"
import { readFileSync, writeFileSync } from "node:fs"
import { connect } from "node:net"
import { openSqlite, sqliteVersion } from "./sqlite-driver.ts"
import { peakRssFromProc } from "./types.ts"
import type { ArmHost } from "./types.ts"
import { runWorkerProbe } from "./worker.ts"
import type { PermissionProbe } from "../schema.ts"

const MODEL = "node-process-flag"

const errorCode = (error: unknown): string => {
  if (typeof error === "object" && error !== null && "code" in error) {
    const code = Reflect.get(error, "code")
    if (typeof code === "string") return code
  }
  return error instanceof Error ? error.name : String(error)
}

const attempt = async (id: string, body: () => unknown): Promise<PermissionProbe> => {
  try {
    await body()
    return { id, denied: false, code: "ALLOWED", model: MODEL }
  } catch (error) {
    const code = errorCode(error)
    // A permission model refuses before the effect; a refused connection is an
    // ordinary failure and must not be reported as containment.
    return { id, denied: code === "ERR_ACCESS_DENIED", code, model: MODEL }
  }
}

/**
 * Node arm host.
 *
 * Node 24's permission model is an opt-in process flag (`--permission`) that
 * covers filesystem, process, and worker access. It has no network capability,
 * so the network probe records `denied=false` — a capability gap, not a
 * fail-open denial, and the driver classifies it as such.
 */
export const nodeHost: ArmHost = {
  runtime: "node",
  runtimeVersion: process.version,
  sqliteVersion: sqliteVersion(),
  permissionModel: MODEL,
  factory: openSqlite,
  rssBytes: () => process.memoryUsage().rss,
  peakRssBytes: () => peakRssFromProc((path) => readFileSync(path, "utf8")),
  runWorkerProbe,
  permissionProbes: (workdir) =>
    Promise.all([
      attempt("fs-write", () => writeFileSync(`${workdir}/denied-write.txt`, "denied")),
      attempt(
        "net-connect",
        () =>
          new Promise((resolve, reject) => {
            const socket = connect({ host: "127.0.0.1", port: 9 })
            socket.once("connect", () => {
              socket.destroy()
              resolve(undefined)
            })
            socket.once("error", (error) => {
              socket.destroy()
              reject(error)
            })
          }),
      ),
      attempt("process-spawn", () => spawnSync("true")),
      attempt("fs-read-outside-scope", () => readFileSync("/etc/hostname", "utf8")),
    ]),
  installSigterm: (handler) => {
    process.once("SIGTERM", () => handler())
  },
  exit: (code) => {
    process.exit(code)
  },
  print: (line) => {
    process.stdout.write(`${line}\n`)
  },
  keepAlive: () => {
    setInterval(() => undefined, 1000)
  },
  readTextFile: (path) => readFileSync(path, "utf8"),
}
