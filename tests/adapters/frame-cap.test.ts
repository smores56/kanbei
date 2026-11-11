import { assertEquals } from "@std/assert"
import { Option } from "effect"
import { CHAT_MAX_FRAME_BYTES, readChatFrame } from "../../src/adapters/chat-stream.ts"
import { readCsrFrame } from "../../src/adapters/csr-vsock.ts"

/** A real TCP pair: the header is written first, no body follows. */
const pair = async (): Promise<{ client: Deno.Conn; server: Deno.Conn; close: () => void }> => {
  const listener = Deno.listen({ hostname: "127.0.0.1", port: 0 })
  const port = (listener.addr as Deno.NetAddr).port
  const client = await Deno.connect({ hostname: "127.0.0.1", port })
  const server = await listener.accept()
  listener.close()
  return {
    client,
    server,
    close: () => {
      client.close()
      server.close()
    },
  }
}

const declaredLength = (length: number): Uint8Array => {
  const header = new Uint8Array(4)
  new DataView(header.buffer).setUint32(0, length, false)
  return header
}

Deno.test("chat-stream refuses a declared frame above the cap without reading a body", async () => {
  const sockets = await pair()
  try {
    await sockets.client.write(declaredLength(0xffff_ffff))
    const error = await readChatFrame(sockets.server, CHAT_MAX_FRAME_BYTES).then(
      () => undefined,
      (failure: unknown) => failure,
    )
    assertEquals(error instanceof Error, true)
    assertEquals((error as Error).message.includes("exceeds"), true)
  } finally {
    sockets.close()
  }
})

Deno.test("chat-stream reads an in-cap frame intact", async () => {
  const sockets = await pair()
  try {
    const body = new TextEncoder().encode('{"ok":true}')
    const frame = new Uint8Array(4 + body.length)
    new DataView(frame.buffer).setUint32(0, body.length, false)
    frame.set(body, 4)
    await sockets.client.write(frame)
    const read = await readChatFrame(sockets.server, CHAT_MAX_FRAME_BYTES)
    assertEquals(Option.isSome(read), true)
    if (Option.isSome(read)) assertEquals(read.value.length, frame.length)
  } finally {
    sockets.close()
  }
})

Deno.test("csr-vsock refuses a declared response above the cap without allocating it", async () => {
  const sockets = await pair()
  try {
    await sockets.client.write(declaredLength(0xffff_ffff))
    const error = await readCsrFrame(sockets.server, 4096).then(
      () => undefined,
      (failure: unknown) => failure,
    )
    assertEquals(error instanceof Error, true)
    assertEquals((error as Error).message.includes("outside 1..4096"), true)
  } finally {
    sockets.close()
  }
})

Deno.test("csr-vsock refuses a zero-length response", async () => {
  const sockets = await pair()
  try {
    await sockets.client.write(declaredLength(0))
    const error = await readCsrFrame(sockets.server, 4096).then(
      () => undefined,
      (failure: unknown) => failure,
    )
    assertEquals(error instanceof Error, true)
  } finally {
    sockets.close()
  }
})
