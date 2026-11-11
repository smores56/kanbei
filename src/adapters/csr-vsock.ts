import { Effect, Layer, Result } from "effect"
import { decodeCsrFrame, encodeCsrFrame } from "../channels/csr/frame.ts"
import type { CsrRequest, HostResponse } from "../channels/csr/schema.ts"
import { parseHostResponse } from "../channels/csr/schema.ts"
import { CsrTransport, CsrTransportError } from "../channels/csr/client.ts"

const transportError = (reason: string): CsrTransportError => new CsrTransportError({ reason })

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

const exchangeOnce = async (
  path: string,
  request: CsrRequest,
  maxResponseBytes: number,
): Promise<HostResponse> => {
  const connection = await Deno.connect({ transport: "unix", path })
  try {
    await writeAll(connection, encodeCsrFrame(request))
    const header = await readExact(connection, 4)
    if (header === null) throw new Error("host closed before a response")
    const length = new DataView(header.buffer, header.byteOffset, 4).getUint32(0, false)
    const body = await readExact(connection, length)
    if (body === null) throw new Error("truncated host response")
    const frame = new Uint8Array(4 + body.length)
    frame.set(header, 0)
    frame.set(body, 4)
    const decoded = decodeCsrFrame(frame, maxResponseBytes)
    if (Result.isFailure(decoded)) throw new Error(decoded.failure.detail)
    return parseHostResponse(decoded.success)
  } finally {
    connection.close()
  }
}

/** The host ACME writer is the only peer; the in-VM path is a vsock UDS. */
export const vsockCsrTransportLayer = (
  path: string,
  maxResponseBytes: number,
): Layer.Layer<CsrTransport> =>
  Layer.succeed(CsrTransport)({
    exchange: (request) =>
      Effect.tryPromise({
        try: () => exchangeOnce(path, request, maxResponseBytes),
        catch: (error) => transportError(error instanceof Error ? error.message : String(error)),
      }),
  })
