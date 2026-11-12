/**
 * Minimal WebAssembly binary encoder for the S3 probe modules.
 *
 * The spike must run on a host without a WASM toolchain, so the probe modules
 * are assembled here from raw opcodes instead of being compiled from `.wat`.
 * Only the instruction subset exercised by the probes is emitted.
 */

const I32 = 0x7f
const VOID: ReadonlyArray<number> = []

const uleb = (value: number): ReadonlyArray<number> =>
  value < 0x80 ? [value] : [(value & 0x7f) | 0x80, ...uleb(Math.floor(value / 128))]

const concat = (parts: ReadonlyArray<ReadonlyArray<number>>): ReadonlyArray<number> => parts.flat()

const vector = (items: ReadonlyArray<ReadonlyArray<number>>): ReadonlyArray<number> =>
  concat([uleb(items.length), ...items])

const encodedName = (text: string): ReadonlyArray<number> => {
  const bytes = [...new TextEncoder().encode(text)]
  return concat([uleb(bytes.length), bytes])
}

const section = (id: number, payload: ReadonlyArray<number>): ReadonlyArray<number> =>
  concat([[id], uleb(payload.length), payload])

const funcType = (
  params: ReadonlyArray<number>,
  results: ReadonlyArray<number>,
): ReadonlyArray<number> =>
  concat([
    [0x60],
    vector(params.map((param) => [param])),
    vector(results.map((result) => [result])),
  ])

export interface FunctionImport {
  readonly module: string
  readonly name: string
  readonly typeIndex: number
}

export interface FunctionExport {
  readonly name: string
  readonly kind: "func" | "memory"
  readonly index: number
}

export interface ModuleSpec {
  readonly types: ReadonlyArray<ReadonlyArray<number>>
  readonly imports: ReadonlyArray<FunctionImport>
  readonly functions: ReadonlyArray<number>
  readonly memory?: { readonly min: number; readonly max: number }
  readonly exports: ReadonlyArray<FunctionExport>
  /** One body per defined function, in definition order, including the locals vector. */
  readonly bodies: ReadonlyArray<ReadonlyArray<number>>
}

const importEntry = (entry: FunctionImport): ReadonlyArray<number> =>
  concat([encodedName(entry.module), encodedName(entry.name), [0x00], uleb(entry.typeIndex)])

const exportEntry = (entry: FunctionExport): ReadonlyArray<number> =>
  concat([
    encodedName(entry.name),
    [entry.kind === "func" ? 0x00 : 0x02],
    uleb(entry.index),
  ])

const memorySection = (
  memory: { readonly min: number; readonly max: number },
): ReadonlyArray<number> => vector([concat([[0x01], uleb(memory.min), uleb(memory.max)])])

const codeEntry = (body: ReadonlyArray<number>): ReadonlyArray<number> =>
  concat([uleb(body.length), body])

export const encodeModule = (spec: ModuleSpec): Uint8Array<ArrayBuffer> => {
  const sections = concat([
    section(1, vector(spec.types)),
    spec.imports.length === 0 ? [] : section(2, vector(spec.imports.map(importEntry))),
    section(3, vector(spec.functions.map((typeIndex) => uleb(typeIndex)))),
    spec.memory === undefined ? [] : section(5, memorySection(spec.memory)),
    section(7, vector(spec.exports.map(exportEntry))),
    section(10, vector(spec.bodies.map(codeEntry))),
  ])
  return new Uint8Array([0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, ...sections])
}

/** Accumulation loop: local0 counts down, local1 accumulates. */
const accumulateBody = (term: ReadonlyArray<number>): ReadonlyArray<number> =>
  concat([
    [0x01, 0x01, I32],
    [0x02, 0x40],
    [0x03, 0x40],
    [0x20, 0x00],
    [0x45],
    [0x0d, 0x01],
    [0x20, 0x01],
    term,
    [0x6a],
    [0x21, 0x01],
    [0x20, 0x00],
    [0x41, 0x01],
    [0x6b],
    [0x21, 0x00],
    [0x0c, 0x00],
    [0x0b],
    [0x0b],
    [0x20, 0x01],
    [0x0b],
  ])

const trapBody: ReadonlyArray<number> = [0x00, 0x00, 0x0b]

const hangBody: ReadonlyArray<number> = [0x00, 0x03, 0x40, 0x0c, 0x00, 0x0b, 0x0b]

const growBody: ReadonlyArray<number> = [0x00, 0x20, 0x00, 0x40, 0x00, 0x0b]

const storeBody: ReadonlyArray<number> = [
  0x00,
  0x41,
  0x00,
  0x41,
  0xc1,
  0x00,
  0x36,
  0x02,
  0x00,
  0x0b,
]

export const HOST_IMPORT_MODULE = "kanbei"
export const HOST_IMPORT_NAME = "ping"

/**
 * Single module for the WASM arm: compute loop, host-call loop, memory growth,
 * trap, and an infinite loop. One import, so the entire host ABI is one
 * integer-returning function.
 */
export const buildAgentModule = (maxPages: number): Uint8Array<ArrayBuffer> =>
  encodeModule({
    types: [funcType([I32], [I32]), funcType(VOID, VOID)],
    imports: [{ module: HOST_IMPORT_MODULE, name: HOST_IMPORT_NAME, typeIndex: 0 }],
    functions: [0, 0, 0, 1, 1],
    memory: { min: 1, max: maxPages },
    exports: [
      { name: "spin", kind: "func", index: 1 },
      { name: "callHost", kind: "func", index: 2 },
      { name: "alloc", kind: "func", index: 3 },
      { name: "boom", kind: "func", index: 4 },
      { name: "hang", kind: "func", index: 5 },
      { name: "memory", kind: "memory", index: 0 },
    ],
    bodies: [
      accumulateBody([0x20, 0x00]),
      accumulateBody([0x20, 0x00, 0x10, 0x00]),
      growBody,
      trapBody,
      hangBody,
    ],
  })

/** Compute-only module with no imports, used to prove the host ABI can be empty. */
export const buildComputeOnlyModule = (): Uint8Array<ArrayBuffer> =>
  encodeModule({
    types: [funcType([I32], [I32])],
    imports: [],
    functions: [0],
    exports: [{ name: "spin", kind: "func", index: 0 }],
    bodies: [accumulateBody([0x20, 0x00])],
  })

export const buildStoreModule = (): Uint8Array<ArrayBuffer> =>
  encodeModule({
    types: [funcType(VOID, VOID)],
    imports: [],
    functions: [0],
    memory: { min: 1, max: 1 },
    exports: [
      { name: "store", kind: "func", index: 0 },
      { name: "memory", kind: "memory", index: 0 },
    ],
    bodies: [storeBody],
  })
