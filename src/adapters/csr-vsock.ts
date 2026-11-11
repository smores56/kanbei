import { Effect, Layer, Result } from "effect"
import {
  CSR_AUTH_PREFIX,
  CSR_MAX_FRAME_BYTES,
  CsrWireError,
  encodeCsrWireRequest,
  parseCsrWireResponse,
} from "../channels/csr/schema.ts"
import { CsrTransport, CsrTransportError } from "../channels/csr/client.ts"
import type { CsrTransportReply } from "../channels/csr/client.ts"

const TOKEN_PATTERN = /^[A-Za-z0-9._-]{32,128}$/u
const encoder = new TextEncoder()

export interface CsrVsockOptions {
  readonly path: string
  readonly token: string
  readonly maxResponseBytes?: number
}

const transportError = (reason: string): CsrTransportError => new CsrTransportError({ reason })

const writeAll = async (
  connection: Deno.Conn,
  bytes: Uint8Array,
  offset = 0,
): Promise<void> => {
  if (offset >= bytes.length) return
  const written = await connection.write(bytes.subarray(offset))
  if (written === 0) throw new Error("short write")
  return writeAll(connection, bytes, offset + written)
}

/**
 * The declared length is checked before the body buffer exists: a hostile host
 * must not be able to make the adapter allocate a gigabyte.
 */
export const readCsrFrame = async (
  connection: Deno.Conn,
  maxBytes: number,
): Promise<Uint8Array> => {
  const header = await readExact(connection, 4)
  const length = new DataView(header.buffer, header.byteOffset, 4).getUint32(0, false)
  if (length === 0 || length > maxBytes) {
    throw new Error(`response ${length} outside 1..${maxBytes}`)
  }
  return readExact(connection, length)
}

const readExact = async (
  connection: Deno.Conn,
  length: number,
  buffer = new Uint8Array(length),
  offset = 0,
): Promise<Uint8Array> => {
  if (offset >= length) return buffer
  const read = await connection.read(buffer.subarray(offset))
  if (read === null) throw new Error("host closed before a response")
  return readExact(connection, length, buffer, offset + read)
}

const frameOf = (payload: Uint8Array): Uint8Array => {
  const framed = new Uint8Array(4 + payload.length)
  new DataView(framed.buffer).setUint32(0, payload.length, false)
  framed.set(payload, 4)
  return framed
}

const exchangeOnce = async (
  options: CsrVsockOptions,
  csrPem: string,
  maxResponseBytes: number,
): Promise<CsrTransportReply> => {
  if (!TOKEN_PATTERN.test(options.token)) {
    throw new Error("csr token is not 32..128 URL-safe characters")
  }
  const connection = await Deno.connect({ transport: "unix", path: options.path })
  try {
    await writeAll(connection, encoder.encode(`${CSR_AUTH_PREFIX}${options.token}\n`))
    await writeAll(connection, frameOf(encodeCsrWireRequest(csrPem)))
    const body = await readCsrFrame(connection, maxResponseBytes)
    const parsed = Result.try({
      try: () => parseCsrWireResponse(JSON.parse(new TextDecoder().decode(body))),
      catch: (failure) =>
        failure instanceof Error ? failure.message.slice(0, 200) : String(failure),
    })
    if (Result.isFailure(parsed)) throw new Error(`malformed csr response: ${parsed.failure}`)
    if ("error" in parsed.success) {
      const failure: CsrWireError = parsed.success
      throw new Error(`host refused the certificate request: ${failure.error}`)
    }
    return { chain: parsed.success.chain, notAfter: parsed.success.not_after }
  } finally {
    connection.close()
  }
}

/** The host ACME responder is the only peer; the in-VM path is a vsock UDS. */
export const vsockCsrTransportLayer = (
  options: CsrVsockOptions,
): Layer.Layer<CsrTransport> =>
  Layer.succeed(CsrTransport)({
    exchange: (csrPem) =>
      Effect.tryPromise({
        try: () =>
          exchangeOnce(
            options,
            csrPem,
            options.maxResponseBytes ?? CSR_MAX_FRAME_BYTES,
          ),
        catch: (error) => transportError(error instanceof Error ? error.message : String(error)),
      }),
  })
