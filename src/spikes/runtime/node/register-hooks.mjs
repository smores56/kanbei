import { createRequire, registerHooks } from "node:module"
import { join } from "node:path"
import { pathToFileURL } from "node:url"

/**
 * Node-side module-resolution boundary.
 *
 * The repo's Deno import map resolves `effect` from `deno.json`. Node has no
 * import map and the repository has no `node_modules` by design, so the spike
 * installs the same pinned package under `tmp/spikes/runtime/node-deps` and maps
 * the bare specifier here. Nothing outside this file knows about it.
 *
 * The target is resolved before the hooks are registered: resolving from inside
 * the hook re-enters the hook and overflows the stack.
 */
const root = process.env.KANBEI_NODE_MODULES

if (root !== undefined && root !== "") {
  const requireFromRoot = createRequire(join(root, "noop.cjs"))
  const effectEntry = pathToFileURL(requireFromRoot.resolve("effect")).href

  registerHooks({
    resolve(specifier, context, nextResolve) {
      if (specifier === "effect") return nextResolve(effectEntry, context)
      return nextResolve(specifier, context)
    },
  })
}
