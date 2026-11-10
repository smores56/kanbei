/**
 * Runtime-neutral view of the ledger driver.
 *
 * Both arms use `node:sqlite` — Deno 2.9.2 ships the same module — so a
 * difference in the tables is a difference between the runtimes, not between
 * two different SQLite bindings. The `node:` import lives only in
 * `adapters/sqlite-driver.ts`.
 */
export type SqlValue = string | number | bigint | null | Uint8Array

export interface StatementLike {
  readonly run: (...params: ReadonlyArray<SqlValue>) => void
  readonly get: (...params: ReadonlyArray<SqlValue>) => unknown
  readonly all: (...params: ReadonlyArray<SqlValue>) => ReadonlyArray<unknown>
}

export interface DatabaseLike {
  readonly exec: (sql: string) => void
  readonly prepare: (sql: string) => StatementLike
  readonly close: () => void
}

export interface DatabaseOptions {
  /** SQLite busy-handler timeout. Omitted means "no busy handler": fail fast. */
  readonly timeoutMs?: number
}

export type DatabaseFactory = (path: string, options?: DatabaseOptions) => DatabaseLike

/** Reads the single column of a single-row query; undefined when the shape is unexpected. */
export const readScalar = (database: DatabaseLike, sql: string): unknown => {
  const row = database.prepare(sql).get()
  if (typeof row !== "object" || row === null) return undefined
  const values = Object.values(row)
  return values.length === 1 ? values[0] : values
}

export const readString = (database: DatabaseLike, sql: string): string => {
  const value = readScalar(database, sql)
  return typeof value === "string" ? value : String(value)
}

export const readNumber = (database: DatabaseLike, sql: string): number => {
  const value = readScalar(database, sql)
  return typeof value === "number" ? value : Number(value)
}
