/**
 * S3 process-arm agent: a line protocol on stdin/stdout inside the sandbox.
 * It reads no `/proc` or `/sys`; all resource observation is supervisor-side.
 */
import { forkBombScript, markerArgs, sleepForeverScript, WORKLOAD_OK } from "./agent-contract.ts"

const encoder = new TextEncoder()

const stdout = (line: string): void => {
  Deno.stdout.writeSync(encoder.encode(`${line}\n`))
}

const spin = (iterations: number): number => {
  let accumulator = 0
  for (let index = iterations; index > 0; index -= 1) {
    accumulator = (accumulator + index) % 1_000_003
  }
  return accumulator
}

const hangForever = (): never => {
  while (true) {
    spin(1_000_000)
  }
}

const holdAllocations = (mebibytes: number): number => {
  const chunks: Uint8Array[] = []
  for (let index = 0; index < mebibytes; index += 1) {
    const chunk = new Uint8Array(1024 * 1024)
    chunk.fill(1)
    chunks.push(chunk)
  }
  return chunks.length
}

const describeError = (error: unknown): string =>
  error instanceof Error ? `${error.name}: ${error.message}` : String(error)

/** Marker-carrying child; argv[0] survives exec so the supervisor can see it. */
const spawnMarkerChild = (marker: string, script: string): Deno.ChildProcess => {
  // The chroot has no `/dev`, so `stdio: "null"` fails exec with ENOENT; pipe
  // the child's output rather than letting it write into the agent protocol.
  const child = new Deno.Command("bash", {
    args: [...markerArgs(marker, script)],
    stdin: "inherit",
    stdout: "piped",
    stderr: "piped",
  }).spawn()
  child.unref()
  return child
}

/**
 * The four LANG-1 capability operations. Success reports exactly `WORKLOAD ok`;
 * a permission error, a missing endpoint we can reach, or a failed spawn is a
 * failure, never a pass.
 */
const workload = async (): Promise<string> => {
  try {
    const release = Deno.readTextFileSync("/etc/os-release")
    if (release.trim() === "") return "WORKLOAD fail /etc/os-release is empty"
  } catch (error) {
    return `WORKLOAD fail read /etc: ${describeError(error)}`
  }
  try {
    const payload = `kanbei-workload ${Deno.pid}`
    Deno.writeTextFileSync("/scratch/workload.txt", payload)
    if (Deno.readTextFileSync("/scratch/workload.txt") !== payload) {
      return "WORKLOAD fail /scratch/workload.txt did not round-trip"
    }
  } catch (error) {
    return `WORKLOAD fail write /scratch: ${describeError(error)}`
  }
  try {
    const status = new Deno.Command("true", {
      stdin: "inherit",
      stdout: "piped",
      stderr: "piped",
    }).outputSync()
    if (status.code !== 0) return `WORKLOAD fail spawn true exited ${status.code}`
  } catch (error) {
    return `WORKLOAD fail spawn true: ${describeError(error)}`
  }
  try {
    const connection = await Deno.connect({ hostname: "127.0.0.1", port: 9 })
    connection.close()
    return "WORKLOAD fail tcp 127.0.0.1:9 accepted a connection"
  } catch (error) {
    // Capability is present only when the connect reached the stack and the
    // kernel refused the absent endpoint; a permission denial is not that.
    if (!(error instanceof Deno.errors.ConnectionRefused)) {
      return `WORKLOAD fail tcp 127.0.0.1:9: ${describeError(error)}`
    }
  }
  return WORKLOAD_OK
}

const decoder = new TextDecoder()

stdout(`READY ${Deno.pid}`)

const handle = async (line: string): Promise<boolean> => {
  const [command = "", argument = "0"] = line.split(" ")
  switch (command) {
    case "PING":
      stdout("PONG")
      return true
    case "CPU": {
      const started = performance.now()
      const checksum = spin(Number(argument))
      stdout(`CPU ${checksum} ${(performance.now() - started).toFixed(2)}`)
      return true
    }
    case "ALLOC": {
      const held = holdAllocations(Number(argument))
      stdout(`ALLOCATED ${held}`)
      return true
    }
    case "WORKLOAD":
      stdout(await workload())
      return true
    case "SPAWN": {
      const marker = Deno.env.get("KANBEI_ISO_MARKER") ?? "kanbei-iso-orphan"
      const child = spawnMarkerChild(marker, sleepForeverScript(300))
      stdout(`SPAWNED ${child.pid}`)
      return true
    }
    case "FORKBOMB": {
      const marker = Deno.env.get("KANBEI_ISO_MARKER") ?? "kanbei-iso-forkbomb"
      const child = spawnMarkerChild(marker, forkBombScript(200))
      stdout(`FORKBOMB ${child.pid}`)
      return true
    }
    case "CRASH":
      Deno.kill(Deno.pid, "SIGKILL")
      return true
    case "HANG":
      return hangForever()
    case "EXIT":
      stdout("BYE")
      return false
    default:
      stdout(`UNKNOWN ${command}`)
      return true
  }
}

/** Lines are handled in order; a control line stops the agent. */
const handleLines = async (lines: ReadonlyArray<string>, index: number): Promise<boolean> => {
  const line = lines[index]
  if (line === undefined) return true
  const trimmed = line.trim()
  if (trimmed === "") return handleLines(lines, index + 1)
  if (!(await handle(trimmed))) return false
  return handleLines(lines, index + 1)
}

for await (const chunk of Deno.stdin.readable) {
  const lines = decoder.decode(chunk, { stream: true }).split("\n")
  if (!(await handleLines(lines, 0))) break
}

export const AGENT_ENTRYPOINT = "kanbei/isolation/agent-process"
