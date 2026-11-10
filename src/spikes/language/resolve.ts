import { Effect } from "effect"
import { CommandRunner } from "./process.ts"
import type { RuntimeVariantId } from "./types.ts"
import type { RuntimeResolution } from "./types.ts"

export interface RuntimeRequest {
  readonly variant: RuntimeVariantId
  readonly commandName: string
  readonly versionArgs: ReadonlyArray<string>
  readonly nixInstallable: string
  readonly nixExecutable: string
  readonly versionPattern: RegExp
}

/** Rejects version-compatibility shims (for example Deno's `node` shim) as if they were the real runtime. */
export const matchesVersion = (request: RuntimeRequest, version: string): boolean =>
  request.versionPattern.test(version)

const NIX_ARGS = ["--extra-experimental-features", "nix-command flakes"] as const

const firstLine = (value: string): string => {
  const line = value
    .split("\n")
    .map((entry) => entry.trim())
    .find((entry) => entry.length > 0)
  return line ?? ""
}

const versionFromStorePath = (binaryPath: string): string | undefined => {
  const match = /\/nix\/store\/[^/]+?-([0-9][^/]*)\/bin\//u.exec(binaryPath)
  return match?.[1]
}

const commandVersion = (
  binaryPath: string,
  args: ReadonlyArray<string>,
  env: Readonly<Record<string, string>>,
): Effect.Effect<string | undefined, never, CommandRunner> =>
  Effect.gen(function* () {
    const runner = yield* CommandRunner
    const outcome = yield* runner
      .run({ argv: [binaryPath, ...args], env, timeoutMs: 30_000 })
      .pipe(Effect.catch(() => Effect.succeed(undefined)))
    if (outcome === undefined || outcome.exitCode !== 0) return undefined
    const line = firstLine(outcome.stdout)
    return line.length === 0 ? undefined : line
  })

const available = (
  variant: RuntimeVariantId,
  binaryPath: string,
  version: string,
  source: "path" | "nixpkgs",
  env: Readonly<Record<string, string>>,
): RuntimeResolution => ({ status: "available", variant, binaryPath, version, source, env })

const unavailable = (
  variant: RuntimeVariantId,
  reason: string,
  missingInput: string,
  log: ReadonlyArray<string>,
): RuntimeResolution => ({ status: "unavailable", variant, reason, missingInput, log: [...log] })

const nixOutPath = (
  installable: string,
): Effect.Effect<string | undefined, never, CommandRunner> =>
  Effect.gen(function* () {
    const runner = yield* CommandRunner
    const outcome = yield* runner
      .run({
        argv: ["nix", ...NIX_ARGS, "build", "--no-link", "--print-out-paths", installable],
        timeoutMs: 240_000,
      })
      .pipe(Effect.catch(() => Effect.succeed(undefined)))
    if (outcome === undefined || outcome.exitCode !== 0) return undefined
    const path = firstLine(outcome.stdout)
    return path.length === 0 ? undefined : path
  })

/** Resolve an installed binary first, then a pinned nixpkgs binary. Never fails; misses are recorded. */
export const resolveRuntime = (
  request: RuntimeRequest,
): Effect.Effect<RuntimeResolution, never, CommandRunner> =>
  Effect.gen(function* () {
    const runner = yield* CommandRunner
    const log: Array<string> = []

    const lookup = yield* runner
      .run({
        argv: ["bash", "-c", `command -v -- ${request.commandName}`],
        timeoutMs: 10_000,
      })
      .pipe(Effect.catch(() => Effect.succeed(undefined)))

    const pathCandidate = lookup !== undefined && lookup.exitCode === 0
      ? firstLine(lookup.stdout)
      : ""

    if (pathCandidate.length > 0) {
      const version = yield* commandVersion(pathCandidate, request.versionArgs, {})
      if (version !== undefined && matchesVersion(request, version)) {
        return available(request.variant, pathCandidate, version, "path", {})
      }
      if (version !== undefined) {
        log.push(`path candidate ${pathCandidate} reported a non-matching version: ${version}`)
      } else {
        log.push(`path candidate ${pathCandidate} did not report a version`)
      }
    } else {
      log.push(`command -v ${request.commandName} found nothing`)
    }

    const outPath = yield* nixOutPath(request.nixInstallable)
    if (outPath !== undefined) {
      const binaryPath = `${outPath}/bin/${request.nixExecutable}`
      const exists = yield* runner
        .run({ argv: ["bash", "-c", `test -x ${binaryPath}`], timeoutMs: 5_000 })
        .pipe(Effect.catch(() => Effect.succeed(undefined)))
      if (exists !== undefined && exists.exitCode === 0) {
        const probedVersion = yield* commandVersion(binaryPath, request.versionArgs, {})
        const version = probedVersion !== undefined && matchesVersion(request, probedVersion)
          ? probedVersion
          : versionFromStorePath(binaryPath)
        if (version !== undefined) {
          return available(request.variant, binaryPath, version, "nixpkgs", {})
        }
        log.push(`nixpkgs candidate ${binaryPath} did not report a version`)
      } else {
        log.push(`nixpkgs output ${binaryPath} is not executable`)
      }
    } else {
      log.push(`nix build ${request.nixInstallable} failed or nix is unavailable`)
    }

    return unavailable(
      request.variant,
      `runtime ${request.commandName} is not installed and could not be resolved from ${request.nixInstallable}`,
      `install ${request.commandName} on PATH or provide ${request.nixInstallable}`,
      log,
    )
  })

