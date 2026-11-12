/* EXPERIMENTAL: the length-prefixed frame API is not the deployed chat contract.
 * The authority is the HTTP `POST /api/chat {message}` + bearer session surface in
 * `nix-config modules/lib/kanbei/ui/server.py`, implemented in `./http.ts`.
 * This frame API stays only for spike/property tests and must not be exposed.
 */
import { Data, Result, Schema } from "effect"

export type ChatFrameErrorCode = "frame_too_large" | "frame_invalid"

export class ChatFrameError extends Data.TaggedError("ChatFrameError")<{
  readonly code: ChatFrameErrorCode
  readonly detail: string
}> {}

const encoder = new TextEncoder()
const decoder = new TextDecoder()

/** One frame is a big-endian length prefix plus strict JSON, so no byte mode exists. */
export const encodeFrame = (value: unknown): Uint8Array => {
  const payload = encoder.encode(JSON.stringify(value))
  const framed = new Uint8Array(4 + payload.length)
  new DataView(framed.buffer).setUint32(0, payload.length, false)
  framed.set(payload, 4)
  return framed
}

export const decodeFrame = (
  bytes: Uint8Array,
  maxBytes: number,
): Result.Result<Schema.Json, ChatFrameError> => {
  if (bytes.length < 4) {
    return Result.fail(
      new ChatFrameError({ code: "frame_invalid", detail: "missing length prefix" }),
    )
  }
  const length = new DataView(bytes.buffer, bytes.byteOffset, 4).getUint32(0, false)
  if (length > maxBytes) {
    return Result.fail(
      new ChatFrameError({ code: "frame_too_large", detail: `${length} > ${maxBytes}` }),
    )
  }
  if (bytes.length !== 4 + length) {
    return Result.fail(
      new ChatFrameError({
        code: "frame_invalid",
        detail: `declared ${length} bytes, received ${bytes.length - 4}`,
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
      new ChatFrameError({
        code: "frame_invalid",
        detail: error instanceof Error ? error.message.slice(0, 200) : String(error),
      }),
    )
  }
}
