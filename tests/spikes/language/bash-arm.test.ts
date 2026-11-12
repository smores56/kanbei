import { assertEquals } from "@std/assert"
import { DenoServices } from "@effect/platform-deno"
import { Effect, FileSystem, Path } from "effect"
import { bashArm } from "../../../src/spikes/language/arms/bash.ts"
import { parseProbeOutput } from "../../../src/spikes/language/arms/contract.ts"
import { fanoutGold } from "../../../src/spikes/language/host-calls.ts"
import { liveCommandRunnerLayer } from "../../../src/spikes/language/process.ts"
import { CommandRunner } from "../../../src/spikes/language/process.ts"

const CANARY = "kanbei-local-test-canary"

Deno.test("bash arm runs the probe program and returns gold-correct fan-out output", async () => {
  const enabled = await Effect.runPromise(
    bashArm.resolve().pipe(
      Effect.provide(liveCommandRunnerLayer),
      Effect.provide(DenoServices.layer),
    ),
  )
  if (enabled.status !== "available") {
    assertEquals(enabled.missingInput.length > 0, true)
    return
  }

  const directory = await Deno.makeTempDir({ prefix: "kanbei-bash-arm-" })
  try {
    const canaryFile = `${directory}/canary.txt`
    await Deno.writeTextFile(canaryFile, CANARY)
    const outcome = await Effect.runPromise(
      Effect.gen(function* () {
        const runner = yield* CommandRunner
        const fs = yield* FileSystem.FileSystem
        const path = yield* Path.Path
        const prepared = bashArm.prepare(enabled, {
          latencyMs: 2,
          calls: 2,
          scratchDir: directory,
          canary: CANARY,
          canaryFile,
        })
        yield* fs.makeDirectory(directory, { recursive: true })
        yield* Effect.forEach(
          prepared.files,
          (file) => fs.writeFileString(path.join(directory, file.name), file.text),
        )
        const run = yield* runner.run({
          argv: prepared.argv(directory),
          env: prepared.env(directory),
          timeoutMs: prepared.timeoutMs,
        })
        return { run, prepared }
      }).pipe(
        Effect.provide(liveCommandRunnerLayer),
        Effect.provide(DenoServices.layer),
      ),
    )

    assertEquals(outcome.run.exitCode, 0)
    assertEquals(outcome.prepared.latencySimulation, "external-sleep")
    const probe = await Effect.runPromise(parseProbeOutput("bash", outcome.run.stdout))
    assertEquals(probe.fanout.calls, 2)
    assertEquals(probe.fanout.checksum, fanoutGold(2).checksum)
    assertEquals(probe.fanout.top, fanoutGold(2).top)
    assertEquals(probe.expression.value, 285)
    assertEquals(probe.stream.events, 4)
    assertEquals(probe.retry.attempts, 3)
  } finally {
    await Deno.remove(directory, { recursive: true }).catch(() => undefined)
  }
})
