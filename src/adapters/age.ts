import { Effect, Layer } from "effect"
import { AgeDecryptor, AgeError } from "../channels/inbox/age.ts"

export interface AgeDecryptorOptions {
  readonly identityPath: string
  readonly binary?: string
}

const decoder = new TextDecoder()

const decryptOnce = async (
  binary: string,
  identityPath: string,
  ciphertext: Uint8Array,
): Promise<Uint8Array> => {
  const child = new Deno.Command(binary, {
    args: ["--decrypt", "--identity", identityPath],
    stdin: "piped",
    stdout: "piped",
    stderr: "piped",
  }).spawn()
  const writer = child.stdin.getWriter()
  await writer.write(ciphertext)
  await writer.close()
  const [status, stdout, stderr] = await Promise.all([
    child.status,
    new Response(child.stdout).bytes(),
    new Response(child.stderr).bytes(),
  ])
  if (status.code !== 0) {
    const message = decoder.decode(stderr).trim()
    throw new Error(message === "" ? `age exited ${status.code}` : message)
  }
  return stdout
}

/** Calls `age --decrypt` through an adapter; the domain never shells out. */
export const ageDecryptorLayer = (
  options: AgeDecryptorOptions,
): Layer.Layer<AgeDecryptor> =>
  Layer.effect(
    AgeDecryptor,
    Effect.succeed({
      decrypt: (ciphertext: Uint8Array) =>
        Effect.tryPromise({
          try: () => decryptOnce(options.binary ?? "age", options.identityPath, ciphertext),
          catch: (error) =>
            new AgeError({
              operation: "decrypt",
              reason: error instanceof Error ? error.message : String(error),
            }),
        }),
    }),
  )
