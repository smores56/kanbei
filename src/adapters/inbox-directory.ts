import { Effect, Layer } from "effect"
import { InboxDirectory, InboxDirectoryError } from "../channels/inbox/directory.ts"
import type { InboxFile } from "../channels/inbox/directory.ts"

const baseName = (name: string): boolean =>
  !name.includes("/") && !name.includes("\\") && name !== ".."

const listFiles = (root: string): ReadonlyArray<InboxFile> =>
  [...Deno.readDirSync(root)]
    .filter((entry) => entry.isFile)
    .map((entry) => ({
      name: entry.name,
      size: Deno.statSync(`${root}/${entry.name}`).size,
    }))

/** Read-only adapter for the Inbox mount. Names are base names only. */
export const directoryLayer = (root: string): Layer.Layer<InboxDirectory> =>
  Layer.effect(
    InboxDirectory,
    Effect.succeed({
      list: Effect.tryPromise({
        try: () => Promise.resolve(listFiles(root)),
        catch: (error) =>
          new InboxDirectoryError({
            operation: "list",
            name: root,
            reason: error instanceof Error ? error.message : String(error),
          }),
      }),
      read: (name: string) => {
        if (!baseName(name)) {
          return Effect.fail(
            new InboxDirectoryError({
              operation: "read",
              name,
              reason: "name is not a base name",
            }),
          )
        }
        return Effect.tryPromise({
          try: () => Deno.readFile(`${root}/${name}`),
          catch: (error) =>
            new InboxDirectoryError({
              operation: "read",
              name,
              reason: error instanceof Error ? error.message : String(error),
            }),
        })
      },
    }),
  )
