import { resolveRuntime, RUNTIME_REQUESTS } from "../resolve.ts"
import type { ArmDefinition, PreparedProgram, ProbeOptions } from "./contract.ts"
import { commonEnv, runTimeoutMs } from "./contract.ts"
import type { AvailableRuntime } from "./contract.ts"

const probeSource = String.raw`
const isDeno = typeof Deno !== "undefined"

const readEnv = (name) => {
  try {
    if (isDeno) return Deno.env.get(name) ?? null
    return process.env[name] ?? null
  } catch {
    return null
  }
}

const now = () => performance.now()
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms))
const categoryOf = (i) => (i % 3 === 0 ? "alpha" : i % 3 === 1 ? "beta" : "gamma")
const amountOf = (i) => ((i * 37) % 100) + 1

const denied = (error) => ({
  attempted: true,
  allowed: false,
  errorClass: (error && error.name) || "Error",
  detail: String((error && error.message) || error).slice(0, 200),
})

const readFile = async (path) => {
  if (isDeno) return await Deno.readTextFile(path)
  const fs = await import("node:fs/promises")
  return await fs.readFile(path, "utf8")
}

const writeFile = async (path, text) => {
  if (isDeno) return await Deno.writeTextFile(path, text)
  const fs = await import("node:fs/promises")
  return await fs.writeFile(path, text)
}

const fileWriteProbe = async (path) => {
  if (!path) return { attempted: false, allowed: false, errorClass: "MissingInput" }
  try {
    await writeFile(path, "derived")
    const back = await readFile(path)
    return { attempted: true, allowed: back === "derived", detail: "scratch write/read" }
  } catch (error) {
    return denied(error)
  }
}

const fileReadProbe = async (path) => {
  if (!path) return { attempted: false, allowed: false, errorClass: "MissingInput" }
  try {
    const contents = await readFile(path)
    return { attempted: true, allowed: contents.length > 0, detail: "canary file readable" }
  } catch (error) {
    return denied(error)
  }
}

const processProbe = async () => {
  try {
    if (isDeno) {
      new Deno.Command("true").outputSync()
    } else {
      const childProcess = await import("node:child_process")
      const result = childProcess.spawnSync("true")
      if (result.error) throw result.error
    }
    return { attempted: true, allowed: true }
  } catch (error) {
    return denied(error)
  }
}

const networkProbe = async () => {
  try {
    if (isDeno) {
      const connection = await Deno.connect({ hostname: "127.0.0.1", port: 1 })
      connection.close()
      return { attempted: true, allowed: true, detail: "loopback connect succeeded" }
    }
    const net = await import("node:net")
    await new Promise((resolve) => {
      const socket = net.connect({ host: "127.0.0.1", port: 1 })
      const done = () => {
        socket.destroy()
        resolve()
      }
      socket.on("connect", done)
      socket.on("error", done)
      setTimeout(done, 250)
    })
    return { attempted: true, allowed: true, detail: "socket capability present; target refused" }
  } catch (error) {
    const name = (error && error.name) || "Error"
    const permissionDenied = name === "NotCapable" || String(name).indexOf("Permission") >= 0
    return {
      attempted: true,
      allowed: !permissionDenied,
      errorClass: name,
      detail: permissionDenied ? "permission denied" : "connect failed without a policy denial",
    }
  }
}

const dependencyProbe = async () => {
  const modules = []
  for (const name of ["node:path", "node:crypto", "undici", "axios"]) {
    try {
      await import(name)
      modules.push({ name, ok: true })
    } catch {
      modules.push({ name, ok: false })
    }
  }
  return { ok: modules.every((entry) => entry.ok), modules }
}

const main = async () => {
  const latency = Number(readEnv("KANBEI_LATENCY_MS") || "10")
  const calls = Number(readEnv("KANBEI_CALLS") || "50")

  let expressionValue = 0
  for (let i = 0; i < 10; i += 1) expressionValue += i * i
  const expression = { ok: expressionValue === 285, value: expressionValue }

  const items = []
  const overheads = []
  const start = now()
  for (let i = 0; i < calls; i += 1) {
    const callStart = now()
    await sleep(latency)
    overheads.push(Math.max(0, now() - callStart - latency))
    items.push({ index: i, category: categoryOf(i), amount: amountOf(i) })
  }
  const totalMs = now() - start

  const kept = items.filter((item) => item.amount > 50)
  const sums = { alpha: 0, beta: 0, gamma: 0 }
  for (const item of kept) sums[item.category] = (sums[item.category] || 0) + item.amount
  const groups = Object.keys(sums).sort().map((key) => [key, sums[key]])
  const top = kept
    .slice()
    .sort((a, b) => (b.amount - a.amount) || (a.index - b.index))
    .slice(0, 3)
    .map((item) => [item.index, item.amount])
  const checksum = kept.reduce((total, item) => total + item.amount, 0)

  let streamEvents = 0
  for (let i = 0; i < 4; i += 1) {
    streamEvents += 1
    console.log("progress: " + String(streamEvents))
  }

  let retryAttempts = 0
  let retryValue = 0
  while (retryAttempts < 4) {
    retryAttempts += 1
    if (retryAttempts >= 3) {
      retryValue = 42
      break
    }
  }

  const canary = readEnv("KANBEI_BAKEOFF_CANARY")
  const envRead = {
    attempted: true,
    allowed: canary !== null && canary.length > 0,
    ...(canary !== null && canary.length > 0
      ? { detail: "canary env var readable" }
      : { errorClass: "DeniedOrUnset" }),
  }

  const result = {
    probeVersion: 1,
    expression,
    fanout: {
      ok: true,
      calls,
      checksum,
      groups,
      top,
      callOverheadsMs: overheads,
      totalMs,
    },
    stream: { events: streamEvents },
    retry: { attempts: retryAttempts, ok: retryValue === 42 && retryAttempts === 3 },
    fileWrite: await fileWriteProbe(readEnv("KANBEI_SCRATCH_FILE")),
    envRead,
    processSpawn: await processProbe(),
    network: await networkProbe(),
    secretFileRead: await fileReadProbe(readEnv("KANBEI_CANARY_FILE")),
    dependencyImport: await dependencyProbe(),
    snapshot: {
      supported: false,
      ok: false,
      detail: "no execution-state snapshot API in the JS runtime",
    },
    approvalPause: {
      supported: false,
      ok: false,
      detail: "no pause/resume primitive; approval requires host replay",
    },
  }
  console.log("KANBEI_PROBE_JSON:" + JSON.stringify(result))
}

main().catch((error) => {
  console.error("probe failed: " + String(error))
  if (isDeno) Deno.exit(1)
  process.exit(1)
})
`

