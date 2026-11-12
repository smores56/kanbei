import {
  children,
  decodeOid,
  encodeBitString,
  encodeDnsName,
  encodeOctet,
  encodeOid,
  encodeSequence,
  encodeSet,
  encodeTlv,
  encodeUtf8,
  pemDecode,
  pemEncode,
  single,
  splitPemBlocks,
} from "./der.ts"
import type { Tlv } from "./der.ts"

export const OID_CN = "2.5.4.3"
export const OID_EXTENSION_REQUEST = "1.2.840.113549.1.9.14"
export const OID_SUBJECT_ALT_NAME = "2.5.29.17"
export const OID_EC = "1.2.840.10045.2.1"
export const OID_P256 = "1.2.840.10045.3.1.7"
export const OID_ECDSA_SHA256 = "1.2.840.10045.4.3.2"
export const OID_RSA = "1.2.840.113549.1.1.1"

export const CSR_MAX_CSR_BYTES = 16 * 1024
export const CSR_MAX_CERT_BYTES = 64 * 1024

export class CsrPolicyError extends Error {}

const text = new TextDecoder("utf-8", { fatal: true })
const magnitude = (bytes: Uint8Array): Uint8Array => {
  let start = 0
  while (start < bytes.length - 1 && bytes[start] === 0) start += 1
  return bytes.subarray(start)
}

const derInteger = (bytes: Uint8Array): Uint8Array => {
  const body = (bytes[0] ?? 0) & 0x80 ? Uint8Array.from([0, ...bytes]) : bytes
  return Uint8Array.from([0x02, body.length, ...body])
}

/** RFC 3279: WebCrypto returns the raw r||s pair; PKCS#10 wants DER INTEGERs. */
export const rawEcdsaToDer = (raw: Uint8Array): Uint8Array => {
  // Deno returns raw r||s; an already-DER signature is passed through so a
  // runtime following the older convention cannot corrupt the CSR.
  if (raw[0] === 0x30 && raw[1] === raw.length - 2 && raw[2] === 0x02) return raw
  const half = raw.length / 2
  if (!Number.isInteger(half) || half === 0) {
    throw new CsrPolicyError(`ECDSA signature is not a raw r||s pair (${raw.length} bytes)`)
  }
  const r = magnitude(raw.subarray(0, half))
  const s = magnitude(raw.subarray(half))
  const body = Uint8Array.from([...derInteger(r), ...derInteger(s)])
  return Uint8Array.from([0x30, body.length, ...body])
}

export interface CsrInput {
  readonly commonName: string
  readonly spkiDer: Uint8Array
  readonly sign: (info: Uint8Array) => Promise<Uint8Array>
}

/**
 * Build one PKCS#10 CSR with the fixed subject CN, the SAN extension request,
 * and an ecdsa-with-SHA256 signature over the CertificationRequestInfo.
 */
export const buildCsrPem = async (input: CsrInput): Promise<string> => {
  const subject = encodeSequence(
    encodeSet(encodeSequence(encodeOid(OID_CN), encodeUtf8(input.commonName))),
  )
  const san = encodeSequence(
    encodeOid(OID_SUBJECT_ALT_NAME),
    encodeOctet(encodeSequence(encodeDnsName(input.commonName))),
  )
  const attributes = encodeTlv(
    0xa0,
    encodeSequence(
      encodeOid(OID_EXTENSION_REQUEST),
      encodeSet(encodeSequence(san)),
    ),
  )
  const info = encodeSequence(
    Uint8Array.from([0x02, 0x01, 0x00]),
    subject,
    input.spkiDer,
    attributes,
  )
  const signature = rawEcdsaToDer(await input.sign(info))
  const request = encodeSequence(
    info,
    encodeSequence(encodeOid(OID_ECDSA_SHA256)),
    encodeBitString(signature),
  )
  return pemEncode("CERTIFICATE REQUEST", request)
}

