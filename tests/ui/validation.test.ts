import { assertEquals } from "@std/assert"
import { Result } from "effect"
import { parseViewDocument, validateViewDocument } from "../../src/ui/validation.ts"
import type { ValidationOptions } from "../../src/ui/validation.ts"
import type { ViewAction } from "../../src/ui/model.ts"

const retarget = (
  actions: ReadonlyArray<ViewAction>,
  id: string,
  target: string,
): ReadonlyArray<ViewAction> =>
  actions.map((action) =>
    action.id === id ? { id: action.id, kind: action.kind, label: action.label, target } : action
  )
import { baseDocument, secondDocument } from "./support.ts"

const options: ValidationOptions = { authorizedSources: ["obs:"] }

const codes = (result: ReturnType<typeof validateViewDocument>): ReadonlyArray<string> =>
  Result.isFailure(result) ? result.failure.map((entry) => entry.code) : []

Deno.test("a well-formed declarative view passes validation", () => {
  const result = validateViewDocument(baseDocument(), options)
  assertEquals(Result.isSuccess(result), true)
})

Deno.test("a view that removes the decision affordance is rejected", () => {
  const base = baseDocument()
  const row = base.sections[0]?.rows[0]
  if (row === undefined) throw new Error("baseline has no row")
  const withoutAffordance = baseDocument({
    sections: [{
      id: "queue",
      title: "Queue",
      rows: [{
        ...row,
        actions: row.actions.filter((action) => action.id !== "action:approve"),
      }],
    }],
  })
  assertEquals(
    codes(validateViewDocument(withoutAffordance, { ...options, base })).includes(
      "decision_affordance_removed",
    ),
    true,
  )
  assertEquals(
    codes(validateViewDocument(withoutAffordance, options)).includes(
      "decision_affordance_missing",
    ),
    true,
  )
})

Deno.test("a view that drops an anchor or the consent link is rejected", () => {
  const base = baseDocument()
  const dropped = baseDocument({ anchors: ["anchor:row-1"], consentLink: "consent:other" })
  const result = validateViewDocument(dropped, { ...options, base })
  const found = codes(result)
  assertEquals(found.includes("anchor_removed"), true)
  assertEquals(found.includes("consent_link_removed"), true)
})

Deno.test("a view that adds a remote origin is rejected", () => {
  const remote = baseDocument({ anchors: ["anchor:row-1", "https://evil.invalid/x"] })
  assertEquals(codes(validateViewDocument(remote, options)).includes("remote_reference"), true)

  const base = baseDocument()
  const row = base.sections[0]?.rows[0]
  if (row === undefined) throw new Error("baseline has no row")
  const actionRemote = baseDocument({
    sections: [{
      id: "queue",
      title: "Queue",
      rows: [{
        ...row,
        actions: retarget(row.actions, "action:expand", "//evil.invalid/x"),
      }],
    }],
  })
  assertEquals(
    codes(validateViewDocument(actionRemote, options)).includes("remote_reference"),
    true,
  )
})

Deno.test("a view that adds executable code to a structural field is rejected", () => {
  const base = baseDocument()
  const row = base.sections[0]?.rows[0]
  if (row === undefined) throw new Error("baseline has no row")
  const coded = baseDocument({
    sections: [{
      id: "queue",
      title: "Queue",
      rows: [{
        ...row,
        actions: retarget(row.actions, "action:expand", "javascript:alert(1)"),
      }],
    }],
  })
  assertEquals(
    codes(validateViewDocument(coded, options)).includes("code_in_structural_field"),
    true,
  )
})

Deno.test("a partial disclosure rule set is rejected as an implicit fallback", () => {
  const partial = baseDocument({
    disclosure: baseDocument().disclosure.filter((rule) => rule.field !== "badges"),
  })
  assertEquals(
    codes(validateViewDocument(partial, options)).includes("disclosure_incomplete"),
    true,
  )
})

Deno.test("a view citing an unauthorized source is rejected", () => {
  const base = baseDocument()
  const row = base.sections[0]?.rows[0]
  if (row === undefined) throw new Error("baseline has no row")
  const foreign = baseDocument({
    sections: [{
      id: "queue",
      title: "Queue",
      rows: [{
        ...row,
        provenance: { ...row.provenance, observationId: "foreign:9" },
      }],
    }],
  })
  assertEquals(
    codes(validateViewDocument(foreign, options)).includes("unauthorized_source"),
    true,
  )
})

Deno.test("a view over budget or claiming currency while building is rejected", () => {
  const base = baseDocument()
  const row = base.sections[0]?.rows[0]
  if (row === undefined) throw new Error("baseline has no row")
  const sections = Array.from({ length: 9 }, (_, index) => ({
    id: `section-${index}`,
    title: `Section ${index}`,
    rows: [{ ...row, id: `row-${index}`, anchor: `anchor:row-${index}` }],
  }))
  assertEquals(
    codes(validateViewDocument(baseDocument({ sections }), options)).includes("budget_exceeded"),
    true,
  )
  const dishonest = baseDocument({
    freshness: { ...base.freshness, pending: 3 },
  })
  assertEquals(
    codes(validateViewDocument(dishonest, options)).includes("freshness_inconsistent"),
    true,
  )
})

Deno.test("unknown fields are rejected at parse instead of silently dropped", () => {
  const parsed = parseViewDocument({ ...baseDocument(), extra: true })
  assertEquals(Result.isFailure(parsed), true)
  if (Result.isFailure(parsed)) assertEquals(parsed.failure.code, "schema_invalid")
  assertEquals(Result.isSuccess(parseViewDocument(baseDocument())), true)
})

Deno.test("a next revision that only changes presentation still validates", () => {
  const next = secondDocument()
  assertEquals(
    Result.isSuccess(validateViewDocument(next, { ...options, base: baseDocument() })),
    true,
  )
})
