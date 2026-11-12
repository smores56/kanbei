import { assertEquals } from "@std/assert"
import { Effect, Option } from "effect"
import { pointerFileLayer } from "../../src/adapters/pointer-file.ts"
import { PointerStore } from "../../src/candidates/pointer.ts"
import type { ScopePointer } from "../../src/candidates/pointer.ts"
import { withTempDir } from "../support.ts"

const pointer = (scope: string, generation: number): ScopePointer => ({
  scope,
  generation,
  candidateId: `candidate-${generation}`,
  candidateDigest: `sha256:${"a".repeat(64)}`,
  stateGeneration: generation,
  activatedAtUs: generation,
  activationReceipt: `receipt-${generation}`,
})

const encoder = new TextEncoder()

Deno.test("scope names that sanitize alike stay distinct pointer files", async () => {
  await withTempDir("pointer", async (root) => {
    const result = await Effect.runPromise(
      Effect.gen(function* () {
        const store = yield* PointerStore
        yield* store.publish(pointer("a/b", 1))
        yield* store.publish(pointer("a_b", 2))
        return {
          slash: yield* store.read("a/b"),
          underscore: yield* store.read("a_b"),
        }
      }).pipe(Effect.provide(pointerFileLayer(root))),
    )
    assertEquals(Option.getOrThrow(result.slash).generation, 1)
    assertEquals(Option.getOrThrow(result.underscore).generation, 2)
    assertEquals(
      [...Deno.readDirSync(root)].filter((entry) => entry.name.endsWith(".tmp")).length,
      0,
    )
  })
})

Deno.test("concurrent publishes to one scope leave one complete pointer", async () => {
  await withTempDir("pointer", async (root) => {
    const written = await Effect.runPromise(
      Effect.gen(function* () {
        const store = yield* PointerStore
        yield* Effect.forEach(
          Array.from({ length: 12 }, (_, index) => index + 1),
          (generation) => store.publish(pointer("agent-1", generation)),
          { concurrency: "unbounded" },
        )
        return yield* store.read("agent-1")
      }).pipe(Effect.provide(pointerFileLayer(root))),
    )
    const stored = Option.getOrThrow(written)
    assertEquals(Number.isInteger(stored.generation), true)
    const files = [...Deno.readDirSync(root)]
    assertEquals(files.length, 1)
    const text = await Deno.readTextFile(`${root}/${files[0]?.name ?? ""}`)
    assertEquals(JSON.parse(text).scope, "agent-1")
    assertEquals(encoder.encode(text).length > 0, true)
  })
})
