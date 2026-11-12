import type { SpikeResultDocument } from "./schema.ts"

/**
 * The security probes measure the runtime's own capability boundary. They say
 * nothing about whether an OS sandbox holds, which is a separate deployment
 * finding (`DEP-1` / `DG-3`).
 */
export const SECURITY_SCOPE =
  "Covers inner runtime capability denial only; OS-sandbox enforcement remains DEP-1/DG-3."

export const renderSecurityEnvelope = (
  document: Pick<SpikeResultDocument, "securityEnvelope">,
): string => {
  const envelope = document.securityEnvelope
  if (envelope === undefined) {
    return ["## Security evidence", "", "No security envelope was emitted.", ""].join("\n")
  }
  return [
    "## Security evidence",
    "",
    `- test: \`${envelope.testId}\``,
    `- fixture: \`${envelope.fixtureVersion}\``,
    `- outcome: \`${envelope.outcome}\``,
    `- covered probes: ${envelope.invariantIds.join(", ")}`,
    `- scope: ${SECURITY_SCOPE}`,
    "",
  ].join("\n")
}
