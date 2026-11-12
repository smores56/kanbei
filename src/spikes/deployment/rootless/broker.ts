const [secretPath, socketPath, readyPath] = Deno.args
if (secretPath === undefined || socketPath === undefined || readyPath === undefined) {
  throw new Error("usage: broker.ts <secretPath> <socketPath> <readyPath>")
}

const secret = await Deno.open(secretPath, { read: true })
void secret
try {
  Deno.removeSync(socketPath)
} catch {
  // No stale socket.
}
const listener = Deno.listen({ transport: "unix", path: socketPath })
await Deno.writeTextFile(
  readyPath,
  JSON.stringify({ pid: Deno.pid, socket: socketPath, secretPath }),
)
for await (const connection of listener) {
  await connection.write(new TextEncoder().encode("ok\n"))
  connection.close()
}
