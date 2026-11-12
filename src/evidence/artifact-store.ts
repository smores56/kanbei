import { Context, Effect, FileSystem, Layer, Path } from "effect"
import { canonicalBytes } from "./canonical.ts"
import type { Json } from "./canonical.ts"
import { sha256 } from "./digest.ts"
import { ArtifactError } from "./errors.ts"

export type ContentDigest = string

export interface ArtifactStore {
  readonly put: (bytes: Uint8Array) => Effect.Effect<ContentDigest, ArtifactError>
  readonly putText: (text: string) => Effect.Effect<ContentDigest, ArtifactError>
  readonly putJson: (value: Json) => Effect.Effect<ContentDigest, ArtifactError>
  readonly read: (digest: ContentDigest) => Effect.Effect<Uint8Array, ArtifactError>
}

export const ArtifactStore = Context.Service<ArtifactStore>("kanbei/evidence/ArtifactStore")

const artifactPath = (path: Path.Path, root: string, digest: ContentDigest): string =>
  path.join(root, "sha256", digest.slice(7, 9), digest.slice(9))

const platformError = (
  operation: ArtifactError["operation"],
  path: string,
  error: unknown,
): ArtifactError =>
  new ArtifactError({
    operation,
    path,
    reason: error instanceof Error ? error.message : String(error),
  })

const make = (fs: FileSystem.FileSystem, path: Path.Path, root: string): ArtifactStore => {
  const put = (bytes: Uint8Array): Effect.Effect<ContentDigest, ArtifactError> =>
    Effect.gen(function* () {
      const digest = yield* sha256(bytes).pipe(
        Effect.mapError((error) =>
          new ArtifactError({ operation: "digest", path: root, reason: error.reason })
        ),
      )
      const target = artifactPath(path, root, digest)
      const exists = yield* fs
        .exists(target)
        .pipe(Effect.mapError((error) => platformError("read", target, error)))
      if (!exists) {
        yield* fs
          .makeDirectory(path.dirname(target), { recursive: true })
          .pipe(Effect.mapError((error) => platformError("mkdir", target, error)))
        yield* fs
          .writeFile(target, bytes)
          .pipe(Effect.mapError((error) => platformError("write", target, error)))
      }
      return digest
    })

  return {
    put,
    putText: (text) => put(new TextEncoder().encode(text)),
    putJson: (value) => put(canonicalBytes(value)),
    read: (digest) => {
      const target = artifactPath(path, root, digest)
      return fs
        .readFile(target)
        .pipe(Effect.mapError((error) => platformError("read", target, error)))
    },
  }
}

export const fileArtifactStoreLayer = (
  root: string,
): Layer.Layer<ArtifactStore, never, FileSystem.FileSystem | Path.Path> =>
  Layer.effect(
    ArtifactStore,
    Effect.gen(function* () {
      const fs = yield* FileSystem.FileSystem
      const path = yield* Path.Path
      return make(fs, path, root)
    }),
  )
