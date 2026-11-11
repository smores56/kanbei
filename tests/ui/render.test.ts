import { assertEquals } from "@std/assert"
import { Effect, Result } from "effect"
import { makeSyntheticNetwork } from "../../src/evidence/synthetic-network.ts"
import { CONTENT_SECURITY_POLICY } from "../../src/ui/csp.ts"
import { renderModelText, renderView } from "../../src/ui/render.ts"
import { baseDocument } from "./support.ts"

const render = (document: ReturnType<typeof baseDocument>) => {
  const rendered = renderView(document)
  if (Result.isFailure(rendered)) throw rendered.failure
  return rendered.success
}

const withCellText = (text: string) => {
  const base = baseDocument()
  const row = base.sections[0]?.rows[0]
  if (row === undefined) throw new Error("baseline document has no row")
  return baseDocument({
    sections: [{
      id: "queue",
      title: "Queue",
      rows: [{
        ...row,
        cells: [{ label: "subject", text }],
      }],
    }],
  })
}

Deno.test("the static shell serves the CSP of 07 4.11.7 verbatim", () => {
  assertEquals(
    CONTENT_SECURITY_POLICY,
    "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self' data:; " +
      "connect-src 'self'; form-action 'self'; frame-ancestors 'none'; base-uri 'none'",
  )
})

Deno.test("a declarative view renders to a stable inert-text snapshot", () => {
  const rendered = render(baseDocument())
  assertEquals(rendered.mediaType, "text/plain; charset=utf-8")
  assertEquals(rendered.neutralized, 0)
  assertEquals(
    rendered.text,
    [
      "decisions decisions rev rev-1",
      "freshness: current observed 2026-11-11T00:00:00.000Z window 3600000ms pending 0 " +
      "watermark 4",
      "neutralized: 0",
      "Decision queue",
      "== Queue ==",
      "- row-1 (anchor:row-1)",
      "  subject: Renew the lease",
      "  badges: current",
      "  actions: [rule] Approve (action:approve) [link-consent] Open consent " +
      "(action:consent) [expand] Expand (action:expand)",
      "  provenance: observation=obs:1 span=bytes 0-120 derivation=capture scope=personal",
    ].join("\n"),
  )
})

Deno.test("a planted remote image and iframe make zero requests and zero navigations", async () => {
  const network = await Effect.runPromise(makeSyntheticNetwork())
  await Effect.runPromise(network.serve("https://example.invalid/x.png", {
    status: 200,
    headers: {},
    body: "image",
  }))
  const rendered = render(
    withCellText(
      '![x](https://example.invalid/x.png) <iframe src="https://example.invalid/y"></iframe>',
    ),
  )
  const calls = await Effect.runPromise(network.calls)
  assertEquals(calls.length, 0)
  assertEquals(rendered.text.includes("https://example.invalid/x.png"), true)
  assertEquals(rendered.mediaType, "text/plain; charset=utf-8")
  assertEquals(rendered.lines.some((line) => line.startsWith("<")), false)
})

Deno.test("a planted javascript link stays inert text and cannot become a target", () => {
  const text = renderModelText("[x](javascript:alert(1))")
  assertEquals(text.text, "[x](javascript:alert(1))")
  assertEquals(text.neutralized, true)

  const base = baseDocument()
  const row = base.sections[0]?.rows[0]
  if (row === undefined) throw new Error("baseline document has no row")
  const rendered = renderView(baseDocument({
    sections: [{
      id: "queue",
      title: "Queue",
      rows: [{
        ...row,
        actions: [
          ...row.actions,
          {
            id: "action:evil",
            kind: "navigate",
            label: "Go",
            target: "javascript:alert(1)",
          },
        ],
      }],
    }],
  }))
  assertEquals(Result.isFailure(rendered), true)
  if (Result.isFailure(rendered)) assertEquals(rendered.failure.code, "dangerous_scheme")
})

Deno.test("HTML entities and markup render literally without decoding", () => {
  const entity = renderModelText("&lt;script&gt;alert(1)&lt;/script&gt;")
  assertEquals(entity.text, "&lt;script&gt;alert(1)&lt;/script&gt;")
  assertEquals(entity.neutralized, false)

  const rendered = render(withCellText("<b>bold</b> &amp; <script>alert(1)</script>"))
  assertEquals(rendered.text.includes("<b>bold</b> &amp; <script>alert(1)</script>"), true)
  assertEquals(rendered.mediaType, "text/plain; charset=utf-8")
})

Deno.test("long content truncates at the cell limit with a visible marker", () => {
  const rendered = render(withCellText("a".repeat(500)))
  assertEquals(rendered.truncated >= 1, true)
  assertEquals(rendered.text.includes("…[truncated]"), true)
  assertEquals(rendered.text.includes("a".repeat(121)), false)
})

Deno.test("a stale or unknown source is never rendered as current", () => {
  const stale = render(baseDocument({
    freshness: { ...baseDocument().freshness, state: "stale" },
  }))
  assertEquals(stale.lines[1]?.startsWith("freshness: stale"), true)
  const unknown = render(baseDocument({
    freshness: { ...baseDocument().freshness, state: "unknown", pending: 2 },
  }))
  assertEquals(unknown.lines[1]?.startsWith("freshness: unknown"), true)
  assertEquals(unknown.lines[1]?.includes("current"), false)
})

Deno.test("control and bidi characters are neutralized in text", () => {
  const text = renderModelText("safe\u0000text\u202Eevil")
  assertEquals(text.text, "safe text evil")
})
