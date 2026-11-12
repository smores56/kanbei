// Trusted loader for the A2b sandbox. It must apply the ruleset before the
// untrusted agent module runs; after that point it must not relax the ruleset.
const libc = Deno.dlopen("libc.so.6", {
  syscall: {
    parameters: ["i64", "i64", "i64", "i64", "i64", "i64"],
    result: "i64",
    variadic: true,
  },
  prctl: { parameters: ["i32", "u64", "u64", "u64", "u64"], result: "i32" },
  open: { parameters: ["i64", "i32"], result: "i32" },
  close: { parameters: ["i32"], result: "i32" },
  write: { parameters: ["i32", "i64", "usize"], result: "isize" },
  execve: { parameters: ["i64", "i64", "i64"], result: "i32" },
})

const symbols = libc.symbols
const encoder = new TextEncoder()
const pointerOf = (bytes: Uint8Array): bigint =>
  Deno.UnsafePointer.value(Deno.UnsafePointer.of(bytes))

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

const READ = FS.execute | FS.readFile | FS.readDir
const WRITE = Object.values(FS).reduce((left, right) => left | right, 0n)

export interface LandlockRule {
  readonly path: string
  readonly access: "read" | "write"
}

/** `landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION)`; 0 when unsupported. */
export const probeLandlockAbi = (): number => {
  const abi = Number(symbols.syscall(LANDLOCK_CREATE_RULESET, 0n, 0n, 1n, 0n, 0n))
  return abi < 0 ? 0 : abi
}

export const applyLandlock = (rules: ReadonlyArray<LandlockRule>): number => {
  const abi = probeLandlockAbi()
  const attr = new BigUint64Array([WRITE, 0n])
  const ruleset = Number(
    symbols.syscall(
      LANDLOCK_CREATE_RULESET,
      pointerOf(new Uint8Array(attr.buffer)),
      8n,
      0n,
      0n,
      0n,
    ),
  )
  if (ruleset < 0) throw new Error(`landlock_create_ruleset failed: ${ruleset}`)

  for (const rule of rules) {
    const fd = symbols.open(pointerOf(cString(rule.path)), 0)
    if (fd < 0) throw new Error(`cannot open Landlock path ${rule.path}: ${fd}`)
    const pathBeneath = new Uint8Array(16)
    const view = new DataView(pathBeneath.buffer)
    view.setBigUint64(0, rule.access === "read" ? READ : WRITE, true)
    view.setInt32(8, fd, true)
    const added = Number(
      symbols.syscall(
        LANDLOCK_ADD_RULE,
        BigInt(ruleset),
        BigInt(LANDLOCK_RULE_PATH_BENEATH),
        pointerOf(pathBeneath),
        0n,
        0n,
      ),
    )
    symbols.close(fd)
    if (added !== 0) throw new Error(`landlock_add_rule failed for ${rule.path}: ${added}`)
  }

  const noNewPrivs = symbols.prctl(38, 1n, 0n, 0n, 0n)
  if (noNewPrivs !== 0) throw new Error(`PR_SET_NO_NEW_PRIVS failed: ${noNewPrivs}`)
  const restricted = Number(
    symbols.syscall(LANDLOCK_RESTRICT_SELF, BigInt(ruleset), 0n, 0n, 0n, 0n),
  )
  symbols.close(ruleset)
  if (restricted !== 0) throw new Error(`landlock_restrict_self failed: ${restricted}`)
  return abi
}

const pointerArray = (items: ReadonlyArray<Uint8Array>): Uint8Array => {
  const pointers = new BigUint64Array(items.length + 1)
  items.forEach((item, index) => {
    pointers[index] = BigInt(Deno.UnsafePointer.value(Deno.UnsafePointer.of(item)))
  })
  return new Uint8Array(pointers.buffer)
}

/** Replaces the current image so the restricted process is the agent itself. */
export const execveProcess = (
  program: string,
  args: ReadonlyArray<string>,
  env: Readonly<Record<string, string>>,
): never => {
  const argv = pointerArray([cString(program), ...args.map(cString)])
  const envp = pointerArray(Object.entries(env).map(([key, value]) => cString(`${key}=${value}`)))
  symbols.execve(pointerOf(cString(program)), pointerOf(argv), pointerOf(envp))
  throw new Error(`execve ${program} failed`)
}

export const emit = (value: unknown): void => {
  const bytes = encoder.encode(`${JSON.stringify(value)}\n`)
  symbols.write(1, pointerOf(bytes), BigInt(bytes.length))
}
