/**
 * S3 process-arm agent: a line protocol on stdin/stdout inside the sandbox.
 * It reads no `/proc` or `/sys`; all resource observation is supervisor-side.
 */
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

const decoder = new TextDecoder()

stdout(`READY ${Deno.pid}`)

const handle = (line: string): boolean => {
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
    case "SPAWN": {
      const marker = Deno.env.get("KANBEI_ISO_MARKER") ?? "kanbei-iso-orphan"
      const child = new Deno.Command("sh", {
        args: ["-c", "sleep 300", marker],
        stdin: "inherit",
        stdout: "piped",
        stderr: "piped",
      }).spawn()
      child.unref()
      stdout(`SPAWNED ${child.pid}`)
      return true
    }
    case "FORKBOMB": {
      const marker = Deno.env.get("KANBEI_ISO_MARKER") ?? "kanbei-iso-forkbomb"
      const script =
        `i=0; while [ $i -lt 200 ]; do ( sh -c 'sleep 30' '${marker}' & ); i=$((i+1)); done; wait`
      const child = new Deno.Command("sh", {
        args: ["-c", script],
        stdin: "inherit",
        stdout: "piped",
        stderr: "piped",
      }).spawn()
      child.unref()
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

for await (const chunk of Deno.stdin.readable) {
  const lines = decoder.decode(chunk, { stream: true }).split("\n")
  const stopped = lines.some((line) => {
    const trimmed = line.trim()
    return trimmed !== "" && !handle(trimmed)
  })
  if (stopped) break
}

export const AGENT_ENTRYPOINT = "kanbei/isolation/agent-process"
