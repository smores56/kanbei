// Holds a secret file open and reports its pid, modelling a broker that keeps a
// credential file descriptor alive. Runs as a child process of the probe runner.
const [secretPath, readyPath] = Deno.args
if (secretPath === undefined || readyPath === undefined) {
  throw new Error("usage: fd-holder.ts <secretPath> <readyPath>")
}

const file = await Deno.open(secretPath, { read: true })
void file

await Deno.writeTextFile(readyPath, JSON.stringify({ pid: Deno.pid }))
await new Promise((resolve) => setTimeout(resolve, 60_000))
