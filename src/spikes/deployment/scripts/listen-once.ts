// Binds one address and reports the port, then serves a single connection.
const [host, readyPath] = Deno.args
if (host === undefined || readyPath === undefined) {
  throw new Error("usage: listen-once.ts <host> <ready>")
}

const listener = Deno.listen({ hostname: host, port: 0 })
const port = listener.addr.transport === "tcp" ? listener.addr.port : 0
await Deno.writeTextFile(readyPath, JSON.stringify({ port }))
const timer = setTimeout(() => listener.close(), 8_000)
try {
  for await (const connection of listener) {
    connection.close()
    break
  }
} catch {
  // listener closed by the timer
} finally {
  clearTimeout(timer)
  try {
    listener.close()
  } catch {
    // already closed
  }
}
