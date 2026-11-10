import { applyLandlock, emit } from "./landlock.ts"
import type { LandlockRule } from "./landlock.ts"

interface AgentConfig {
  readonly brokerPid: number
  readonly brokerSocket: string
  readonly allowedFile: string
  readonly deniedFile: string
  readonly writeAllowedDir: string
  readonly deniedWritePath: string
  readonly hostEscapePath: string
  readonly rules: ReadonlyArray<LandlockRule>
}

const read = (path: string): string => {
  try {
    return Deno.readTextFileSync(path)
  } catch (error) {
    return `ERROR:${String(error).slice(0, 120)}`
  }
}

const write = (path: string, value: string): boolean => {
  try {
    Deno.writeTextFileSync(path, value)
    return true
  } catch {
    return false
  }
}

const connectBroker = async (socket: string): Promise<{ ok: boolean; error: string }> => {
  try {
    const connection = await Deno.connect({ transport: "unix", path: socket })
    const buffer = new Uint8Array(16)
    const bytesRead = await connection.read(buffer)
    connection.close()
    const value = bytesRead !== null
      ? new TextDecoder().decode(buffer.subarray(0, bytesRead)).trim()
      : ""
    return { ok: value === "ok", error: value }
  } catch (error) {
    return { ok: false, error: String(error).slice(0, 160) }
  }
}

const systemdControl = async (): Promise<boolean> => {
  try {
    const child = new Deno.Command("/run/current-system/sw/bin/systemctl", {
      args: ["--user", "show-environment"],
      stdout: "piped",
      stderr: "piped",
    }).spawn()
    const timer = setTimeout(() => child.kill("SIGKILL"), 2000)
    const output = await child.output()
    clearTimeout(timer)
    return output.success
  } catch {
    return false
  }
}

const main = async (): Promise<void> => {
  const raw = Deno.env.get("KANBEI_A2B_CONFIG")
  if (raw === undefined) {
    emit({ fatal: "missing KANBEI_A2B_CONFIG" })
    return
  }
  const config = JSON.parse(raw) as AgentConfig
  const abi = applyLandlock(config.rules)

  const allowed = read(config.allowedFile)
  const denied = read(config.deniedFile)
  const allowedWrite = write(`${config.writeAllowedDir}/agent-write.txt`, "allowed\n")
  const deniedWrite = write(config.deniedWritePath, "denied\n")
  const brokerFd = read(`/proc/${config.brokerPid}/fd/3`)
  const brokerSignal = (() => {
    try {
      Deno.kill(config.brokerPid, "SIGTERM")
      return true
    } catch {
      return false
    }
  })()
  const brokerSocket = await connectBroker(config.brokerSocket)
  const systemd = await systemdControl()
  const procSelfRoot = read(config.hostEscapePath)

  emit({
    abi,
    allowedRead: allowed === "allowed\n",
    deniedReadBlocked: denied.startsWith("ERROR:"),
    allowedWrite,
    deniedWriteBlocked: !deniedWrite,
    brokerFdBlocked: brokerFd.startsWith("ERROR:"),
    brokerSignalBlocked: !brokerSignal,
    brokerSocketReachable: brokerSocket.ok,
    systemdControlBlocked: !systemd,
    procSelfRootBlocked: procSelfRoot.startsWith("ERROR:"),
    details: {
      deniedRead: denied.slice(0, 80),
      brokerFd: brokerFd.slice(0, 80),
      procSelfRoot: procSelfRoot.slice(0, 80),
      brokerSocket: brokerSocket.error,
    },
  })
}

await main()
