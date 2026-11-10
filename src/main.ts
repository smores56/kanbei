import { Console, Effect } from "effect"

const program = Effect.gen(function* () {
  yield* Console.log("kanbei: bootstrap")
})

if (import.meta.main) {
  await Effect.runPromise(program)
}
