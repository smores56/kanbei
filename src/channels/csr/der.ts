/**
 * Minimal DER writer/reader for the CSR channel. The deployed host parser
 * (`nix-config modules/lib/kanbei/vsock.py`) accepts exactly one CN, an optional
 * SAN carrying that name, and a P-256 or RSA key; this module produces and
 * checks that short allow-list without a third-party x509 dependency.
 */

export interface Tlv {
  readonly tag: number
  readonly value: Uint8Array
  /** Offset just past this value. */
  readonly end: number
}

export class DerError extends Error {}

const concat = (parts: ReadonlyArray<Uint8Array>): Uint8Array => {
  const total = parts.reduce((sum, part) => sum + part.length, 0)
  const out = new Uint8Array(total)
  let offset = 0
  for (const part of parts) {
    out.set(part, offset)
    offset += part.length
  }
  return out
}

const encodeLength = (length: number): Uint8Array => {
  if (length < 0x80) return Uint8Array.of(length)
  const bytes: Array<number> = []
  let remaining = length
  while (remaining > 0) {
    bytes.unshift(remaining & 0xff)
    remaining = Math.floor(remaining / 256)
  }
  return Uint8Array.of(0x80 | bytes.length, ...bytes)
}

export const encodeTlv = (tag: number, value: Uint8Array): Uint8Array =>
  concat([Uint8Array.of(tag), encodeLength(value.length), value])

export const encodeSequence = (...values: ReadonlyArray<Uint8Array>): Uint8Array =>
  encodeTlv(0x30, concat(values))

export const encodeSet = (...values: ReadonlyArray<Uint8Array>): Uint8Array =>
  encodeTlv(0x31, concat(values))

export const encodeOctet = (value: Uint8Array): Uint8Array => encodeTlv(0x04, value)

export const encodeBitString = (value: Uint8Array): Uint8Array =>
  encodeTlv(0x03, concat([Uint8Array.of(0), value]))

export const encodeUtf8 = (text: string): Uint8Array =>
  encodeTlv(0x0c, new TextEncoder().encode(text))

export const encodeDnsName = (name: string): Uint8Array =>
  encodeTlv(0x82, new TextEncoder().encode(name))

const encodeBase128 = (value: number): Array<number> => {
  const bytes: Array<number> = [value & 0x7f]
  let remaining = Math.floor(value / 128)
  while (remaining > 0) {
    bytes.unshift((remaining & 0x7f) | 0x80)
    remaining = Math.floor(remaining / 128)
  }
  return bytes
}

export const encodeOid = (dotted: string): Uint8Array => {
  const parts = dotted.split(".").map((part) => Number(part))
  const first = parts[0]
  const second = parts[1]
  if (first === undefined || second === undefined || !Number.isInteger(first)) {
    throw new DerError(`malformed OID ${dotted}`)
  }
  const body = [first * 40 + second, ...parts.slice(2).flatMap(encodeBase128)]
  return encodeTlv(0x06, Uint8Array.from(body))
}

/** Positive INTEGER from unsigned magnitude bytes. */
export const encodeInteger = (magnitude: Uint8Array): Uint8Array => {
  const first = magnitude[0] ?? 0
  const body = first & 0x80 ? concat([Uint8Array.of(0), magnitude]) : magnitude
  return encodeTlv(0x02, body)
}

export const encodeIntegerValue = (value: number): Uint8Array => encodeInteger(Uint8Array.of(value))

export const readTlv = (data: Uint8Array, offset = 0): Tlv => {
  if (offset + 2 > data.length) throw new DerError("truncated DER tag")
  const tag = data[offset] ?? 0
  const first = data[offset + 1] ?? 0
  let cursor = offset + 2
  let length = 0
  if (first < 0x80) {
    length = first
  } else {
    const count = first & 0x7f
    if (count === 0 || count > 4 || cursor + count > data.length) {
      throw new DerError("unsupported DER length")
    }
    if (data[cursor] === 0) throw new DerError("non-minimal DER length")
    for (let index = 0; index < count; index += 1) {
      length = length * 256 + (data[cursor + index] ?? 0)
    }
    cursor += count
    if (length < 0x80) throw new DerError("non-minimal DER length")
  }
  const end = cursor + length
  if (end > data.length) throw new DerError("truncated DER value")
  return { tag, value: data.subarray(cursor, end), end }
}

export const children = (value: Uint8Array): ReadonlyArray<Tlv> => {
  const out: Array<Tlv> = []
  let offset = 0
  while (offset < value.length) {
    const item = readTlv(value, offset)
    out.push(item)
    offset = item.end
  }
  if (out.length === 0) throw new DerError("empty DER container")
  return out
}

export const single = (data: Uint8Array, tag: number, what: string): Uint8Array => {
  const item = readTlv(data)
  if (item.tag !== tag || item.end !== data.length) {
    throw new DerError(`${what} must be one tag 0x${tag.toString(16)} value`)
  }
  return item.value
}

export const decodeOid = (value: Uint8Array): string => {
  const first = value[0]
  if (first === undefined) throw new DerError("empty OID")
  const parts: Array<string> = [String(Math.floor(first / 40)), String(first % 40)]
  let number = 0
  let started = false
  for (const byte of value.subarray(1)) {
    number = number * 128 + (byte & 0x7f)
    started = true
    if ((byte & 0x80) === 0) {
      parts.push(String(number))
      number = 0
      started = false
    }
  }
  if (started) throw new DerError("truncated OID")
  return parts.join(".")
}

const PEM_BODY = /^[A-Za-z0-9+/=\r\n]+$/u

export const pemEncode = (label: string, der: Uint8Array): string => {
  const base64 = der.toBase64()
  const lines = base64.match(/.{1,64}/gu) ?? []
  return `-----BEGIN ${label}-----\n${lines.join("\n")}\n-----END ${label}-----\n`
}

export const pemDecode = (pem: string, label: string, maxBytes: number): Uint8Array => {
  const pattern = new RegExp(
    `-----BEGIN ${label}-----\\r?\\n([A-Za-z0-9+/=\\r\\n]+)-----END ${label}-----\\r?\\n?$`,
    "u",
  )
  const match = pattern.exec(pem.trim())
  if (match === null || !PEM_BODY.test(match[1] ?? "")) {
    throw new DerError(`${label} is not a single PEM block`)
  }
  const body = (match[1] ?? "").replace(/\s+/gu, "")
  let der: Uint8Array
  try {
    der = Uint8Array.fromBase64(body)
  } catch {
    throw new DerError(`${label} base64 is invalid`)
  }
  if (der.length === 0 || der.length > maxBytes) {
    throw new DerError(`${label} is empty or too large`)
  }
  return der
}

export const splitPemBlocks = (
  pem: string,
  label: string,
  maxBytes: number,
): ReadonlyArray<Uint8Array> => {
  const pattern = new RegExp(`-----BEGIN ${label}-----[\\s\\S]*?-----END ${label}-----`, "gu")
  const blocks = pem.match(pattern) ?? []
  if (blocks.length === 0) throw new DerError(`chain holds no ${label} blocks`)
  return blocks.map((block) => pemDecode(block, label, maxBytes))
}
