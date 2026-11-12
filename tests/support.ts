import { Effect } from "effect"
import { sha256Text } from "../src/evidence/digest.ts"

export const hash = (text: string): Promise<string> => Effect.runPromise(sha256Text(text))

export const withTempDir = async <A>(
  prefix: string,
  run: (directory: string) => Promise<A>,
): Promise<A> => {
  const directory = await Deno.makeTempDir({ prefix: `kanbei-${prefix}-` })
  try {
    return await run(directory)
  } finally {
    await Deno.remove(directory, { recursive: true })
  }
}
