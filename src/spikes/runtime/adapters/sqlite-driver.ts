import { DatabaseSync } from "node:sqlite"
import type { DatabaseFactory, SqlValue, StatementLike } from "../sqlite.ts"

/**
 * Boundary adapter: `node:sqlite` is the single SQLite binding for both arms.
 * Deno 2.9.2 implements the same module, so the bake-off isolates the runtime
 * rather than the driver.
 */
export const openSqlite: DatabaseFactory = (path, options) => {
  const database = options?.timeoutMs === undefined
    ? new DatabaseSync(path)
    : new DatabaseSync(path, { timeout: options.timeoutMs })

  const prepare = (sql: string): StatementLike => {
    const statement = database.prepare(sql)
    return {
      run: (...params: ReadonlyArray<SqlValue>) => {
        statement.run(...params)
      },
      get: (...params: ReadonlyArray<SqlValue>) => statement.get(...params),
      all: (...params: ReadonlyArray<SqlValue>) => statement.all(...params),
    }
  }

  return {
    exec: (sql) => {
      database.exec(sql)
    },
    prepare,
    close: () => {
      database.close()
    },
  }
}

export const sqliteVersion = (): string => {
  const database = new DatabaseSync(":memory:")
  try {
    const row = database.prepare("SELECT sqlite_version() AS version").get()
    const version = typeof row === "object" && row !== null ? Object.values(row)[0] : undefined
    return typeof version === "string" ? version : "unknown"
  } finally {
    database.close()
  }
}
