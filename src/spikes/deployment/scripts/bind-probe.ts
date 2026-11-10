// Reports which TCP listeners a process can bind, one JSON line per family.
const emit = (value: unknown): void => {
  Deno.stdout.writeSync(new TextEncoder().encode(`${JSON.stringify(value)}\n`))
}

const results: Array<{ host: string; result: string; error?: string }> = []
for (const host of ["127.0.0.1", "::1"]) {
  try {
    const listener = Deno.listen({ hostname: host, port: 0 })
    listener.close()
    results.push({ host, result: "allowed" })
  } catch (error) {
    results.push({ host, result: "denied", error: String(error).slice(0, 120) })
  }
}
emit({ results })