const parseName = (value: Uint8Array): ReadonlyArray<readonly [string, string]> => {
  const fields: Array<readonly [string, string]> = []
  for (const rdn of children(value)) {
    if (rdn.tag !== 0x31) throw new CsrPolicyError("Name must contain SET values")
    const entries = children(rdn.value)
    if (entries.length !== 1) throw new CsrPolicyError("multi-valued RDNs are not allowed")
    const atv = entries[0]
    if (atv === undefined || atv.tag !== 0x30) {
      throw new CsrPolicyError("AttributeTypeAndValue must be a SEQUENCE")
    }
    const parts = children(atv.value)
    if (parts.length !== 2) {
      throw new CsrPolicyError("AttributeTypeAndValue must hold an OID and one value")
    }
    const [oidPart, valuePart] = parts as [Tlv, Tlv]
    if (oidPart.tag !== 0x06) throw new CsrPolicyError("attribute type must be an OID")
    if (valuePart.tag !== 0x0c && valuePart.tag !== 0x13) {
      throw new CsrPolicyError("attribute value must be a string")
    }
    fields.push([decodeOid(oidPart.value), text.decode(valuePart.value)])
  }
  return fields
}

const checkSubject = (subject: Uint8Array, commonName: string): void => {
  const fields = parseName(subject)
  const only = fields[0]
  if (
    fields.length !== 1 || only === undefined || only[0] !== OID_CN || only[1] !== commonName
  ) {
    throw new CsrPolicyError(`subject must contain only CN=${commonName}`)
  }
}

const checkSpki = (spki: Uint8Array): void => {
  const parts = children(spki)
  const [algorithm, key] = parts as [Tlv, Tlv]
  if (parts.length !== 2 || algorithm.tag !== 0x30 || key === undefined || key.tag !== 0x03) {
    throw new CsrPolicyError("SubjectPublicKeyInfo is malformed")
  }
  const algParts = children(algorithm.value)
  const oidPart = algParts[0]
  if (oidPart === undefined || oidPart.tag !== 0x06) {
    throw new CsrPolicyError("algorithm identifier has no OID")
  }
  const oid = decodeOid(oidPart.value)
  if (oid !== OID_EC) throw new CsrPolicyError(`public key algorithm ${oid} is not allowed`)
  const curve = algParts[1]
  if (curve === undefined || curve.tag !== 0x06 || decodeOid(curve.value) !== OID_P256) {
    throw new CsrPolicyError("EC keys must use prime256v1 (P-256)")
  }
  const bits = key.value
  if (bits.length !== 66 || bits[0] !== 0 || bits[1] !== 0x04) {
    throw new CsrPolicyError("P-256 public key must be one uncompressed point")
  }
}

const checkExtensions = (extensions: Uint8Array, commonName: string): void => {
  let seenSan = false
  for (const extension of children(extensions)) {
    if (extension.tag !== 0x30) throw new CsrPolicyError("each extension must be a SEQUENCE")
    const parts = children(extension.value)
    const oidPart = parts[0]
    const valuePart = parts.at(-1)
    if (
      parts.length < 2 || parts.length > 3 || oidPart === undefined || oidPart.tag !== 0x06 ||
      valuePart === undefined || valuePart.tag !== 0x04
    ) {
      throw new CsrPolicyError("extension must hold an OID and an OCTET STRING")
    }
    const oid = decodeOid(oidPart.value)
    if (oid !== OID_SUBJECT_ALT_NAME) throw new CsrPolicyError(`extension ${oid} is not allowed`)
    if (seenSan) throw new CsrPolicyError("duplicate subjectAltName")
    seenSan = true
    const names = children(single(valuePart.value, 0x30, "GeneralNames"))
    const only = names[0]
    if (
      names.length !== 1 || only === undefined || only.tag !== 0x82 ||
      new TextDecoder().decode(only.value) !== commonName
    ) {
      throw new CsrPolicyError(`subjectAltName must be exactly ${commonName}`)
    }
  }
}

const checkAttributes = (attributes: Uint8Array, commonName: string): void => {
  if (attributes.length === 0) return
  let seen = false
  for (const attribute of children(attributes)) {
    if (attribute.tag !== 0x30) throw new CsrPolicyError("attribute must be a SEQUENCE")
    const parts = children(attribute.value)
    const oidPart = parts[0]
    const setPart = parts[1]
    if (
      parts.length !== 2 || oidPart === undefined || oidPart.tag !== 0x06 ||
      setPart === undefined ||
      setPart.tag !== 0x31
    ) {
      throw new CsrPolicyError("attribute must hold an OID and a SET")
    }
    if (decodeOid(oidPart.value) !== OID_EXTENSION_REQUEST) {
      throw new CsrPolicyError("only extensionRequest attributes are allowed")
    }
    if (seen) throw new CsrPolicyError("duplicate extensionRequest")
    seen = true
    const values = children(setPart.value)
    const only = values[0]
    if (values.length !== 1 || only === undefined || only.tag !== 0x30) {
      throw new CsrPolicyError("extensionRequest must hold one Extensions value")
    }
    checkExtensions(only.value, commonName)
  }
}

