import { Effect, Schema } from "effect"
import { canonicalize } from "../../src/evidence/canonical.ts"
import { sha256Text } from "../../src/evidence/digest.ts"
import { channelProbes, loadCatalogue } from "./probes.ts"
import type { ProbeResult } from "./probes.ts"

/**
 * What each case may claim as its denial evidence. A case with no entry fails the
 * run, so a probe cannot silently assert a receipt the seam never wrote.
 */
export type EvidenceClaim = "receipt" | "quarantine" | "none"

export const EVIDENCE_CLAIMS: Readonly<Record<string, EvidenceClaim>> = {
  "INBOX-CIPHERTEXT-SUBSTITUTION": "quarantine",
  "INBOX-OVERSIZED": "quarantine",
  "INBOX-PATH-TRAVERSAL": "none",
  "INBOX-REPLAY": "none",
  "VAULT-SYMLINK": "none",
  "VAULT-HARDLINK": "none",
  "VAULT-TRAVERSAL": "none",
  "TODO-QUOTA": "none",
  "TODO-SYMLINK": "none",
  "TODO-CONFLICT": "none",
  "CHAT-NO-TOKEN": "receipt",
  "CHAT-BAD-PASSKEY": "receipt",
  "CHAT-FOREIGN-ORIGIN": "receipt",
  "CHAT-OVERSIZED": "receipt",
  "CSR-WRONG-CN": "receipt",
  "CSR-WRONG-SAN": "receipt",
  "CSR-WRONG-KEY": "receipt",
  "CSR-WRONG-SPKI": "receipt",
  "SEARCH-DLP-BYPASS": "receipt",
  "SEARCH-JUDGE-BYPASS": "receipt",
  "SEARCH-CHUNKED-EXFIL": "receipt",
  "SEARCH-BUDGET-RESET": "receipt",
  "SEARCH-MARKUP": "receipt",
  "SEARCH-FORGED-FIELD": "none",
  "CSP-PLANTED-RESOURCES": "none",
  "BOUNDARY-EXTRA-MOUNT": "none",
  "BOUNDARY-EXTRA-SOCKET": "none",
  "BOUNDARY-NIC": "none",
  "BOUNDARY-WEAK-POSTURE": "none",
}

export interface RedteamReport {
  readonly schemaVersion: 1
  readonly dataset: string
  readonly suite: string
  readonly cspPolicyDigest: string
  readonly evidenceClaims: Readonly<Record<string, EvidenceClaim>>
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
    const claim = EVIDENCE_CLAIMS[probeCase.id]
    const claimFailure = claim === undefined
      ? [`${probeCase.id}: no evidence claim`]
      : claim === "none"
      ? probeCase.receipt ? [`${probeCase.id}: claims no receipt but reports one`] : []
      : probeCase.receipt
      ? []
      : [`${probeCase.id}: claims ${claim} evidence but the seam wrote none`]
    return [...mismatch, ...claimFailure]
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
    evidenceClaims: { ...EVIDENCE_CLAIMS },
    cases: sorted,
    failures,
    pass: failures.length === 0,
    runDigest,
  }
}

export const redteamReportText = (report: RedteamReport): string =>
  `${JSON.stringify(report, null, 2)}\n`
