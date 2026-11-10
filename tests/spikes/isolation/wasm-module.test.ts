import { assertEquals, assertThrows } from "@std/assert"
import {
  buildAgentModule,
  buildComputeOnlyModule,
  buildStoreModule,
} from "../../../src/spikes/isolation/wasm-module.ts"

const exportsOf = (instance: WebAssembly.Instance) => {
  const read = (name: string): (argument: number) => number => {
    const value: unknown = instance.exports[name]
    if (typeof value !== "function") throw new Error(`missing export ${name}`)
    return (argument: number) => Number(Reflect.apply(value, undefined, [argument]))
  }
  return { read }
}

const instantiateAgent = async () => {
  const module = new WebAssembly.Module(buildAgentModule(16))
  const instance = await WebAssembly.instantiate(module, {
    kanbei: { ping: (value: number) => value + 1 },
  })
  return { module, instance, exports: exportsOf(instance) }
}

Deno.test("agent module exposes exactly one integer host import", async () => {
  const { module } = await instantiateAgent()
  assertEquals(WebAssembly.Module.imports(module), [
    { module: "kanbei", name: "ping", kind: "function" },
  ])
})

Deno.test("compute-only module exposes no imports", () => {
  const module = new WebAssembly.Module(buildComputeOnlyModule())
  assertEquals(WebAssembly.Module.imports(module).length, 0)
})

Deno.test("spin accumulates the same checksum as the process-arm workload", async () => {
  const { exports } = await instantiateAgent()
  assertEquals(exports.read("spin")(1000), 500500)
})

Deno.test("host-call loop drives the single host import", async () => {
  const { exports } = await instantiateAgent()
  assertEquals(exports.read("callHost")(100), 5150)
})

Deno.test("linear memory growth is refused beyond the declared maximum", async () => {
  const { instance, exports } = await instantiateAgent()
  const memory = instance.exports.memory
  if (!(memory instanceof WebAssembly.Memory)) throw new Error("missing memory")
  assertEquals(exports.read("alloc")(8), 1)
  assertEquals(exports.read("alloc")(100), -1)
  assertEquals(memory.buffer.byteLength / 65536, 9)
})

Deno.test("unreachable traps as RuntimeError and leaves the instance callable", async () => {
  const { exports } = await instantiateAgent()
  assertThrows(() => exports.read("boom")(0), WebAssembly.RuntimeError)
  assertEquals(exports.read("spin")(10), 55)
})

Deno.test("store module writes into its own linear memory", async () => {
  const module = new WebAssembly.Module(buildStoreModule())
  const instance = await WebAssembly.instantiate(module, {})
  const store = exportsOf(instance).read("store")
  store(0)
  const memory = instance.exports.memory
  if (!(memory instanceof WebAssembly.Memory)) throw new Error("missing memory")
  assertEquals(new Uint32Array(memory.buffer)[0], 0x41)
})
