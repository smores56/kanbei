import { assertEquals } from "@std/assert"
import { Effect } from "effect"
import {
  parseCertificatePem,
  parseCsrPem,
  verifyIssuedChain,
} from "../../../src/channels/csr/pkcs10.ts"
import { CsrTransport } from "../../../src/channels/csr/client.ts"
import { vsockCsrTransportLayer } from "../../../src/adapters/csr-vsock.ts"
import { withTempDir } from "../../support.ts"

const root = new URL("../../../fixtures/csr-v1", import.meta.url).pathname
const NOW = Date.parse("2026-11-11T00:00:00.000Z")
const TOKEN = "csr-token-0123456789abcdef-0123456789"

interface HostParser {
  readonly csr: { readonly cn: string; readonly key_type: string; readonly key_bits: number }
  readonly cert: { readonly cn: string; readonly not_after: string; readonly spki: string }
  readonly chain_blocks: number
}

const host = JSON.parse(Deno.readTextFileSync(`${root}/host-parser.json`)) as HostParser

const hex = (bytes: Uint8Array): string =>
  Array.from(bytes, (byte) => byte.toString(16).padStart(2, "0")).join("")

const concat = (left: Uint8Array, right: Uint8Array): Uint8Array => {
  const out = new Uint8Array(left.length + right.length)
  out.set(left)
  out.set(right, left.length)
  return out
}

/** Read one auth line plus one length-prefixed JSON frame from a raw socket. */
const readAuthAndFrame = async (
  connection: Deno.Conn,
): Promise<{ readonly auth: string; readonly frame: unknown }> => {
  let bytes: Uint8Array = new Uint8Array(0)
  const ready = (): { auth: string; body: string } | undefined => {
    const newline = bytes.indexOf(0x0a)
    if (newline === -1) return undefined
    const after = newline + 1
    if (bytes.length < after + 4) return undefined
    const length = new DataView(bytes.buffer, bytes.byteOffset + after, 4).getUint32(0, false)
    if (bytes.length < after + 4 + length) return undefined
    return {
      auth: new TextDecoder().decode(bytes.subarray(0, newline)),
      body: new TextDecoder().decode(bytes.subarray(after + 4, after + 4 + length)),
    }
  }
  const readUntil = async (): Promise<{ auth: string; body: string }> => {
    const parsed = ready()
    if (parsed !== undefined) return parsed
    const buffer = new Uint8Array(4096)
    const read = await connection.read(buffer)
    if (read === null) throw new Error("client closed before the frame")
    bytes = concat(bytes, buffer.subarray(0, read))
    return readUntil()
  }
  const parsed = await readUntil()
  return { auth: parsed.auth, frame: JSON.parse(parsed.body) }
}

const frameOf = (value: unknown): Uint8Array => {
  const payload = new TextEncoder().encode(JSON.stringify(value))
  const framed = new Uint8Array(4 + payload.length)
  new DataView(framed.buffer).setUint32(0, payload.length, false)
  framed.set(payload, 4)
  return framed
}

Deno.test("the host-parsed CSR fixture passes the same policy in the VM", () => {
  const csr = Deno.readTextFileSync(`${root}/request.pem`)
  const parsed = parseCsrPem(csr, host.csr.cn)
  assertEquals(parsed.keyType, host.csr.key_type)
  assertEquals(parsed.keyBits, host.csr.key_bits)
  assertEquals(hex(parsed.spkiDer), host.cert.spki)
})

Deno.test("the host-signed chain validates against the fixture CSR key", () => {
  const csr = parseCsrPem(Deno.readTextFileSync(`${root}/request.pem`), host.csr.cn)
  const chain = Deno.readTextFileSync(`${root}/chain.pem`)
  const verified = verifyIssuedChain(chain, csr.spkiDer, host.csr.cn, NOW)
  assertEquals(verified.notAfter, host.cert.not_after)
  assertEquals(verified.leaf.commonName, host.cert.cn)
  const leaf = parseCertificatePem(Deno.readTextFileSync(`${root}/leaf.pem`))
  assertEquals(hex(leaf.spkiDer), host.cert.spki)
})

Deno.test("the adapter frames {v,cn,csr} after the auth line and maps {v,cn,chain,not_after}", async () => {
  await withTempDir("csr-wire", async (directory) => {
    const path = `${directory}/csr.sock`
    const listener = Deno.listen({ transport: "unix", path })
    const csr = Deno.readTextFileSync(`${root}/request.pem`)
    const chain = Deno.readTextFileSync(`${root}/chain.pem`)
    const server = (async () => {
      const connection = await listener.accept()
      const seen = await readAuthAndFrame(connection)
      await connection.write(
        frameOf({ v: 1, cn: "kanbei.sammohr.dev", chain, not_after: host.cert.not_after }),
      )
      connection.close()
      listener.close()
      return seen
    })()

    const reply = await Effect.runPromise(
      Effect.gen(function* () {
        const transport = yield* CsrTransport
        return yield* transport.exchange(csr)
      }).pipe(Effect.provide(vsockCsrTransportLayer({ path, token: TOKEN }))),
    )
    const seen = await server
    assertEquals(seen.auth, `KANBEI-AUTH 1 ${TOKEN}`)
    assertEquals(seen.frame, { v: 1, cn: "kanbei.sammohr.dev", csr })
    assertEquals(reply.notAfter, host.cert.not_after)
  })
})

Deno.test("a {v,error} reply fails closed", async () => {
  await withTempDir("csr-wire", async (directory) => {
    const path = `${directory}/csr.sock`
    const listener = Deno.listen({ transport: "unix", path })
    const server = (async () => {
      const connection = await listener.accept()
      await readAuthAndFrame(connection)
      await connection.write(frameOf({ v: 1, error: "csr must be exactly the CN" }))
      connection.close()
      listener.close()
    })()
    const failure = await Effect.runPromise(
      Effect.gen(function* () {
        const transport = yield* CsrTransport
        return yield* Effect.flip(transport.exchange("csr"))
      }).pipe(Effect.provide(vsockCsrTransportLayer({ path, token: TOKEN }))),
    )
    await server
    assertEquals(failure.reason.includes("host refused"), true)
  })
})
