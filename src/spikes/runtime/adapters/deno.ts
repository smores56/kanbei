import { openSqlite, sqliteVersion } from "./sqlite-driver.ts"
import { peakRssFromProc } from "./types.ts"
import type { ArmHost } from "./types.ts"
import { runWorkerProbe } from "./worker.ts"
import type { PermissionProbe } from "../schema.ts"

const attempt = async (id: string, body: () => unknown): Promise<PermissionProbe> => {
  try {
    await body()
    return { id, denied: false, code: "ALLOWED", model: "deno-inner" }
  } catch (error) {
    const code = error instanceof Error ? error.name : String(error)
    return { id, denied: code === "NotCapable", code, model: "deno-inner" }
  }
}

/**
 * Deno arm host.
 *
 * The permission shard runs without `-A`, so every probe below is refused by the
 * inner capability model. `denied=true` means the runtime refused before the
 * effect, which is the fail-closed behavior the specification requires; Deno's
 * permissions are inner only, the OS sandbox stays mandatory.
 */
export const denoHost: ArmHost = {
  runtime: "deno",
  runtimeVersion: Deno.version.deno,
  sqliteVersion: sqliteVersion(),
  permissionModel: "deno-inner",
  factory: openSqlite,
  rssBytes: () => Deno.memoryUsage().rss,
  peakRssBytes: () => peakRssFromProc((path) => Deno.readTextFileSync(path)),
  runWorkerProbe,
  permissionProbes: (workdir) =>
    Promise.all([
      attempt("fs-write", () => Deno.writeTextFileSync(`${workdir}/denied-write.txt`, "denied")),
      attempt("net-connect", () => Deno.connect({ hostname: "127.0.0.1", port: 9 })),
      attempt("process-spawn", () => new Deno.Command("true").outputSync()),
      attempt("fs-read-outside-scope", () => Deno.readTextFileSync("/etc/hostname")),
    ]),
  installSigterm: (handler) => {
    Deno.addSignalListener("SIGTERM", handler)
  },
  exit: (code) => {
    Deno.exit(code)
  },
  print: (line) => {
    Deno.stdout.writeSync(new TextEncoder().encode(`${line}\n`))
  },
  keepAlive: () => {
    setInterval(() => undefined, 1000)
  },
  readTextFile: (path) => Deno.readTextFileSync(path),
}
