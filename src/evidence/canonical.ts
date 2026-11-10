import { Schema } from "effect"
import { CanonicalJsonError } from "./errors.ts"

export type Json = Schema.Json

const encodeString = (value: string): string => JSON.stringify(value)

const encodeNumber = (value: number): string => {
  if (!Number.isFinite(value)) {
    throw new CanonicalJsonError({ reason: "non-finite numbers are not JSON" })
  }
  if (Object.is(value, -0)) return "0"
  return JSON.stringify(value)
}

const encode = (value: Json, seen: Set<object>): string => {
  if (value === null) return "null"

  switch (typeof value) {
    case "boolean":
      return value ? "true" : "false"
    case "number":
      return encodeNumber(value)
    case "string":
      return encodeString(value)
    case "object": {
      if (seen.has(value)) {
        throw new CanonicalJsonError({ reason: "cyclic JSON value" })
      }

      seen.add(value)
      const encoded = Array.isArray(value)
        ? `[${value.map((item) => encode(item, seen)).join(",")}]`
        : `{${
          Object.entries(value)
            .toSorted(([left], [right]) => left.localeCompare(right))
            .map(([key, item]) => `${encodeString(key)}:${encode(item, seen)}`)
            .join(",")
        }}`
      seen.delete(value)
      return encoded
    }
  }
}

export const canonicalize = (value: Json): string => encode(value, new Set())

export const canonicalBytes = (value: Json): Uint8Array =>
  new TextEncoder().encode(canonicalize(value))

export const parseJson = (text: string): Json =>
  Schema.decodeUnknownSync(Schema.Json)(JSON.parse(text))
