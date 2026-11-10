import type { Json } from "../../evidence/canonical.ts"
import type { ProgressiveDisclosureRow } from "./types.ts"

export const TOOL_COUNTS = [10, 50, 200, 1000] as const
const DESCRIBE_HITS = 8

const toolDescriptor = (index: number): Json => ({
  name: `project_tool_${String(index).padStart(4, "0")}`,
  version: "1.0.0",
  digest: `sha256:${String(index).padStart(64, "0")}`,
  kind: "host",
  summary: `Project operation ${index} for the sealed workload catalog.`,
  input: {
    type: "object",
    properties: {
      project: { type: "string", description: "Project root identifier selected by the human." },
      query: { type: "string", description: "Language-agnostic query or path fragment." },
      limit: { type: "number", description: "Maximum rows to return, operator-capped." },
    },
    required: ["project"],
    additionalProperties: false,
  },
  output: {
    type: "object",
    properties: {
      items: { type: "array", items: { type: "object" } },
      nextCursor: { type: "string" },
    },
  },
  errors: ["not_found", "permission_denied", "timeout", "version_conflict"],
  effect: "read",
  idempotency: "supported",
  timeoutMs: 30_000,
  streaming: false,
  capabilities: [{ kind: "filesystem", scope: "read", root: "project" }],
  docs: `cas://sha256:${String(index).padStart(64, "0")}`,
  examples: [
    {
      input: { project: "kanbei", query: `src/module-${index}.ts`, limit: 10 },
      output: { items: [] },
      note: "Empty result is a valid success.",
    },
  ],
})

const toolSummary = (index: number): Json => ({
  name: `project_tool_${String(index).padStart(4, "0")}`,
  version: "1.0.0",
  digest: `sha256:${String(index).padStart(64, "0")}`,
  kind: "host",
  summary: `Project operation ${index} for the sealed workload catalog.`,
  tags: ["project", "read"],
})

export const fullSchemaChars = (tools: number): number =>
  JSON.stringify(Array.from({ length: tools }, (_unused, index) => toolDescriptor(index))).length

/** Summaries for the whole catalog plus exact descriptors for the top describe hits. */
export const progressiveChars = (tools: number): number => {
  const summaries = Array.from({ length: tools }, (_unused, index) => toolSummary(index))
  const described = Array.from(
    { length: Math.min(DESCRIBE_HITS, tools) },
    (_unused, index) => toolDescriptor(index),
  )
  return JSON.stringify({ summaries, described }).length
}

export const progressiveDisclosureRows = (): ReadonlyArray<ProgressiveDisclosureRow> =>
  TOOL_COUNTS.map((tools) => {
    const full = fullSchemaChars(tools)
    const progressive = progressiveChars(tools)
    return {
      tools,
      fullSchemaChars: full,
      progressiveChars: progressive,
      savingsRatio: full === 0 ? 0 : 1 - progressive / full,
      tokenMeasurement: "blocked",
      blockedBy: ["MI-07"],
    }
  })
