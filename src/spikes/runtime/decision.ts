import type {
  ArmResult,
  Decision,
  HardFail,
  Measurement,
  PreRegistration,
  RuntimeName,
} from "./schema.ts"

/**
 * Pre-registered decision rule, applied mechanically.
 *
 * Pure and total: the same arm results and plan always produce the same
 * decision, so the gate record is reproducible from `result.json`.
 */

export interface MetricSpec {
  readonly id: string
  readonly label: string
  readonly unit: string
  readonly direction: "lower" | "higher"
  readonly extract: (arm: ArmResult) => number
}

export const METRICS: ReadonlyArray<MetricSpec> = [
  {
    id: "coldReady",
    label: "cold start to ready",
    unit: "ms",
    direction: "lower",
    extract: (arm) => arm.boot.coldReady.p50,
  },
  {
    id: "coldStart",
    label: "cold start to exit",
    unit: "ms",
    direction: "lower",
    extract: (arm) => arm.boot.coldStart.p50,
  },
  {
    id: "warmStart",
    label: "warm start (in-process bring-up)",
    unit: "ms",
    direction: "lower",
    extract: (arm) => arm.boot.warmStart.p50,
  },
  {
    id: "rssAfterBoot",
    label: "RSS after boot",
    unit: "MiB",
    direction: "lower",
    extract: (arm) => arm.memory.rssAfterBoot.p50,
  },
  {
    id: "peakRss",
    label: "peak RSS",
    unit: "MiB",
    direction: "lower",
    extract: (arm) => arm.memory.peakRssBytes / 1_048_576,
  },
  {
    id: "eventLoopP95",
    label: "event-loop lag p95 under load",
    unit: "ms",
    direction: "lower",
    extract: (arm) => arm.eventLoop.p95,
  },
  {
    id: "appendP95",
    label: "durable append p95",
    unit: "ms",
    direction: "lower",
    extract: (arm) => arm.ledger.append.p95,
  },
  {
    id: "readP95",
    label: "point read p95",
    unit: "ms",
    direction: "lower",
    extract: (arm) => arm.ledger.read.p95,
  },
  {
    id: "appendThroughput",
    label: "durable append throughput",
    unit: "ops/s",
    direction: "higher",
    extract: (arm) => arm.ledger.appendsPerSecond,
  },
  {
    id: "workerStartup",
    label: "worker startup",
    unit: "ms",
    direction: "lower",
    extract: (arm) => arm.isolation.workerStartupMs,
  },
  {
    id: "workerRoundtripP95",
    label: "worker round-trip p95",
    unit: "ms",
    direction: "lower",
    extract: (arm) => arm.isolation.roundtrip.p95,
  },
  {
    id: "shutdown",
    label: "SIGTERM shutdown",
    unit: "ms",
    direction: "lower",
    extract: (arm) => arm.signals.shutdownMs,
  },
  {
    id: "runtimeBinary",
    label: "runtime binary size",
    unit: "MiB",
    direction: "lower",
    extract: (arm) => arm.packaging.runtimeBinaryBytes / 1_048_576,
  },
]

const round = (value: number, digits = 3): number => {
  const factor = 10 ** digits
  return Math.round(value * factor) / factor
}

const ratioOf = (node: number, deno: number): number => {
  if (deno === 0) return node === 0 ? 1 : Number.POSITIVE_INFINITY
  return round(node / deno, 4)
}

const winnerOf = (spec: MetricSpec, node: number, deno: number): Measurement["winner"] => {
  if (Math.abs(node - deno) < 1e-9) return "tie"
  if (spec.direction === "lower") return node < deno ? "node" : "deno"
  return node > deno ? "node" : "deno"
}

export const measure = (
  arms: Readonly<Record<RuntimeName, ArmResult>>,
): ReadonlyArray<Measurement> =>
  METRICS.map((spec) => {
    const node = round(spec.extract(arms.node))
    const deno = round(spec.extract(arms.deno))
    return {
      id: spec.id,
      label: spec.label,
      unit: spec.unit,
      direction: spec.direction,
      node,
      deno,
      ratio: ratioOf(node, deno),
      winner: winnerOf(spec, node, deno),
    }
  })

