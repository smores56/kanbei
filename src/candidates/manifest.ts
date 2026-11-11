import { Effect, Schema } from "effect"
import { canonicalize } from "../evidence/canonical.ts"
import { sha256Labeled } from "../evidence/digest.ts"
import { ALL_CAPABILITIES } from "../kernel/capabilities.ts"

export const CandidateKind = Schema.Literals(["executable", "declarative"])
export type CandidateKind = typeof CandidateKind.Type

export const CapabilityScope = Schema.Literals(["narrow", "broad"])
export type CapabilityScope = typeof CapabilityScope.Type

export const CapabilityRequest = Schema.Struct({
  capability: Schema.String,
  scope: CapabilityScope,
})
export type CapabilityRequest = typeof CapabilityRequest.Type

export const Dependency = Schema.Struct({
  id: Schema.String,
  version: Schema.String,
})
export type Dependency = typeof Dependency.Type

export const ContentRef = Schema.Struct({
  digest: Schema.String,
  manifestDigest: Schema.String,
  size: Schema.Number,
})
export type ContentRef = typeof ContentRef.Type

export const CandidateManifest = Schema.Struct({
  candidateId: Schema.String,
  scope: Schema.String,
  generation: Schema.Number,
  base: Schema.optional(Schema.String),
  kind: CandidateKind,
  artifact: ContentRef,
  manifestDigest: Schema.String,
  dependencyLockDigest: Schema.String,
  capabilityRequestsDigest: Schema.String,
  schema: Schema.Struct({
    contractVersion: Schema.String,
    dataVersion: Schema.Number,
  }),
  createdAtUs: Schema.Number,
})
export type CandidateManifest = typeof CandidateManifest.Type

export const CapabilityDiff = Schema.Struct({
  added: Schema.Array(CapabilityRequest),
  removed: Schema.Array(CapabilityRequest),
  changed: Schema.Array(Schema.Struct({
    before: CapabilityRequest,
    after: CapabilityRequest,
    broadening: Schema.Boolean,
  })),
  dependencyChanges: Schema.Array(Schema.Struct({
    id: Schema.String,
    before: Schema.String,
    after: Schema.String,
    compatible: Schema.Boolean,
  })),
  migrations: Schema.Array(Schema.Struct({
    store: Schema.String,
    from: Schema.Number,
    to: Schema.Number,
    mode: Schema.Literals(["compatible", "forward-only", "requires-checkpoint"]),
  })),
  trustRootAccess: Schema.Boolean,
})
export type CapabilityDiff = typeof CapabilityDiff.Type

export const CandidateWorkspace = Schema.Struct({
  candidateId: Schema.String,
  scope: Schema.String,
  generation: Schema.Number,
  base: Schema.optional(Schema.String),
  kind: CandidateKind,
  capabilities: Schema.Array(CapabilityRequest),
  dependencies: Schema.Array(Dependency),
  schema: Schema.Struct({
    contractVersion: Schema.String,
    dataVersion: Schema.Number,
  }),
})
export type CandidateWorkspace = typeof CandidateWorkspace.Type

export const CANDIDATE_CONTRACT_VERSION = "kb/1.0.0"

const declared = new Set<string>([
  ...ALL_CAPABILITIES,
  "view.render",
  "tool.register",
  "projection.build",
  "plugin.state.read",
  "plugin.state.write",
])

const forbidden = new Set<string>([
  "canonical.append",
  "canonical.query",
  "fs.write",
  "process.spawn",
  "model.invoke",
  "secret.use",
  "activation.pointer.write",
])

export const trustRootCapability = (capability: string): boolean =>
  capability.startsWith("trust_root.")

export const capabilityClass = (
  capability: string,
): "declared" | "forbidden" | "trust_root" | "unknown" => {
  if (trustRootCapability(capability)) return "trust_root"
  if (forbidden.has(capability)) return "forbidden"
  if (declared.has(capability)) return "declared"
  return "unknown"
}

export const candidateDigest = (manifest: CandidateManifest): Effect.Effect<string> =>
  sha256Labeled(
    "kanbei.candidate.v1",
    [canonicalize(Schema.decodeUnknownSync(Schema.Json)(manifest))],
  ).pipe(Effect.orDie)

export const diffDigest = (diff: CapabilityDiff): Effect.Effect<string> =>
  sha256Labeled("kanbei.capabilitydiff.v1", [
    canonicalize(Schema.decodeUnknownSync(Schema.Json)(diff)),
  ])
    .pipe(Effect.orDie)

export const digestOfJson = (value: Schema.Json, label: string): Effect.Effect<string> =>
  sha256Labeled(label, [canonicalize(value)]).pipe(Effect.orDie)

const scopeRank: Readonly<Record<CapabilityScope, number>> = { narrow: 0, broad: 1 }

const capabilityKey = (capability: CapabilityRequest): string => capability.capability

/** The capability and dependency diff the consent surface shows (`U-06`). */
export const capabilityDiff = (
  before: ReadonlyArray<CapabilityRequest>,
  after: ReadonlyArray<CapabilityRequest>,
  baseDependencies: ReadonlyArray<Dependency>,
  dependencies: ReadonlyArray<Dependency>,
  migrations: CapabilityDiff["migrations"] = [],
): CapabilityDiff => {
  const beforeBy = new Map(before.map((entry) => [capabilityKey(entry), entry]))
  const afterBy = new Map(after.map((entry) => [capabilityKey(entry), entry]))
  const dependencyBy = new Map(baseDependencies.map((entry) => [entry.id, entry.version]))
  return {
    added: after.filter((entry) => !beforeBy.has(capabilityKey(entry))),
    removed: before.filter((entry) => !afterBy.has(capabilityKey(entry))),
    changed: after.flatMap((entry) => {
      const previous = beforeBy.get(capabilityKey(entry))
      if (previous === undefined || previous.scope === entry.scope) return []
      return [{
        before: previous,
        after: entry,
        broadening: scopeRank[entry.scope] > scopeRank[previous.scope],
      }]
    }),
    dependencyChanges: dependencies.flatMap((entry) => {
      const previous = dependencyBy.get(entry.id)
      if (previous === entry.version) return []
      return [{
        id: entry.id,
        before: previous ?? "absent",
        after: entry.version,
        compatible: previous === undefined
          ? false
          : previous.split(".")[0] === entry.version.split(".")[0],
      }]
    }),
    migrations,
    trustRootAccess: after.some((entry) => trustRootCapability(entry.capability)),
  }
}