const prepared = (runtime: AvailableRuntime, options: ProbeOptions): PreparedProgram => ({
  files: [{ name: "probe.mjs", text: probeSource }],
  argv: (directory) =>
    runtime.variant === "deno"
      ? [
        runtime.binaryPath,
        "run",
        "--quiet",
        "--no-config",
        "--allow-env=KANBEI_LATENCY_MS,KANBEI_CALLS,KANBEI_SCRATCH,KANBEI_SCRATCH_FILE,KANBEI_CANARY_FILE,KANBEI_HOST_CALL_BUDGET",
        `--allow-read=${options.scratchDir}`,
        `--allow-write=${options.scratchDir}`,
        `${directory}/probe.mjs`,
      ]
      : [runtime.binaryPath, `${directory}/probe.mjs`],
  env: () => commonEnv(options),
  latencySimulation: "sleep",
  timeoutMs: runTimeoutMs(options),
})

const startupPrepared = (runtime: AvailableRuntime): PreparedProgram => ({
  files: [{ name: "startup.mjs", text: 'console.log("ok")' }],
  argv: (directory) =>
    runtime.variant === "deno"
      ? [runtime.binaryPath, "run", "--quiet", "--no-config", `${directory}/startup.mjs`]
      : [runtime.binaryPath, `${directory}/startup.mjs`],
  env: () => ({}),
  latencySimulation: "sleep",
  timeoutMs: 30_000,
})

export const typescriptDenoArm: ArmDefinition = {
  arm: "typescript",
  variant: "deno",
  resolve: () => resolveRuntime(RUNTIME_REQUESTS.deno),
  prepare: prepared,
  startup: startupPrepared,
}

export const typescriptNodeArm: ArmDefinition = {
  arm: "typescript",
  variant: "node",
  resolve: () => resolveRuntime(RUNTIME_REQUESTS.node),
  prepare: prepared,
  startup: startupPrepared,
}

export const javascriptArms: ReadonlyArray<ArmDefinition> = [typescriptDenoArm, typescriptNodeArm]
