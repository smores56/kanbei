// Landlock enforcement child. It must not use Deno's file APIs after
// `landlock_restrict_self`: the runtime's own descriptor bookkeeping would
// interfere, so results are emitted through libc write(2) only.
//
// usage: landlock-child.ts <fs> <insideDir> <outsideFile>
//        landlock-child.ts <net>
const libc = Deno.dlopen("libc.so.6", {
  syscall: {
    parameters: ["i64", "i64", "i64", "i64", "i64", "i64"],
    result: "i64",
    variadic: true,
  },
  prctl: { parameters: ["i32", "u64", "u64", "u64", "u64"], result: "i32" },
  open: { parameters: ["i64", "i32"], result: "i32" },
  read: { parameters: ["i32", "i64", "usize"], result: "isize" },
  write: { parameters: ["i32", "i64", "usize"], result: "isize" },
  close: { parameters: ["i32"], result: "i32" },
})

type Symbols = typeof libc.symbols
const symbols: Symbols = libc.symbols

const encoder = new TextEncoder()
const pointerOf = (bytes: Uint8Array): bigint =>
  Deno.UnsafePointer.value(Deno.UnsafePointer.of(bytes))
const emit = (value: unknown): void => {
  const bytes = encoder.encode(`${JSON.stringify(value)}\n`)
  symbols.write(1, pointerOf(bytes), BigInt(bytes.length))
}
const cString = (text: string): Uint8Array => {
  const bytes = new Uint8Array(text.length + 1)
  bytes.set(encoder.encode(text))
  return bytes
}

const LANDLOCK_CREATE_RULESET = 444n
const LANDLOCK_ADD_RULE = 445n
const LANDLOCK_RESTRICT_SELF = 446n
const LANDLOCK_RULE_PATH_BENEATH = 1

const FS = {
  execute: 1n << 0n,
  writeFile: 1n << 1n,
  readFile: 1n << 2n,
  readDir: 1n << 3n,
  removeDir: 1n << 4n,
  removeFile: 1n << 5n,
  makeChar: 1n << 6n,
  makeDir: 1n << 7n,
  makeReg: 1n << 8n,
  makeSock: 1n << 9n,
  makeFifo: 1n << 10n,
  makeBlock: 1n << 11n,
  makeSym: 1n << 12n,
  refer: 1n << 13n,
  truncate: 1n << 14n,
  ioctlDev: 1n << 15n,
} as const

const NET = { bindTcp: 1n, connectTcp: 2n } as const

const allFs = Object.values(FS).reduce((left, right) => left | right, 0n)

const abi = Number(symbols.syscall(LANDLOCK_CREATE_RULESET, 0n, 0n, 1n, 0n, 0n))

const createRuleset = (fsMask: bigint, netMask: bigint): number => {
  const attr = new BigUint64Array(3)
  attr[0] = fsMask
  attr[1] = netMask
  const bytes = new Uint8Array(attr.buffer)
  const size = netMask === 0n ? 8 : 16
  return Number(
    symbols.syscall(LANDLOCK_CREATE_RULESET, pointerOf(bytes), BigInt(size), 0n, 0n, 0n),
  )
}

const mode = Deno.args[0]

if (mode === "fs") {
  const insideDir = Deno.args[1] ?? ""
  const outsideFile = Deno.args[2] ?? ""
  const masks = [allFs, 0x7fffn, 0x7ffn, 0x7n]
  const created = masks
    .map((mask) => ({ mask, fd: createRuleset(mask, 0n) }))
    .find((candidate) => candidate.fd >= 0)
  if (created === undefined) {
    emit({ abi, mode, created: false })
  } else {
    const dirFd = symbols.open(pointerOf(cString(insideDir)), 0)
    const pathBeneath = new Uint8Array(16)
    const view = new DataView(pathBeneath.buffer)
    view.setBigUint64(0, created.mask, true)
    view.setInt32(8, dirFd, true)
    const addRule = Number(
      symbols.syscall(
        LANDLOCK_ADD_RULE,
        BigInt(created.fd),
        BigInt(LANDLOCK_RULE_PATH_BENEATH),
        pointerOf(pathBeneath),
        0n,
        0n,
      ),
    )
    const noNewPrivs = symbols.prctl(38, 1n, 0n, 0n, 0n)
    const restrict = Number(
      symbols.syscall(LANDLOCK_RESTRICT_SELF, BigInt(created.fd), 0n, 0n, 0n, 0n),
    )
    const insideFd = symbols.open(pointerOf(cString(`${insideDir}/allowed.txt`)), 0)
    const outsideFd = symbols.open(pointerOf(cString(outsideFile)), 0)
    const buffer = new Uint8Array(64)
    const readCount = insideFd >= 0 ? Number(symbols.read(insideFd, pointerOf(buffer), 64n)) : -1
    emit({
      abi,
      mode,
      created: true,
      mask: created.mask.toString(),
      addRule,
      noNewPrivs,
      restrict,
      insideOpened: insideFd >= 0,
      outsideOpened: outsideFd >= 0,
      insideContent: new TextDecoder().decode(buffer.subarray(0, Math.max(readCount, 0))).trim(),
    })
  }
} else if (mode === "net") {
  const fd = createRuleset(0n, NET.bindTcp | NET.connectTcp)
  if (fd < 0) {
    emit({ abi, mode, created: false })
  } else {
    const noNewPrivs = symbols.prctl(38, 1n, 0n, 0n, 0n)
    const restrict = Number(symbols.syscall(LANDLOCK_RESTRICT_SELF, BigInt(fd), 0n, 0n, 0n, 0n))
    try {
      const listener = Deno.listen({ hostname: "127.0.0.1", port: 0 })
      listener.close()
      emit({ abi, mode, created: true, noNewPrivs, restrict, bind: "allowed" })
    } catch (error) {
      emit({
        abi,
        mode,
        created: true,
        noNewPrivs,
        restrict,
        bind: "denied",
        error: String(error).slice(0, 120),
      })
    }
  }
} else {
  emit({ error: `unknown mode ${String(mode)}` })
}
