import { Effect, Layer, Result } from "effect"
import { SearchChannel, SearchTransportError } from "../channels/search/client.ts"
import type { SearchQuery, SearchReply } from "../channels/search/schema.ts"
import { parseSearchReply } from "../channels/search/schema.ts"

const encoder = new TextEncoder()
const decoder = new TextDecoder()

const error = (reason: string): SearchTransportError => new SearchTransportError({ reason })

const readExact = async (
  connection: Deno.Conn,
  length: number,
  buffer = new Uint8Array(length),
  offset = 0,
): Promise<Uint8Array | null> => {
  if (offset >= length) return buffer
  const read = await connection.read(buffer.subarray(offset))
  if (read === null) return null
  return readExact(connection, length, buffer, offset + read)
}

const writeAll = async (
  connection: Deno.Conn,
  frame: Uint8Array,
  offset = 0,
): Promise<void> => {
  if (offset >= frame.length) return
  const written = await connection.write(frame.subarray(offset))
  if (written === 0) throw new Error("short write")
  return writeAll(connection, frame, offset + written)
}

const frameOf = (value: unknown): Uint8Array => {
  const payload = encoder.encode(JSON.stringify(value))
  const framed = new Uint8Array(4 + payload.length)
  new DataView(framed.buffer).setUint32(0, payload.length, false)
  framed.set(payload, 4)
  return framed
}

const exchangeOnce = async (
  path: string,
  query: SearchQuery,
  maxResponseBytes: number,
): Promise<SearchReply> => {
  const connection = await Deno.connect({ transport: "unix", path })
  try {
    await writeAll(connection, frameOf(query))
    const header = await readExact(connection, 4)
    if (header === null) throw new Error("search VM closed before a reply")
    const length = new DataView(header.buffer, header.byteOffset, 4).getUint32(0, false)
    if (length > maxResponseBytes) throw new Error(`reply ${length} > ${maxResponseBytes}`)
    const body = await readExact(connection, length)
    if (body === null) throw new Error("truncated search reply")
    const parsed = Result.try({
      try: () => parseSearchReply(JSON.parse(decoder.decode(body))),
      catch: (failure) =>
        failure instanceof Error ? failure.message.slice(0, 200) : String(failure),
    })
    if (Result.isFailure(parsed)) throw new Error(parsed.failure)
    return parsed.success
  } finally {
    connection.close()
  }
}

/** The only egress from Kanbei: one capped query frame to the Search VM. */
export const vsockSearchChannelLayer = (
  path: string,
  maxResponseBytes: number,
): Layer.Layer<SearchChannel> =>
  Layer.succeed(SearchChannel)({
    search: (query) =>
      Effect.tryPromise({
        try: () => exchangeOnce(path, query, maxResponseBytes),
        catch: (failure) => error(failure instanceof Error ? failure.message : String(failure)),
      }),
  })
