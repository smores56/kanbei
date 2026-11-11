import { Clock, Context, Effect, Layer, Option, Ref, Result } from "effect"
import { sha256 } from "../../evidence/digest.ts"
import type { ContentDigest } from "../../kernel/blob-sink.ts"
import { VaultError } from "./errors.ts"
import { VaultFiles } from "./files.ts"
import { parseVaultPath, prefixesOf } from "./policy.ts"
import type { VaultPolicy } from "./policy.ts"

export type { VaultPolicy } from "./policy.ts"

export interface VaultRead {
  readonly path: string
  readonly pathClass: "vault"
  readonly digest: ContentDigest
  readonly bytes: number
  readonly modifiedAt: string
}

export interface VaultReceipt {
  readonly kind: "vault_read"
  readonly path: string
  readonly digest: ContentDigest
  readonly bytes: number
  readonly at: string
}

export interface VaultReader {
  readonly read: (path: string) => Effect.Effect<VaultRead, VaultError>
  readonly receipts: Effect.Effect<ReadonlyArray<VaultReceipt>>
}

export const VaultReader = Context.Service<VaultReader>("kanbei/channel/vault/Reader")

const fail = (code: VaultError["code"], path: string, detail: string): VaultError =>
  new VaultError({ code, path, detail })

const configIssues = (policy: VaultPolicy): ReadonlyArray<string> => {
  const issues: Array<string> = []
  if (policy.roots.length === 0) issues.push("roots must not be empty")
  if (!Number.isFinite(policy.maxBytes) || policy.maxBytes <= 0) {
    issues.push("maxBytes must be positive")
  }
  return issues
}

/**
 * Read-only citation path. Every path component is checked with lstat, the
 * opened file must be the same inode as the checked one, and hardlinks are
 * refused so a vault file cannot alias a file outside the mount.
 */
export const vaultReaderLayer = (
  policy: VaultPolicy,
): Layer.Layer<VaultReader, never, VaultFiles> =>
  Layer.effect(
    VaultReader,
    Effect.gen(function* () {
      const issues = configIssues(policy)
      if (issues.length > 0) {
        return yield* Effect.die(new Error(`invalid vault policy: ${issues.join("; ")}`))
      }
      const files = yield* VaultFiles
      const receipts = yield* Ref.make<ReadonlyArray<VaultReceipt>>([])

      const read = (path: string): Effect.Effect<VaultRead, VaultError> =>
        Effect.gen(function* () {
          const parsed = parseVaultPath(path, policy)
          if (Result.isFailure(parsed)) return yield* Effect.fail(parsed.failure)
          const { segments } = parsed.success

          for (const prefix of prefixesOf(segments)) {
            const info = yield* files.lstat(prefix).pipe(
              Effect.mapError((error) => fail("vault_io", path, error.reason)),
            )
            if (Option.isNone(info)) {
              return yield* Effect.fail(fail("vault_not_found", path, `${prefix} is missing`))
            }
            if (info.value.kind === "symlink") {
              return yield* Effect.fail(fail("vault_symlink", path, `${prefix} is a symlink`))
            }
          }
          const checked = yield* files.lstat(path).pipe(
            Effect.mapError((error) => fail("vault_io", path, error.reason)),
          )
          if (Option.isNone(checked)) {
            return yield* Effect.fail(fail("vault_not_found", path, "file is missing"))
          }
          const stat = checked.value
          if (stat.kind !== "file") {
            return yield* Effect.fail(fail("vault_not_a_file", path, `kind ${stat.kind}`))
          }
          if (stat.hardLinks > 1) {
            return yield* Effect.fail(
              fail("vault_hardlink", path, `hardlink count ${stat.hardLinks}`),
            )
          }
          if (stat.size > policy.maxBytes) {
            return yield* Effect.fail(fail("vault_too_large", path, `${stat.size} bytes`))
          }

          const opened = yield* files.read(path).pipe(
            Effect.mapError((error) => fail("vault_io", path, error.reason)),
          )
          if (opened.stat.dev !== stat.dev || opened.stat.ino !== stat.ino) {
            return yield* Effect.fail(
              fail("vault_symlink", path, "opened file is not the checked inode"),
            )
          }
          if (opened.bytes.length > policy.maxBytes) {
            return yield* Effect.fail(fail("vault_too_large", path, `${opened.bytes.length} bytes`))
          }
          const digest = yield* sha256(opened.bytes).pipe(
            Effect.mapError((error) => fail("vault_io", path, error.reason)),
          )
          const now = yield* Clock.currentTimeMillis
          const receipt: VaultReceipt = {
            kind: "vault_read",
            path,
            digest,
            bytes: opened.bytes.length,
            at: new Date(now).toISOString(),
          }
          yield* Ref.update(receipts, (current) => [...current, receipt])
          return {
            path,
            pathClass: "vault",
            digest,
            bytes: opened.bytes.length,
            modifiedAt: new Date(opened.stat.modifiedAtMs).toISOString(),
          }
        })

      return { read, receipts: Ref.get(receipts) }
    }),
  )