export interface ParsedCsr {
  readonly commonName: string
  readonly spkiDer: Uint8Array
  readonly keyType: "ec"
  readonly keyBits: 256
}

/** Mirror of the host's `validate_csr`; a CSR that fails here is never sent. */
export const parseCsrPem = (pem: string, commonName: string): ParsedCsr => {
  const der = pemDecode(pem, "CERTIFICATE REQUEST", CSR_MAX_CSR_BYTES)
  const body = single(der, 0x30, "CertificationRequest")
  const parts = children(body)
  const [info, algorithm, signature] = parts as [Tlv, Tlv, Tlv]
  if (
    parts.length !== 3 || info.tag !== 0x30 || algorithm.tag !== 0x30 || signature === undefined ||
    signature.tag !== 0x03
  ) {
    throw new CsrPolicyError("CertificationRequest is malformed")
  }
  const algParts = children(algorithm.value)
  const algOid = algParts[0]
  if (algOid === undefined || algOid.tag !== 0x06) {
    throw new CsrPolicyError("signature algorithm is malformed")
  }
  if (decodeOid(algOid.value) !== OID_ECDSA_SHA256) {
    throw new CsrPolicyError("CSR signature algorithm must be ecdsa-with-SHA256")
  }
  const infoParts = children(info.value)
  const [version, subject, spki, attributes] = infoParts as [Tlv, Tlv, Tlv, Tlv]
  if (
    infoParts.length !== 4 || version.tag !== 0x02 || version.value[0] !== 0 ||
    version.value.length !== 1 || subject === undefined || subject.tag !== 0x30 ||
    spki === undefined || spki.tag !== 0x30 || attributes === undefined ||
    attributes.tag !== 0xa0
  ) {
    throw new CsrPolicyError("CertificationRequestInfo is malformed")
  }
  checkSubject(subject.value, commonName)
  checkSpki(spki.value)
  checkAttributes(attributes.value, commonName)
  return {
    commonName,
    spkiDer: Uint8Array.from([0x30, spki.value.length, ...spki.value]),
    keyType: "ec",
    keyBits: 256,
  }
}

export interface ParsedCertificate {
  readonly der: Uint8Array
  /** DER of the SubjectPublicKeyInfo, tag and length included. */
  readonly spkiDer: Uint8Array
  readonly commonName: string
  /** Issuer CN; a self-issued leaf is not an ACME certificate. */
  readonly issuer: string
  readonly sans: ReadonlyArray<string>
  readonly notAfter: string
}

const parseTime = (value: Uint8Array, tag: number): string => {
  let digits: string
  try {
    digits = new TextDecoder("ascii", { fatal: true }).decode(value)
  } catch {
    throw new CsrPolicyError("certificate time is not ASCII")
  }
  let year: number
  let rest: string
  if (tag === 0x17) {
    if (digits.length !== 13 || !digits.endsWith("Z")) throw new CsrPolicyError("UTCTime")
    const short = Number(digits.slice(0, 2))
    year = short < 50 ? 2000 + short : 1900 + short
    rest = digits.slice(2)
  } else if (tag === 0x18) {
    if (digits.length !== 15 || !digits.endsWith("Z")) {
      throw new CsrPolicyError("GeneralizedTime")
    }
    year = Number(digits.slice(0, 4))
    rest = digits.slice(4)
  } else {
    throw new CsrPolicyError("unknown certificate time tag")
  }
  return `${String(year).padStart(4, "0")}-${rest.slice(0, 2)}-${rest.slice(2, 4)}T${
    rest.slice(4, 6)
  }:${rest.slice(6, 8)}:${rest.slice(8, 10)}Z`
}

const extractSans = (extensions: Uint8Array): ReadonlyArray<string> => {
  const out: Array<string> = []
  for (const extension of children(extensions)) {
    if (extension.tag !== 0x30) throw new CsrPolicyError("certificate extension is malformed")
    const parts = children(extension.value)
    const oidPart = parts[0]
    const valuePart = parts.at(-1)
    if (oidPart === undefined || oidPart.tag !== 0x06 || valuePart === undefined) {
      throw new CsrPolicyError("certificate extension is malformed")
    }
    if (decodeOid(oidPart.value) !== OID_SUBJECT_ALT_NAME) continue
    for (const name of children(single(valuePart.value, 0x30, "GeneralNames"))) {
      out.push(new TextDecoder().decode(name.value))
    }
  }
  return out
}

