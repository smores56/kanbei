import { Effect, Layer, Option } from "effect"
import { TodoFiles, TodoIoError } from "../channels/todo/files.ts"
import type { TodoFile, TodoFileStat } from "../channels/todo/files.ts"

const kindOf = (info: Deno.FileInfo): TodoFileStat["kind"] => {
  if (info.isSymlink) return "symlink"
  if (info.isFile) return "file"
  if (info.isDirectory) return "directory"
  return "other"
}

const statOf = (info: Deno.FileInfo): TodoFileStat => ({
  kind: kindOf(info),
  size: info.size,
  modifiedAtMs: info.mtime?.getTime() ?? 0,
  dev: info.dev ?? 0,
  ino: info.ino ?? 0,
})

const io = (operation: "lstat" | "read" | "write", path: string, error: unknown): TodoIoError =>
  new TodoIoError({
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

const readOnce = async (root: string, relative: string): Promise<TodoFile> => {
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

const writeAll = async (
  file: Deno.FsFile,
  bytes: Uint8Array,
  offset: number,
): Promise<void> => {
  if (offset >= bytes.length) return
  const written = await file.write(bytes.subarray(offset))
  if (written === 0) throw new Error("short write")
  await writeAll(file, bytes, offset + written)
}

const writeAtomicOnce = async (
  root: string,
  relative: string,
  bytes: Uint8Array,
): Promise<void> => {
  const target = `${root}/${relative}`
  const temporary = `${root}/.${relative}.${crypto.randomUUID()}.tmp`
  try {
    const file = await Deno.open(temporary, { createNew: true, write: true, mode: 0o600 })
    try {
      await writeAll(file, bytes, 0)
      await file.sync()
    } finally {
      file.close()
    }
    await Deno.rename(temporary, target)
  } catch (error) {
    await Deno.remove(temporary).catch(() => {})
    throw error
  }
}

const baseName = (name: string): boolean => !name.includes("/") && !name.includes("\\")

/** Adapter for the capped Todo mount; the rename is the only visible mutation. */
export const todoFilesLayer = (root: string): Layer.Layer<TodoFiles> =>
  Layer.effect(
    TodoFiles,
    Effect.succeed({
      lstat: (relative: string) =>
        Effect.tryPromise({
          try: async () => {
            if (!baseName(relative)) return Option.none()
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
      writeAtomic: (relative: string, bytes: Uint8Array) =>
        Effect.tryPromise({
          try: () => writeAtomicOnce(root, relative, bytes),
          catch: (error) => io("write", relative, error),
        }),
    }),
  )
