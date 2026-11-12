// Same-UID attacker used by the deployment spike. It never escalates: it only
// performs the reads and signals an ordinary process of the same user can attempt.
const [mode, pidArg, secretPath] = Deno.args
if (mode === undefined || pidArg === undefined) {
  throw new Error("usage: proc-attacker.ts <inspect|mem|fd|signal> <pid> [secretPath]")
}

const pid = Number(pidArg)

const emit = (value: unknown): void => {
  Deno.stdout.writeSync(new TextEncoder().encode(`${JSON.stringify(value)}\n`))
}

const read = async (target: string): Promise<string | null> => {
  try {
    return await Deno.readTextFile(target)
  } catch {
    return null
  }
}

const inspect = async () => {
  const cmdline = await read(`/proc/${pid}/cmdline`)
  const environ = await read(`/proc/${pid}/environ`)
  const maps = await read(`/proc/${pid}/maps`)
  const cgroup = await read(`/proc/${pid}/cgroup`)
  return {
    ok: true,
    cmdlineReadable: cmdline !== null,
    environReadable: environ !== null,
    environHasCanary: environ?.includes("KANBEI_DEP1_CANARY") ?? false,
    mapsReadable: maps !== null,
    cgroupReadable: cgroup !== null,
  }
}

const mem = async () => {
  try {
    const file = await Deno.open(`/proc/${pid}/mem`, { read: true })
    const buffer = new Uint8Array(1)
    await file.read(buffer)
    file.close()
    return { ok: true, opened: true, error: "" }
  } catch (error) {
    return { ok: true, opened: false, error: String(error).slice(0, 160) }
  }
}

const fd = async () => {
  const wanted = secretPath ?? ""
  try {
    const entries = [...Deno.readDirSync(`/proc/${pid}/fd`)]
    const matches = entries
      .map((entry) => ({
        entry: entry.name,
        target: Deno.readLinkSync(`/proc/${pid}/fd/${entry.name}`),
      }))
    const first = matches.find((candidate) => candidate.target === wanted)
    if (first === undefined) {
      return {
        ok: true,
        found: false,
        target: "",
        content: "",
        error: "no fd points at the secret path",
      }
    }
    const content = await Deno.readTextFile(`/proc/${pid}/fd/${first.entry}`)
    return { ok: true, found: true, target: first.target, content, error: "" }
  } catch (error) {
    return { ok: true, found: false, target: "", content: "", error: String(error).slice(0, 160) }
  }
}

const signal = () => {
  try {
    Deno.kill(pid, "SIGTERM")
    return { ok: true, delivered: true, error: "" }
  } catch (error) {
    return { ok: true, delivered: false, error: String(error).slice(0, 160) }
  }
}

const result = mode === "inspect"
  ? await inspect()
  : mode === "mem"
  ? await mem()
  : mode === "fd"
  ? await fd()
  : mode === "signal"
  ? signal()
  : { error: `unknown mode ${mode}` }

emit(result)