export const parseCertificateDer = (der: Uint8Array): ParsedCertificate => {
  const body = single(der, 0x30, "Certificate")
  const parts = children(body)
  const tbs = parts[0]
  if (parts.length !== 3 || tbs === undefined || tbs.tag !== 0x30) {
    throw new CsrPolicyError("Certificate is malformed")
  }
  const fields = children(tbs.value)
  const index = fields[0]?.tag === 0xa0 ? 1 : 0
  if (fields.length < index + 6) throw new CsrPolicyError("TBSCertificate is malformed")
  const validity = fields[index + 3]
  const subject = fields[index + 4]
  const spki = fields[index + 5]
  if (validity === undefined || subject === undefined || spki === undefined) {
    throw new CsrPolicyError("TBSCertificate is malformed")
  }
  if (validity.tag !== 0x30 || subject.tag !== 0x30 || spki.tag !== 0x30) {
    throw new CsrPolicyError("TBSCertificate fields are malformed")
  }
  const name = parseName(subject.value)
  const cn = name.find(([oid]) => oid === OID_CN)?.[1] ?? ""
  const issuerField = fields[index + 2]
  if (issuerField === undefined || issuerField.tag !== 0x30) {
    throw new CsrPolicyError("TBSCertificate has no issuer")
  }
  const issuer = parseName(issuerField.value).find(([oid]) => oid === OID_CN)?.[1] ?? ""
  const times = children(validity.value)
  const end = times[1]
  if (times.length !== 2 || end === undefined) {
    throw new CsrPolicyError("Validity must hold two times")
  }
  const extensionField = fields[index + 6]
  const sans = extensionField?.tag === 0xa3
    ? extractSans(single(extensionField.value, 0x30, "Extensions"))
    : []
  return {
    der,
    spkiDer: Uint8Array.from([0x30, spki.value.length, ...spki.value]),
    commonName: cn,
    issuer,
    sans,
    notAfter: parseTime(end.value, end.tag),
  }
}

export const parseCertificatePem = (
  pem: string,
  maxBytes = CSR_MAX_CERT_BYTES,
): ParsedCertificate => parseCertificateDer(pemDecode(pem, "CERTIFICATE", maxBytes))

export const splitChain = (pem: string): ReadonlyArray<Uint8Array> =>
  splitPemBlocks(pem, "CERTIFICATE", CSR_MAX_CERT_BYTES)

export interface VerifiedChain {
  readonly leaf: ParsedCertificate
  readonly chainPem: string
  readonly notAfter: string
}

/**
 * The only certificate the host may return: the fixed CN/SAN, the CSR's public
 * key, and a leaf that has not expired. Any mismatch refuses the whole reply.
 */
export const verifyIssuedChain = (
  chainPem: string,
  expectedSpkiDer: Uint8Array,
  commonName: string,
  now: number,
): VerifiedChain => {
  const blocks = splitChain(chainPem)
  const leafDer = blocks[0]
  if (leafDer === undefined) throw new CsrPolicyError("chain holds no certificates")
  const leaf = parseCertificateDer(leafDer)
  if (leaf.commonName !== commonName) {
    throw new CsrPolicyError(`leaf CN ${leaf.commonName} != ${commonName}`)
  }
  for (const san of leaf.sans) {
    if (san !== commonName) throw new CsrPolicyError(`leaf SAN ${san} != ${commonName}`)
  }
  const expected = Array.from(expectedSpkiDer)
  const actual = Array.from(leaf.spkiDer)
  if (expected.length !== actual.length || expected.some((byte, i) => byte !== actual[i])) {
    throw new CsrPolicyError("certificate public key does not match the CSR")
  }
  if (leaf.issuer.trim() === "" || leaf.issuer === leaf.commonName) {
    throw new CsrPolicyError("certificate is self-issued or has no issuer")
  }
  const notAfter = Date.parse(leaf.notAfter)
  if (!Number.isFinite(notAfter) || notAfter <= now) {
    throw new CsrPolicyError("returned certificate is already expired")
  }
  return { leaf, chainPem, notAfter: leaf.notAfter }
}

export const spkiOf = (pem: string): Uint8Array => {
  const parsed = parseCertificatePem(pem)
  return parsed.spkiDer
}
