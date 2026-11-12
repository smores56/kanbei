import { assertEquals } from "@std/assert"
import { Effect, Schema } from "effect"

Deno.test("effect and schema compose", async () => {
  const program = Effect.gen(function* () {
    const answer = yield* Schema.decodeUnknownEffect(Schema.Number)(42)
    return answer + 1
  })

  assertEquals(await Effect.runPromise(program), 43)
})
