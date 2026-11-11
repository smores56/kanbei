import { Data, Result, Schema } from "effect"

export class CsrFrameError extends Data.TaggedError("CsrFrameError")<{
  readonly code: "frame_too_large" | "frame_invalid"
  readonly detail: string
}> {}

const encoder = new TextEncoder()
const decoder = new TextDecoder()

/** The CSR channel is length-prefixed JSON with strict keys, never TLS or DER bytes. */
export const encodeCsrFrame = (value: unknown): Uint8Array => {
  const payload = encoder.encode(JSON.stringify(value))
  const framed = new Uint8Array(4 + payload.length)
  new DataView(framed.buffer).setUint32(0, payload.length, false)
  framed.set(payload, 4)
  return framed
}

export const decodeCsrFrame = (
  bytes: Uint8Array,
  maxBytes: number,
): Result.Result<Schema.Json, CsrFrameError> => {
  if (bytes.length < 4) {
    return Result.fail(
      new CsrFrameError({ code: "frame_invalid", detail: "missing length prefix" }),
    )
  }
  const length = new DataView(bytes.buffer, bytes.byteOffset, 4).getUint32(0, false)
  if (length > maxBytes) {
    return Result.fail(
      new CsrFrameError({ code: "frame_too_large", detail: `${length} > ${maxBytes}` }),
    )
  }
  if (bytes.length !== 4 + length) {
    return Result.fail(
      new CsrFrameError({
        code: "frame_invalid",
        detail: `declared ${length}, received ${bytes.length - 4}`,
      }),
    )
  }
  try {
    const parsed = Schema.decodeUnknownSync(Schema.Json)(
      JSON.parse(decoder.decode(bytes.subarray(4))),
    )
    return Result.succeed(parsed)
  } catch (error) {
    return Result.fail(
      new CsrFrameError({
        code: "frame_invalid",
        detail: error instanceof Error ? error.message.slice(0, 200) : String(error),
      }),
    )
  }
}
