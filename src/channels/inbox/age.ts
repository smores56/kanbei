import { Context, Data } from "effect"
import type { Effect } from "effect"

export class AgeError extends Data.TaggedError("AgeError")<{
  readonly operation: "decrypt"
  readonly reason: string
}> {}

/** Decryption is the only age operation in the VM; there is no encrypt path. */
export interface AgeDecryptor {
  readonly decrypt: (ciphertext: Uint8Array) => Effect.Effect<Uint8Array, AgeError>
}

export const AgeDecryptor = Context.Service<AgeDecryptor>("kanbei/channel/inbox/AgeDecryptor")
