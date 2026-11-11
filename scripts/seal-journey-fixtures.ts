/**
 * Seals the journey-v2 source into age ciphertext. Run once and commit the
 * output: age encryption is randomized, so resealing changes every digest.
 */
import { Effect, Schema } from "effect"
import { canonicalize } from "../src/evidence/canonical.ts"
import { sha256Text } from "../src/evidence/digest.ts"

const Entry = Schema.Struct({
  resourceKey: Schema.String,
  kind: Schema.String,
  locator: Schema.String,
  capturedAt: Schema.String,
  mediaType: Schema.optional(Schema.String),
  body: Schema.String,
  evidence: Schema.Json,
})

const Batch = Schema.Struct({
  caseId: Schema.String,
  file: Schema.String,
  origin: Schema.String,
  sequence: Schema.Number,
  producesAdvice: Schema.optional(Schema.Boolean),
  duplicateOf: Schema.optional(Schema.String),
  producedAt: Schema.String,
  scan: Schema.Struct({
    mode: Schema.Literals(["event", "full", "targeted"]),
    scopeHash: Schema.String,
    startedAt: Schema.String,
    completedAt: Schema.String,
  }),
  coverage: Schema.Struct({
    complete: Schema.Boolean,
    gaps: Schema.Number,
    suppressed: Schema.Number,
  }),
  suppressed: Schema.Number,
  entries: Schema.Array(Entry),
})

const Source = Schema.Struct({
  dataset: Schema.Literal("journey-v2"),
  version: Schema.Literal("1.0.0"),
  recipient: Schema.String,
  sealedBy: Schema.String,
  batches: Schema.Array(Batch),
})

type Batch = typeof Batch.Type

const root = "fixtures/journey-v2"
const encoder = new TextEncoder()
const decoder = new TextDecoder()

const base64 = (bytes: Uint8Array): string =>
  btoa(Array.from(bytes, (byte) => String.fromCharCode(byte)).join(""))

const runEffect = <A, E>(effect: Effect.Effect<A, E>): Promise<A> => Effect.runPromise(effect)

const buildEnvelope = async (batch: Batch, recipient: string): Promise<string> => {
  const blobs: Record<string, string> = {}
  const entries = await Promise.all(
    batch.entries.map(async (entry) => {
      const digest = await runEffect(sha256Text(entry.body))
      blobs[digest] = base64(encoder.encode(entry.body))
      return {
        resourceKey: entry.resourceKey,
        kind: entry.kind,
        locator: entry.locator,
        capturedAt: entry.capturedAt,
        ...(entry.mediaType === undefined ? {} : { mediaType: entry.mediaType }),
        body: digest,
        evidence: entry.evidence,
      }
    }),
  )
  const manifest = {
    origin: batch.origin,
    sequence: batch.sequence,
    producedAt: batch.producedAt,
    scan: batch.scan,
    entries,
    coverage: batch.coverage,
    suppressed: batch.suppressed,
    encryption: { scheme: "age" as const, recipient },
  }
  const batchDigest = await runEffect(sha256Text(canonicalize(manifest)))
  return JSON.stringify(
    {
      envelopeVersion: 1 as const,
      manifest: { ...manifest, batchDigest },
      blobs,
    },
    null,
    2,
  )
}

const encrypt = async (plaintext: string, recipient: string, target: string): Promise<void> => {
  const child = new Deno.Command("age", {
    args: ["-r", recipient, "-o", target],
    stdin: "piped",
    stdout: "piped",
    stderr: "piped",
  }).spawn()
  const writer = child.stdin.getWriter()
  await writer.write(encoder.encode(plaintext))
  await writer.close()
  const status = await child.status
  if (status.code !== 0) {
    throw new Error(decoder.decode(await new Response(child.stderr).bytes()))
  }
}

const main = async (): Promise<void> => {
  const source = Schema.decodeUnknownSync(Source)(
    JSON.parse(await Deno.readTextFile(`${root}/source.json`)),
  )
  const bases = source.batches.filter((batch) => batch.duplicateOf === undefined)
  const plaintextByCase = new Map<string, string>(
    await Promise.all(
      bases.map(async (batch) =>
        [batch.caseId, await buildEnvelope(batch, source.recipient)] as const
      ),
    ),
  )

  await Promise.all(
    source.batches.map(async (batch) => {
      const inherited = batch.duplicateOf === undefined
        ? plaintextByCase.get(batch.caseId)
        : plaintextByCase.get(batch.duplicateOf)
      if (inherited === undefined) {
        throw new Error(`duplicate source ${batch.duplicateOf} is not declared`)
      }
      const target = `${root}/${batch.file}`
      Deno.mkdirSync(target.slice(0, target.lastIndexOf("/")), { recursive: true })
      await encrypt(inherited, source.recipient, target)
      Effect.runSync(Effect.logInfo(`${batch.caseId}: ${batch.file}`))
    }),
  )
}

if (import.meta.main) await main()
