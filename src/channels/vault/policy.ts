import { Result } from "effect"
import { VaultError } from "./errors.ts"

export interface VaultPolicy {
  readonly roots: ReadonlyArray<string>
  readonly maxBytes: number
}

export interface VaultSegments {
  readonly path: string
  readonly segments: ReadonlyArray<string>
}

const invalid = (path: string, detail: string): VaultError =>
  new VaultError({ code: "vault_path_invalid", path, detail })

/** Relative paths only: no absolute roots, no traversal, no NUL or separators. */
export const parseVaultPath = (
  path: string,
  policy: VaultPolicy,
): Result.Result<VaultSegments, VaultError> => {
  if (path === "") return Result.fail(invalid(path, "path is empty"))
  if (path.startsWith("/")) return Result.fail(invalid(path, "absolute path"))
  if (path.includes("\u0000")) return Result.fail(invalid(path, "NUL in path"))
  if (path.includes("\\")) return Result.fail(invalid(path, "backslash in path"))
  const segments = path.split("/")
  if (segments.some((segment) => segment === "" || segment === "." || segment === "..")) {
    return Result.fail(invalid(path, "empty, current, or parent segment"))
  }
  const roots = policy.roots.map((root) => root.replace(/^\/+|\/+$/gu, ""))
  const allowed = roots.some((root) => root === "" || path === root || path.startsWith(`${root}/`))
  if (!allowed) {
    return Result.fail(
      new VaultError({
        code: "vault_outside_allowlist",
        path,
        detail: `path is outside ${policy.roots.join(",")}`,
      }),
    )
  }
  return Result.succeed({ path, segments })
}

export const prefixesOf = (segments: ReadonlyArray<string>): ReadonlyArray<string> =>
  segments.map((_, index) => segments.slice(0, index + 1).join("/"))
