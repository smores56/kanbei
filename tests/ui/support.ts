import { DISCLOSURE_FIELDS } from "../../src/ui/model.ts"
import type { ViewDocument } from "../../src/ui/model.ts"

export const baseDocument = (overrides: Partial<ViewDocument> = {}): ViewDocument => ({
  schemaVersion: 1,
  viewId: "decisions",
  revision: "rev-1",
  route: "decisions",
  title: "Decision queue",
  anchors: ["anchor:row-1", "anchor:consent"],
  consentLink: "consent:approval-1",
  decisionAffordance: "action:approve",
  disclosure: DISCLOSURE_FIELDS.map((field) => ({ field, mode: "include" as const })),
  freshness: {
    state: "current",
    observedAt: "2026-11-11T00:00:00.000Z",
    windowMs: 3_600_000,
    pending: 0,
    watermarkSeq: 4,
  },
  sections: [{
    id: "queue",
    title: "Queue",
    rows: [{
      id: "row-1",
      anchor: "anchor:row-1",
      cells: [{ label: "subject", text: "Renew the lease" }],
      badges: [{ kind: "current", label: "current" }],
      actions: [
        { id: "action:approve", kind: "rule", label: "Approve", target: "anchor:row-1" },
        {
          id: "action:consent",
          kind: "link-consent",
          label: "Open consent",
          target: "anchor:consent",
        },
        { id: "action:expand", kind: "expand", label: "Expand", target: "anchor:row-1" },
      ],
      provenance: {
        observationId: "obs:1",
        span: "bytes 0-120",
        derivation: "capture",
        scope: "personal",
      },
    }],
  }],
  ...overrides,
})

/** A second revision that adds one row and changes one cell. */
export const secondDocument = (): ViewDocument => {
  const base = baseDocument()
  const row = base.sections[0]?.rows[0]
  if (row === undefined) throw new Error("baseline document has no row")
  return baseDocument({
    revision: "rev-2",
    anchors: [...base.anchors, "anchor:row-2"],
    sections: [{
      id: "queue",
      title: "Queue",
      rows: [
        { ...row, cells: [{ label: "subject", text: "Renew the lease twice" }] },
        { ...row, id: "row-2", anchor: "anchor:row-2" },
      ],
    }],
  })
}
