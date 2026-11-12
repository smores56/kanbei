import { Effect, Option } from "effect"
import type { Scope } from "effect"
import type { ChatStream } from "../channels/chat/stream.ts"
import { ChatTransportError } from "../channels/chat/stream.ts"

/** One frame may not ask the adapter to allocate more than this. */
export const CHAT_MAX_FRAME_BYTES = 256 * 1024

export interface TlsChatOptions {
  readonly hostname: string
  readonly port: number
  readonly caCerts?: ReadonlyArray<string>
  readonly maxFrameBytes?: number
}

const transport = (operation: "read" | "write" | "close", error: unknown): ChatTransportError =>
  new ChatTransportError({
    operation,
    reason: error instanceof Error ? error.message : String(error),
  })

const writeAll = async (
  connection: Deno.Conn,
  frame: Uint8Array,
  offset: number,
): Promise<void> => {
  if (offset >= frame.length) return
  const written = await connection.write(frame.subarray(offset))
  if (written === 0) throw new Error("short write")
  return writeAll(connection, frame, offset + written)
}

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

/**
 * The declared length is checked before the body buffer exists: a hostile peer
 * must not be able to make the adapter allocate a gigabyte.
 */
export const readChatFrame = async (
  connection: Deno.Conn,
  maxBytes: number,
): Promise<Option.Option<Uint8Array>> => {
  const header = await readExact(connection, 4)
  if (header === null) return Option.none()
  const length = new DataView(header.buffer, header.byteOffset, 4).getUint32(0, false)
  if (length > maxBytes) {
    throw new Error(`declared frame ${length} exceeds the ${maxBytes}-byte cap`)
  }
  const body = await readExact(connection, length)
  if (body === null) return Option.none()
  const frame = new Uint8Array(4 + body.length)
  frame.set(header, 0)
  frame.set(body, 4)
  return Option.some(frame)
}

/** Client side of the in-VM TLS listener; the relay is never in this path. */
export const connectTlsChatStream = (
  options: TlsChatOptions,
): Effect.Effect<ChatStream, ChatTransportError, Scope.Scope> =>
  Effect.acquireRelease(
    Effect.tryPromise({
      try: () =>
        Deno.connectTls({
          hostname: options.hostname,
          port: options.port,
          ...(options.caCerts === undefined ? {} : { caCerts: [...options.caCerts] }),
        }),
      catch: (error) => transport("read", error),
    }),
    (connection) =>
      Effect.sync(() => {
        try {
          connection.close()
        } catch {
          // Already closed by the client request; closing twice is harmless.
        }
      }),
  ).pipe(
    Effect.map((connection) => ({
      peer: "loopback" as const,
      readFrame: Effect.tryPromise({
        try: () => readChatFrame(connection, options.maxFrameBytes ?? CHAT_MAX_FRAME_BYTES),
        catch: (error) => transport("read", error),
      }),
      writeFrame: (frame: Uint8Array) =>
        Effect.tryPromise({
          try: () => writeAll(connection, frame, 0),
          catch: (error) => transport("write", error),
        }),
      close: Effect.try({
        try: () => connection.close(),
        catch: (error) => transport("close", error),
      }),
    })),
  )
