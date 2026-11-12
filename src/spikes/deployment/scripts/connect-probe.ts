// Attempts a TCP connection and reports success or the error name.
const [host, portArg] = Deno.args
if (host === undefined || portArg === undefined) {
  throw new Error("usage: connect-probe.ts <host> <port>")
}

const emit = (value: unknown): void => {
  Deno.stdout.writeSync(new TextEncoder().encode(`${JSON.stringify(value)}\n`))
}

try {
  const connection = await Deno.connect({ hostname: host, port: Number(portArg) })
  connection.close()
  emit({ connected: true, error: "" })
} catch (error) {
  emit({ connected: false, error: String(error).slice(0, 160) })
}