export const RUNTIME_REQUESTS: Readonly<
  Record<Exclude<RuntimeVariantId, "monty">, RuntimeRequest>
> = {
  deno: {
    variant: "deno",
    versionPattern: /^deno /u,
    commandName: "deno",
    versionArgs: ["--version"],
    nixInstallable: "nixpkgs#deno",
    nixExecutable: "deno",
  },
  node: {
    variant: "node",
    versionPattern: /^v\d+\./u,
    commandName: "node",
    versionArgs: ["--version"],
    nixInstallable: "nixpkgs#nodejs",
    nixExecutable: "node",
  },
  "lua5.4": {
    variant: "lua5.4",
    versionPattern: /^Lua /u,
    commandName: "lua",
    versionArgs: ["-v"],
    nixInstallable: "nixpkgs#lua5_4",
    nixExecutable: "lua",
  },
  luau: {
    variant: "luau",
    versionPattern: /^\d+\./u,
    commandName: "luau",
    versionArgs: ["--version"],
    nixInstallable: "nixpkgs#luau",
    nixExecutable: "luau",
  },
  bash: {
    variant: "bash",
    versionPattern: /^GNU bash/u,
    commandName: "bash",
    versionArgs: ["--version"],
    nixInstallable: "nixpkgs#bash",
    nixExecutable: "bash",
  },
  cpython: {
    variant: "cpython",
    versionPattern: /^Python /u,
    commandName: "python3",
    versionArgs: ["--version"],
    nixInstallable: "nixpkgs#python3",
    nixExecutable: "python3",
  },
}

const MONTY_PACKAGE = "nixpkgs#python3Packages.pydantic-monty"
const TYPING_EXTENSIONS = "nixpkgs#python3Packages.typing-extensions"

const sitePackagesOf = (
  storePath: string,
): Effect.Effect<string | undefined, never, CommandRunner> =>
  Effect.gen(function* () {
    const runner = yield* CommandRunner
    const outcome = yield* runner
      .run({
        argv: ["bash", "-c", `ls -d ${storePath}/lib/python*/site-packages 2>/dev/null | head -1`],
        timeoutMs: 10_000,
      })
      .pipe(Effect.catch(() => Effect.succeed(undefined)))
    if (outcome === undefined || outcome.exitCode !== 0) return undefined
    const path = firstLine(outcome.stdout)
    return path.length === 0 ? undefined : path
  })

/** Monty is a Python package, so the variant resolves CPython plus the two site-package closures. */
export const resolveMonty = (): Effect.Effect<RuntimeResolution, never, CommandRunner> =>
  Effect.gen(function* () {
    const runner = yield* CommandRunner
    const cpython = yield* resolveRuntime(RUNTIME_REQUESTS.cpython)
    if (cpython.status !== "available") {
      return unavailable(
        "monty",
        "CPython is required to host pydantic-monty",
        `install python3 on PATH or provide nixpkgs#python3 for ${MONTY_PACKAGE}`,
        [cpython.status === "unavailable" ? cpython.missingInput : "cpython unavailable"],
      )
    }

    const montyPath = yield* nixOutPath(MONTY_PACKAGE)
    const typingPath = yield* nixOutPath(TYPING_EXTENSIONS)
    if (montyPath === undefined || typingPath === undefined) {
      return unavailable(
        "monty",
        "pydantic-monty is not installed and could not be built from nixpkgs",
        `install pydantic-monty into the host Python or provide ${MONTY_PACKAGE}`,
        [
          montyPath === undefined ? `missing ${MONTY_PACKAGE}` : "",
          typingPath === undefined ? `missing ${TYPING_EXTENSIONS}` : "",
        ].filter((line) => line.length > 0),
      )
    }

    const montySite = yield* sitePackagesOf(montyPath)
    const typingSite = yield* sitePackagesOf(typingPath)
    if (montySite === undefined || typingSite === undefined) {
      return unavailable(
        "monty",
        "pydantic-monty site-packages layout was not found",
        `provide ${MONTY_PACKAGE} with a python site-packages directory`,
        [`monty site: ${montySite ?? "missing"}`, `typing site: ${typingSite ?? "missing"}`],
      )
    }
    const env = { PYTHONPATH: `${montySite}:${typingSite}` }
    const probe = yield* runner
      .run({
        argv: [cpython.binaryPath, "-c", "import pydantic_monty as m; print(m.__version__)"],
        env,
        timeoutMs: 60_000,
      })
      .pipe(Effect.catch(() => Effect.succeed(undefined)))

    if (probe === undefined || probe.exitCode !== 0) {
      return unavailable(
        "monty",
        "the pydantic-monty package could not be imported by CPython",
        `provide a pydantic-monty install importable by ${cpython.binaryPath}`,
        [`import check failed: ${probe === undefined ? "command error" : probe.stderr.trim()}`],
      )
    }

    const version = firstLine(probe.stdout)
    return available("monty", cpython.binaryPath, `pydantic-monty ${version}`, cpython.source, env)
  })
