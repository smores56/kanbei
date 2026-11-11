import { Context, Data, Effect, Option } from "effect"

export class VaultIoError extends Data.TaggedError("VaultIoError")<{
  readonly operation: "lstat" | "read"
  readonly path: string
  readonly reason: string
}> {}

export interface VaultFileStat {
  readonly kind: "file" | "directory" | "symlink" | "other"
  readonly size: number
  readonly modifiedAtMs: number
  readonly hardLinks: number
  readonly dev: number
  readonly ino: number
}

export interface VaultFile {
  readonly bytes: Uint8Array
  readonly stat: VaultFileStat
}

/** Read-only vault access; there is deliberately no write operation here. */
export interface VaultFiles {
  readonly lstat: (
    relative: string,
  ) => Effect.Effect<Option.Option<VaultFileStat>, VaultIoError>
  readonly read: (relative: string) => Effect.Effect<VaultFile, VaultIoError>
}

export const VaultFiles = Context.Service<VaultFiles>("kanbei/channel/vault/Files")
