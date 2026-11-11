import { Effect, Schema } from "effect"
import { canonicalize } from "../../src/evidence/canonical.ts"
import { sha256Text } from "../../src/evidence/digest.ts"
import { channelProbes, loadCatalogue } from "./probes.ts"
import type { ProbeResult } from "./probes.ts"

/** Channels whose denial is recorded in a native receipt or quarantine row. */
export const RECEIPT_REQUIRED = new Set(["inbox", "chat", "csr"])

export interface RedteamReport {
  readonly schemaVersion: 1
  readonly dataset: string
  readonly suite: string
  readonly cspPolicyDigest: string
  readonly receiptRequiredChannels: ReadonlyArray<string>
  readonly cases: ReadonlyArray<ProbeResult>
  readonly failures: ReadonlyArray<string>
  readonly pass: boolean
  readonly runDigest: string
}

export const runRedteam = async (root: string): Promise<RedteamReport> => {
  const catalogue = await loadCatalogue(root)
  const cases = await channelProbes(root, catalogue)
  const failures = cases.flatMap((probeCase) => {
    const mismatch = probeCase.outcome !== probeCase.expected
      ? [`${probeCase.id}: ${probeCase.outcome} != ${probeCase.expected}`]
      : []
    const missingReceipt = RECEIPT_REQUIRED.has(probeCase.channel) && !probeCase.receipt
      ? [`${probeCase.id}: denial has no receipt`]
      : []
    return [...mismatch, ...missingReceipt]
  })
  const idSet = new Set(cases.map((probeCase) => probeCase.id))
  for (const entry of catalogue.cases) {
    if (!idSet.has(entry.id)) failures.push(`${entry.id}: probe did not run`)
  }
  const sorted = [...cases].toSorted((left, right) => left.id.localeCompare(right.id))
  const runDigest = await Effect.runPromise(
    sha256Text(
      canonicalize(
        Schema.decodeUnknownSync(Schema.Json)({
          dataset: `${catalogue.dataset}@${catalogue.version}`,
          suite: catalogue.suite,
          cspPolicyDigest: catalogue.cspPolicyDigest,
          cases: sorted,
          failures,
        }),
      ),
    ),
  )
  return {
    schemaVersion: 1,
    dataset: `${catalogue.dataset}@${catalogue.version}`,
    suite: catalogue.suite,
    cspPolicyDigest: catalogue.cspPolicyDigest,
    receiptRequiredChannels: [...RECEIPT_REQUIRED].toSorted(),
    cases: sorted,
    failures,
    pass: failures.length === 0,
    runDigest,
  }
}

export const redteamReportText = (report: RedteamReport): string =>
  `${JSON.stringify(report, null, 2)}\n`
