import { assertEquals } from "@std/assert"
import { matchesVersion, RUNTIME_REQUESTS } from "../../../src/spikes/language/resolve.ts"

Deno.test("runtime resolution rejects a foreign version from a compatibility shim", () => {
  assertEquals(matchesVersion(RUNTIME_REQUESTS.node, "deno 2.9.2 (stable)"), false)
  assertEquals(matchesVersion(RUNTIME_REQUESTS.node, "v24.21.0"), true)
  assertEquals(matchesVersion(RUNTIME_REQUESTS.deno, "deno 2.9.2 (stable)"), true)
  assertEquals(matchesVersion(RUNTIME_REQUESTS.cpython, "Python 3.14.8"), true)
  assertEquals(matchesVersion(RUNTIME_REQUESTS["lua5.4"], "Lua 5.4.9"), true)
  assertEquals(matchesVersion(RUNTIME_REQUESTS.bash, "GNU bash, version 5.3.9"), true)
  assertEquals(matchesVersion(RUNTIME_REQUESTS.cpython, "v24.21.0"), false)
})
