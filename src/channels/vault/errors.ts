import { Data } from "effect"

export type VaultErrorCode =
  | "vault_path_invalid"
  | "vault_outside_allowlist"
  | "vault_symlink"
  | "vault_hardlink"
  | "vault_too_large"
  | "vault_not_found"
  | "vault_not_a_file"
  | "vault_io"

export class VaultError extends Data.TaggedError("VaultError")<{
  readonly code: VaultErrorCode
  readonly path: string
  readonly detail: string
}> {}
