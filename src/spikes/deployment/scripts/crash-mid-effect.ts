// Models an effectful operation interrupted after intent was recorded. The
// deployment probe kills this process and checks that no completion marker exists.
const directory = Deno.args[0]
if (directory === undefined) throw new Error("usage: crash-mid-effect.ts <directory>")
await Deno.writeTextFile(`${directory}/intent`, "effect-intent\n")
await new Promise((resolve) => setTimeout(resolve, 60_000))