export const compositeScore = (
  arms: Readonly<Record<RuntimeName, ArmResult>>,
  plan: PreRegistration,
): { readonly node: number; readonly deno: number; readonly margin: number } => {
  const cost = (runtime: RuntimeName): number =>
    plan.compositeWeights.reduce((total, weight) => {
      const spec = METRICS.find((candidate) => candidate.id === weight.id)
      if (spec === undefined) return total
      const own = spec.extract(arms[runtime])
      const opponent = spec.extract(arms[runtime === "node" ? "deno" : "node"])
      const best = spec.direction === "lower" ? Math.min(own, opponent) : Math.max(own, opponent)
      const relative = best === 0 ? 1 : own / best
      return total + weight.weight * relative
    }, 0)

  const node = round(cost("node"), 4)
  const deno = round(cost("deno"), 4)
  const lower = Math.min(node, deno)
  const margin = lower === 0 ? 0 : round(Math.abs(node - deno) / lower, 4)
  return { node, deno, margin }
}

const failedCheck = (arm: ArmResult, id: string): boolean =>
  arm.correctness.some((check) => check.id === id && !check.pass)

export const hardFailsFor = (arm: ArmResult, plan: PreRegistration): ReadonlyArray<HardFail> => {
  const fails: Array<HardFail> = []
  const add = (id: string, detail: string): void => {
    fails.push({ id, runtime: arm.runtime, detail })
  }

  const integrityFailure = failedCheck(arm, "integrity-after-load")
  if (integrityFailure) add("HF-INTEGRITY", "integrity_check did not return ok after the load")

  const dataLossFailure = failedCheck(arm, "appends-preserved")
  if (dataLossFailure) {
    add("HF-DATA-LOSS", `issued=${arm.ledger.appends} stored<${arm.ledger.appends}`)
  }

  const otherChecks = arm.correctness.filter(
    (check) =>
      !check.pass && check.id !== "integrity-after-load" && check.id !== "appends-preserved",
  )
  if (otherChecks.length > 0) {
    add(
      "HF-CORRECTNESS",
      otherChecks.map((check) => `${check.id}: ${check.detail}`).join("; "),
    )
  }

  const writer = arm.singleWriter
  if (writer.concurrentWriterCommitted || !writer.failClosed || !writer.recoveryOk) {
    add(
      "HF-SINGLE-WRITER",
      `concurrentCommit=${writer.concurrentWriterCommitted} failClosed=${writer.failClosed} recovery=${writer.recoveryOk}`,
    )
  }

  if (arm.eventLoop.p95 > plan.thresholds.eventLoopP95Ms) {
    add(
      "HF-TAIL-LATENCY",
      `event-loop p95=${arm.eventLoop.p95}ms > ${plan.thresholds.eventLoopP95Ms}ms`,
    )
  }

  if (arm.ledger.append.p95 > plan.thresholds.appendP95Ms) {
    add(
      "HF-APPEND-TAIL",
      `append p95=${arm.ledger.append.p95}ms > ${plan.thresholds.appendP95Ms}ms`,
    )
  }

  if (!arm.isolation.crashObserved || !arm.isolation.parentSurvivedCrash) {
    add(
      "HF-ISOLATION",
      `crashObserved=${arm.isolation.crashObserved} parentSurvived=${arm.isolation.parentSurvivedCrash}`,
    )
  }

  if (
    !arm.signals.handlerInvoked ||
    arm.signals.shutdownMs > plan.thresholds.shutdownMs ||
    arm.signals.exitCode !== 0
  ) {
    add(
      "HF-SIGNAL",
      `handlerInvoked=${arm.signals.handlerInvoked} shutdown=${arm.signals.shutdownMs}ms exit=${arm.signals.exitCode}`,
    )
  }

  const expected = plan.permissionExpectations[arm.runtime]
  const failOpen = arm.permissions.filter(
    (probe) => expected.includes(probe.id) && !probe.denied,
  )
  if (failOpen.length > 0) {
    add(
      "HF-PERMISSION",
      `fail-open: ${failOpen.map((probe) => `${probe.id}(${probe.code})`).join(",")}`,
    )
  }

  return fails
}

const percent = (value: number): string => `${Math.round(value * 1000) / 10}%`

const reasonsAgainst = (
  loser: RuntimeName,
  measurements: ReadonlyArray<Measurement>,
): ReadonlyArray<string> =>
  measurements
    .filter((measurement) => measurement.winner !== loser && measurement.winner !== "tie")
    .map((measurement) => {
      const own = loser === "node" ? measurement.node : measurement.deno
      const other = loser === "node" ? measurement.deno : measurement.node
      const delta = other === 0 ? 0 : Math.abs(own - other) / other
      return `${measurement.label}: ${own}${measurement.unit} versus ${other}${measurement.unit} (${
        percent(delta)
      } worse)`
    })

