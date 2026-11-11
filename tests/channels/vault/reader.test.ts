import { assert, assertEquals } from "@std/assert"
import { Effect, Layer } from "effect"
import { vaultFilesLayer } from "../../../src/adapters/vault-files.ts"
import type { VaultPolicy, VaultReader } from "../../../src/channels/vault/reader.ts"
import {
  VaultReader as VaultReaderTag,
  vaultReaderLayer,
} from "../../../src/channels/vault/reader.ts"
import { VaultError } from "../../../src/channels/vault/errors.ts"
import { hash, withTempDir } from "../../support.ts"

const policy: VaultPolicy = { roots: [""], maxBytes: 1_024 }

interface Scenario {
  readonly root: string
  readonly reader: Layer.Layer<VaultReader>
}

const scenario = <A>(
  run: (context: Scenario) => Promise<A>,
  overrides: Partial<VaultPolicy> = {},
): Promise<A> =>
  withTempDir("vault", (root) => {
    const reader = vaultReaderLayer({ ...policy, ...overrides }).pipe(
      Layer.provide(vaultFilesLayer(root)),
    )
    return run({ root, reader })
  })

const read = (context: Scenario, path: string) =>
  Effect.runPromise(
    Effect.gen(function* () {
      const reader = yield* VaultReaderTag
      return yield* reader.read(path)
    }).pipe(Effect.provide(context.reader), Effect.orDie),
  )

const failure = async (context: Scenario, path: string): Promise<VaultError> => {
  const outcome = await Effect.runPromise(
    Effect.gen(function* () {
      const reader = yield* VaultReaderTag
      return yield* Effect.flip(reader.read(path))
    }).pipe(Effect.provide(context.reader), Effect.orDie),
  )
  return outcome
}

Deno.test("a vault read returns hashed bytes, size, time, and a receipt", async () => {
  await scenario(async (context) => {
    const content = "note body"
    await Deno.writeTextFile(`${context.root}/note.md`, content)
    const outcome = await Effect.runPromise(
      Effect.gen(function* () {
        const reader = yield* VaultReaderTag
        const result = yield* reader.read("note.md")
        return { result, receipts: yield* reader.receipts }
      }).pipe(Effect.provide(context.reader), Effect.orDie),
    )
    assertEquals(outcome.result.path, "note.md")
    assertEquals(outcome.result.digest, await hash(content))
    assertEquals(outcome.result.bytes, content.length)
    assertEquals(new TextDecoder().decode(outcome.result.content), content)
    assertEquals(outcome.result.pathClass, "vault")
    assert(outcome.result.modifiedAt.length > 0)
    assertEquals(outcome.receipts.length, 1)
    assertEquals(outcome.receipts[0]?.digest, outcome.result.digest)
  })
})

Deno.test("a symlink target cannot be followed", async () => {
  await scenario(async (context) => {
    await Deno.writeTextFile(`${context.root}/outside.md`, "secret")
    await Deno.symlink(`${context.root}/outside.md`, `${context.root}/link.md`)
    const error = await failure(context, "link.md")
    assertEquals(error.code, "vault_symlink")
    const state = await Effect.runPromise(
      Effect.gen(function* () {
        const reader = yield* VaultReaderTag
        return yield* reader.receipts
      }).pipe(Effect.provide(context.reader), Effect.orDie),
    )
    assertEquals(state.length, 0)
  })
})

Deno.test("a symlinked parent directory cannot be traversed", async () => {
  await scenario(async (context) => {
    await Deno.mkdir(`${context.root}/real`)
    await Deno.writeTextFile(`${context.root}/real/note.md`, "secret")
    await Deno.symlink(`${context.root}/real`, `${context.root}/alias`)
    const error = await failure(context, "alias/note.md")
    assertEquals(error.code, "vault_symlink")
  })
})

Deno.test("parent traversal, absolute paths, and malformed paths are refused", async () => {
  await scenario(async (context) => {
    const errors = await Promise.all(
      ["../outside", "a/../../b", "/etc/passwd", "a\\\\b", "a//b", ""].map((path) =>
        failure(context, path)
      ),
    )
    for (const error of errors) assertEquals(error.code, "vault_path_invalid")
  })
})

Deno.test("a hardlinked file is refused", async () => {
  await scenario(async (context) => {
    await Deno.writeTextFile(`${context.root}/original.md`, "linked")
    await Deno.link(`${context.root}/original.md`, `${context.root}/hard.md`)
    const error = await failure(context, "hard.md")
    assertEquals(error.code, "vault_hardlink")
  })
})

Deno.test("a file above the read cap is refused", async () => {
  await scenario(async (context) => {
    await Deno.writeTextFile(`${context.root}/big.md`, "x".repeat(policy.maxBytes + 1))
    const error = await failure(context, "big.md")
    assertEquals(error.code, "vault_too_large")
  })
})

Deno.test("a path outside the allowlist is refused", async () => {
  await scenario(
    async (context) => {
      await Deno.mkdir(`${context.root}/notes`)
      await Deno.mkdir(`${context.root}/secrets`)
      await Deno.writeTextFile(`${context.root}/notes/a.md`, "ok")
      await Deno.writeTextFile(`${context.root}/secrets/a.md`, "no")
      const allowed = await read(context, "notes/a.md")
      assertEquals(allowed.bytes, 2)
      const error = await failure(context, "secrets/a.md")
      assertEquals(error.code, "vault_outside_allowlist")
    },
    { roots: ["notes"] },
  )
})

Deno.test("a directory is not a readable vault file", async () => {
  await scenario(async (context) => {
    await Deno.mkdir(`${context.root}/folder`)
    const error = await failure(context, "folder")
    assertEquals(error.code, "vault_not_a_file")
  })
})

Deno.test("a missing file is reported, not fabricated", async () => {
  await scenario(async (context) => {
    const error = await failure(context, "missing.md")
    assertEquals(error.code, "vault_not_found")
  })
})

Deno.test("the vault reader exposes no write capability", async () => {
  await scenario(async (context) => {
    const reader = await Effect.runPromise(
      Effect.gen(function* () {
        return yield* VaultReaderTag
      }).pipe(Effect.provide(context.reader), Effect.orDie),
    )
    assertEquals(Object.keys(reader).toSorted(), ["read", "receipts"])
  })
})
