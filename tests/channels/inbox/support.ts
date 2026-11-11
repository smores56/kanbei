import { Effect } from "effect"
import type { Schema } from "effect"
import { canonicalize } from "../../../src/evidence/canonical.ts"
import { sha256Text } from "../../../src/evidence/digest.ts"

const decoder = new TextDecoder()
const encoder = new TextEncoder()

export const hash = (text: string): Promise<string> => Effect.runPromise(sha256Text(text))

export const base64 = (text: string): string => btoa(String.fromCharCode(...encoder.encode(text)))

export interface KeyPair {
  readonly identityPath: string
  readonly recipient: string
}

export const makeKey = async (directory: string): Promise<KeyPair> => {
  const identityPath = `${directory}/identity.txt`
  const generated = await new Deno.Command("age-keygen", {
    args: ["-o", identityPath],
  }).output()
  if (generated.code !== 0) throw new Error(decoder.decode(generated.stderr))
  const publicKey = await new Deno.Command("age-keygen", {
    args: ["-y", identityPath],
  }).output()
  if (publicKey.code !== 0) throw new Error(decoder.decode(publicKey.stderr))
  return { identityPath, recipient: decoder.decode(publicKey.stdout).trim() }
}

export const encryptFile = async (
  plaintext: string,
  recipient: string,
  path: string,
): Promise<void> => {
  const child = new Deno.Command("age", {
    args: ["-r", recipient, "-o", path],
    stdin: "piped",
    stdout: "piped",
    stderr: "piped",
  }).spawn()
  const writer = child.stdin.getWriter()
  await writer.write(encoder.encode(plaintext))
  await writer.close()
  const status = await child.status
  if (status.code !== 0) throw new Error(decoder.decode(await new Response(child.stderr).bytes()))
}

export interface BatchOptions {
  readonly origin?: string
  readonly sequence?: number
  readonly bodies?: ReadonlyArray<{ readonly key: string; readonly text: string }>
  readonly suppressed?: number
  readonly coverage?: {
    readonly complete: boolean
    readonly gaps: number
    readonly suppressed: number
  }
  readonly recipient: string
  readonly batchDigestOverride?: string
  readonly envelopeVersion?: number
  readonly extraManifest?: Readonly<Record<string, unknown>>
  readonly extraEnvelope?: Readonly<Record<string, unknown>>
}

const iso = "2026-11-11T00:00:00.000Z"

export const buildBatch = async (options: BatchOptions): Promise<string> => {
  const origin = options.origin ?? "tuta-mail"
  const bodies = options.bodies ?? [{ key: "message/1", text: '{"subject":"hello"}' }]
  const hashed = await Promise.all(
    bodies.map(async (body) => ({ body, digest: await hash(body.text) })),
  )
  const blobs: Record<string, string> = Object.fromEntries(
    hashed.map((entry) => [entry.digest, base64(entry.body.text)]),
  )
  const entries = hashed.map((entry) => ({
    resourceKey: entry.body.key,
    kind: "capture",
    locator: `fixture:${entry.body.key}`,
    capturedAt: iso,
    body: entry.digest,
    evidence: { adapter: "fixture-v1" },
  }))
  const baseManifest: Schema.Json = {
    origin,
    sequence: options.sequence ?? 1,
    producedAt: iso,
    scan: {
      mode: "event",
      scopeHash: "a".repeat(64),
      startedAt: iso,
      completedAt: iso,
    },
    entries,
    coverage: options.coverage ?? { complete: true, gaps: 0, suppressed: 0 },
    suppressed: options.suppressed ?? 0,
    encryption: { scheme: "age", recipient: options.recipient },
    ...options.extraManifest,
  }
  const batchDigest = options.batchDigestOverride ?? await hash(canonicalize(baseManifest))
  const envelope = {
    envelopeVersion: options.envelopeVersion ?? 1,
    manifest: { ...baseManifest, batchDigest },
    blobs,
    ...options.extraEnvelope,
  }
  return JSON.stringify(envelope)
}

export const writeBatch = async (
  path: string,
  plaintext: string,
  recipient: string,
): Promise<void> => {
  await encryptFile(plaintext, recipient, path)
}

export const withTempDir = async <A>(
  prefix: string,
  run: (directory: string) => Promise<A>,
): Promise<A> => {
  const directory = await Deno.makeTempDir({ prefix: `kanbei-${prefix}-` })
  try {
    return await run(directory)
  } finally {
    await Deno.remove(directory, { recursive: true })
  }
}
