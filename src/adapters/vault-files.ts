import { Effect, Layer, Option } from "effect"
import { VaultFiles, VaultIoError } from "../channels/vault/files.ts"
import type { VaultFile, VaultFileStat } from "../channels/vault/files.ts"

const kindOf = (info: Deno.FileInfo): VaultFileStat["kind"] => {
  if (info.isSymlink) return "symlink"
  if (info.isFile) return "file"
  if (info.isDirectory) return "directory"
  return "other"
}

const statOf = (info: Deno.FileInfo): VaultFileStat => ({
  kind: kindOf(info),
  size: info.size,
  modifiedAtMs: info.mtime?.getTime() ?? 0,
  hardLinks: info.nlink ?? 1,
  dev: info.dev ?? 0,
  ino: info.ino ?? 0,
})

const io = (operation: "lstat" | "read", path: string, error: unknown): VaultIoError =>
  new VaultIoError({
    operation,
    path,
    reason: error instanceof Error ? error.message : String(error),
  })

const readFully = async (
  file: Deno.FsFile,
  buffer: Uint8Array,
  offset: number,
): Promise<number> => {
  const read = await file.read(buffer.subarray(offset))
  if (read === null || read === 0) return offset
  return readFully(file, buffer, offset + read)
}

const readOnce = async (root: string, relative: string): Promise<VaultFile> => {
  // `lstat` in the domain proves the path is not a symlink; opening and then
  // comparing fstat identity closes the swap window between those two steps.
  const file = await Deno.open(`${root}/${relative}`, { read: true })
  try {
    const info = await file.stat()
    const stat = statOf(info)
    const buffer = new Uint8Array(stat.size)
    const total = await readFully(file, buffer, 0)
    return { bytes: buffer.subarray(0, total), stat }
  } finally {
    file.close()
  }
}

/** Read-only adapter over the vault mount; there is no write method to leak. */
export const vaultFilesLayer = (root: string): Layer.Layer<VaultFiles> =>
  Layer.effect(
    VaultFiles,
    Effect.succeed({
      lstat: (relative: string) =>
        Effect.tryPromise({
          try: async () => {
            try {
              return Option.some(statOf(await Deno.lstat(`${root}/${relative}`)))
            } catch (error) {
              if (error instanceof Deno.errors.NotFound) return Option.none()
              throw error
            }
          },
          catch: (error) => io("lstat", relative, error),
        }),
      read: (relative: string) =>
        Effect.tryPromise({
          try: () => readOnce(root, relative),
          catch: (error) => io("read", relative, error),
        }),
    }),
  )
