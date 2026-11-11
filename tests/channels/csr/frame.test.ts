import { assertEquals, assertThrows } from "@std/assert"
import { Result } from "effect"
import { decodeCsrFrame, encodeCsrFrame } from "../../../src/channels/csr/frame.ts"
import { parseCsrRequest } from "../../../src/channels/csr/schema.ts"

Deno.test("a CSR frame round-trips through the length prefix", () => {
  const value = { version: 1, commonName: "kanbei.sammohr.dev" }
  const decoded = decodeCsrFrame(encodeCsrFrame(value), 1_024)
  assertEquals(Result.isSuccess(decoded), true)
  if (Result.isSuccess(decoded)) assertEquals(decoded.success, value)
})

Deno.test("a frame over the bound is refused before JSON parsing", () => {
  const decoded = decodeCsrFrame(encodeCsrFrame({ value: "x".repeat(100) }), 32)
  assertEquals(Result.isFailure(decoded), true)
  if (Result.isFailure(decoded)) assertEquals(decoded.failure.code, "frame_too_large")
})

Deno.test("a declared length that does not match the bytes is refused", () => {
  const frame = encodeCsrFrame({ ok: true })
  const truncated = frame.subarray(0, frame.length - 2)
  const decoded = decodeCsrFrame(truncated, 1_024)
  assertEquals(Result.isFailure(decoded), true)
  if (Result.isFailure(decoded)) assertEquals(decoded.failure.code, "frame_invalid")
})

Deno.test("the CSR request schema refuses an extra field", () => {
  assertThrows(() =>
    parseCsrRequest({
      version: 1,
      commonName: "kanbei.sammohr.dev",
      keyAlgorithm: "ec-p256",
      sans: ["kanbei.sammohr.dev"],
      publicKeySpki: "AAAA",
      signature: "AAAA",
      extra: true,
    })
  )
})
