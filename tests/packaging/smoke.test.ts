import { assertEquals } from "@std/assert"
import { Effect } from "effect"
import {
  decryptSmoke,
  identityOf,
  runDecryptSmoke,
  SmokeError,
} from "../../src/cli/decrypt-smoke.ts"
import type { SmokeIo } from "../../src/cli/decrypt-smoke.ts"
import { encryptFile, makeKey } from "../channels/inbox/support.ts"
import { withTempDir } from "../support.ts"

const decoder = new TextDecoder()

Deno.test("the packaged decrypt path decrypts a real age batch and rejects garbage", async () => {
  await withTempDir("smoke", async (directory) => {
    const key = await makeKey(directory)
    await encryptFile('{"v":1,"kind":"batch"}', key.recipient, `${directory}/batch.age`)
    const ciphertext = await Deno.readFile(`${directory}/batch.age`)
    const plaintext = await Effect.runPromise(decryptSmoke(key.identityPath, ciphertext))
    assertEquals(decoder.decode(plaintext), '{"v":1,"kind":"batch"}')

    const refused = await Effect.runPromise(
      Effect.flip(decryptSmoke(key.identityPath, new Uint8Array([1, 2, 3, 4]))),
    )
    assertEquals(refused instanceof SmokeError, true)
  })
})

Deno.test("the smoke entrypoint requires an identity and writes only the plaintext", async () => {
  await withTempDir("smoke-run", async (directory) => {
    const key = await makeKey(directory)
    await encryptFile("hello", key.recipient, `${directory}/batch.age`)
    const ciphertext = await Deno.readFile(`${directory}/batch.age`)
    const written: Array<Uint8Array> = []
    const io: SmokeIo = {
      readStdin: Effect.succeed(ciphertext),
      writeStdout: (bytes) => Effect.sync(() => written.push(bytes)),
    }
    const code = await runDecryptSmoke(["--identity", key.identityPath], io)
    assertEquals(code, 0)
    assertEquals(written.map((bytes) => decoder.decode(bytes)), ["hello"])

    const missing = await Effect.runPromise(Effect.flip(identityOf([])))
    assertEquals(missing.reason.includes("--identity"), true)
  })
})
