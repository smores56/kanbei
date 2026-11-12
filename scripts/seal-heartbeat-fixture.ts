/**
 * Seals the host-format heartbeat fixture with the ingest-v1 age identity.
 *
 * The host writes `heartbeat-<epoch>.age` into the same read-only Inbox share as
 * the batches (`nix-config modules/lib/ingest/heartbeat.py`); this fixture is
 * that envelope's shape, not a reader-invented one. age encryption is
 * randomized, so this runs once and the ciphertext is committed.
 *
 *   deno run -A scripts/seal-heartbeat-fixture.ts
 */
const root = "fixtures/inbox-v1/wire"
const identity = "fixtures/ingest-v1/keys/identity.txt"
const recipient = (await Deno.readTextFile("fixtures/ingest-v1/keys/recipient.txt")).trim()
const plaintext = await Deno.readTextFile(`${root}/host-heartbeat-1.json`)

const child = new Deno.Command("age", {
  args: ["--encrypt", "--recipient", recipient, "--output", `${root}/host-heartbeat-1.age`],
  stdin: "piped",
}).spawn()
const writer = child.stdin.getWriter()
await writer.write(new TextEncoder().encode(plaintext))
await writer.close()
const status = await child.status
if (status.code !== 0) throw new Error(`age exited ${status.code}`)

// The identity is committed so the reader test can open the fixture.
const identityText = await Deno.readTextFile(identity)
if (identityText.trim() === "") throw new Error(`${identity} is empty`)
