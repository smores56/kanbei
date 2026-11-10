// Allocates anonymous memory in 1 MiB steps so cgroup ceilings can be observed.
const emit = (value: unknown): void => {
  Deno.stdout.writeSync(new TextEncoder().encode(`${JSON.stringify(value)}\n`))
}

const megabytes = Number(Deno.args[0] ?? "400")
const pages: Array<Uint8Array> = []
for (let index = 0; index < megabytes; index += 1) {
  pages.push(new Uint8Array(1024 * 1024).fill(1))
}
emit({ allocatedMegabytes: pages.length })