export interface DecisionOutcome {
  readonly measurements: ReadonlyArray<Measurement>
  readonly hardFails: ReadonlyArray<HardFail>
  readonly composite: { readonly node: number; readonly deno: number; readonly margin: number }
  readonly decision: Decision
  readonly rejectedAlternative: {
    readonly value: RuntimeName | "none"
    readonly reasons: ReadonlyArray<string>
  }
}

export const decide = (
  arms: Readonly<Record<RuntimeName, ArmResult>>,
  plan: PreRegistration,
  blockedDecisions: ReadonlyArray<string> = [],
): DecisionOutcome => {
  const measurements = measure(arms)
  const composite = compositeScore(arms, plan)
  const hardFails = [...hardFailsFor(arms.node, plan), ...hardFailsFor(arms.deno, plan)]
  const disqualified = new Set(hardFails.map((fail) => fail.runtime))

  const decideWith = (
    value: Decision["value"],
    rule: string,
    rationale: string[],
  ): DecisionOutcome => ({
    measurements,
    hardFails,
    composite,
    decision: { value, rule, rationale },
    rejectedAlternative: value === "inconclusive"
      ? { value: "none", reasons: ["no arm was selected, so no alternative was rejected"] }
      : {
        value: value === "node" ? "deno" : "node",
        reasons: reasonsAgainst(value === "node" ? "deno" : "node", measurements),
      },
  })

  if (blockedDecisions.length > 0) {
    return decideWith("inconclusive", "blocked measurement", [
      ...blockedDecisions,
      "the pre-registration forbids imputing a blocked decision measurement",
    ])
  }

  if (disqualified.size === 2) {
    return decideWith("inconclusive", "hard-fail elimination", [
      "both arms tripped at least one hard fail",
      ...hardFails.map((fail) => `${fail.runtime} ${fail.id}: ${fail.detail}`),
    ])
  }

  if (disqualified.size === 1) {
    const survivor: RuntimeName = disqualified.has("node") ? "deno" : "node"
    const loser: RuntimeName = survivor === "node" ? "deno" : "node"
    const details = hardFails
      .filter((fail) => fail.runtime === loser)
      .map((fail) => `${fail.id}: ${fail.detail}`)
    return decideWith(survivor, "hard-fail elimination", [
      `${loser} tripped ${details.length} hard fail(s) and is disqualified`,
      ...details,
    ])
  }

  const winnerByComposite: RuntimeName = composite.node <= composite.deno ? "node" : "deno"
  if (composite.margin >= plan.decisionMargin) {
    return decideWith(winnerByComposite, "weighted composite", [
      `composite cost node=${composite.node} deno=${composite.deno} (margin ${
        percent(composite.margin)
      } >= ${percent(plan.decisionMargin)})`,
      `weights: ${
        plan.compositeWeights.map((weight) => `${weight.id}=${weight.weight}`).join(", ")
      }`,
    ])
  }

  const rssWinner: RuntimeName = arms.node.memory.peakRssBytes <= arms.deno.memory.peakRssBytes
    ? "node"
    : "deno"
  if (arms.node.memory.peakRssBytes !== arms.deno.memory.peakRssBytes) {
    return decideWith(rssWinner, "tie-breaker: peak RSS", [
      `composite margin ${percent(composite.margin)} < ${percent(plan.decisionMargin)}`,
      `peak RSS node=${arms.node.memory.peakRssBytes} deno=${arms.deno.memory.peakRssBytes}`,
    ])
  }

  const coldWinner: RuntimeName =
    measurements.find((measurement) => measurement.id === "coldReady")?.winner === "deno"
      ? "deno"
      : "node"
  const coldTied =
    measurements.find((measurement) => measurement.id === "coldReady")?.winner === "tie"
  if (!coldTied) {
    return decideWith(coldWinner, "tie-breaker: cold start", [
      `composite margin ${percent(composite.margin)} < ${percent(plan.decisionMargin)}`,
      "peak RSS tied; cold start separates the arms",
    ])
  }

  return decideWith("inconclusive", "no separation", [
    `composite margin ${percent(composite.margin)} < ${percent(plan.decisionMargin)}`,
    "peak RSS and cold start do not separate the arms",
  ])
}
