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

/**
 * Resolve the full path before opening: a symlink swapped in after the domain
 * lstat would otherwise escape the mount. The inode is re-checked after the
 * open, so a swap between realpath and open is also refused.
 */
const readOnce = async (root: string, realRoot: string, relative: string): Promise<VaultFile> => {
  const resolved = await Deno.realPath(`${root}/${relative}`)
  if (resolved !== realRoot && !resolved.startsWith(`${realRoot}/`)) {
    throw new Deno.errors.PermissionDenied(`${relative} resolves outside the vault mount`)
  }
  const file = await Deno.open(resolved, { read: true })
  try {
    const info = await file.stat()
    const stat = statOf(info)
    const checked = await Deno.lstat(resolved)
    if (checked.dev !== stat.dev || checked.ino !== stat.ino) {
      throw new Deno.errors.PermissionDenied(`${relative} changed inode between checks`)
    }
    const buffer = new Uint8Array(stat.size)
    const total = await readFully(file, buffer, 0)
    return { bytes: buffer.subarray(0, total), stat }
  } finally {
    file.close()
  }
}

/** Read-only adapter over the vault mount; there is no write method to leak. */
export const vaultFilesLayer = (root: string): Layer.Layer<VaultFiles, VaultIoError> =>
  Layer.effect(
    VaultFiles,
    Effect.gen(function* () {
      const realRoot = yield* Effect.tryPromise({
        try: () => Deno.realPath(root),
        catch: (error) =>
          new VaultIoError({
            operation: "lstat",
            path: root,
            reason: error instanceof Error ? error.message : String(error),
          }),
      })
      return {
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
            try: () => readOnce(root, realRoot, relative),
            catch: (error) => io("read", relative, error),
          }),
      }
    }),
  )
