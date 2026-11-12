import type { PermissionProbe, RuntimeName } from "../schema.ts"
import type { DatabaseFactory } from "../sqlite.ts"
import type { WorkerProbeResult } from "./worker.ts"

/**
 * Everything the arm program needs from its host runtime.
 *
 * The arm program itself (`arm.ts`) is shared source: if a difference shows up
 * in the tables, it comes from the runtime behind this interface.
 */
export interface ArmHost {
  readonly runtime: RuntimeName
  readonly runtimeVersion: string
  readonly sqliteVersion: string
  readonly permissionModel: string
  readonly factory: DatabaseFactory
  readonly rssBytes: () => number
  readonly peakRssBytes: () => number
  readonly runWorkerProbe: (
    workerPath: string,
    runs: number,
    now: () => number,
  ) => Promise<WorkerProbeResult>
  readonly permissionProbes: (workdir: string) => Promise<ReadonlyArray<PermissionProbe>>
  readonly installSigterm: (handler: () => void) => void
  readonly exit: (code: number) => void
  readonly print: (line: string) => void
  readonly keepAlive: () => void
  readonly readTextFile: (path: string) => string
}

export const peakRssFromProc = (readFile: (path: string) => string): number => {
  const status = readFile("/proc/self/status")
  const line = status.split("\n").find((entry) => entry.startsWith("VmHWM:"))
  const kilobytes = line === undefined ? 0 : Number.parseInt(line.replace(/\D/gu, ""), 10)
  return Number.isFinite(kilobytes) ? kilobytes * 1024 : 0
}
