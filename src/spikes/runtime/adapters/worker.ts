import { Worker } from "node:worker_threads"

export interface WorkerProbeResult {
  readonly workerStartupMs: number
  readonly roundtrip: ReadonlyArray<number>
  readonly crashObserved: boolean
  readonly parentSurvivedCrash: boolean
}

/**
 * Worker/process isolation probe.
 *
 * Boundary adapter: `node:worker_threads` is implemented by Node natively and
 * by Deno's Node compatibility layer, so the same worker entry runs on both
 * arms. The probe measures startup, ping round-trip, and whether an uncaught
 * worker exception is contained.
 */
export const runWorkerProbe = (
  workerPath: string,
  runs: number,
  now: () => number,
): Promise<WorkerProbeResult> =>
  new Promise((resolve) => {
    const startedAt = now()
    const roundtrip: number[] = []
    const state = { readyAt: 0, sentAt: 0, settled: false, heartbeats: 0 }
    const worker = new Worker(workerPath)
    const heartbeat = setInterval(() => {
      state.heartbeats += 1
    }, 5)

    const finish = (crashObserved: boolean): void => {
      if (state.settled) return
      state.settled = true
      clearInterval(heartbeat)
      resolve({
        workerStartupMs: state.readyAt === 0 ? 0 : Number((state.readyAt - startedAt).toFixed(3)),
        roundtrip: [...roundtrip],
        crashObserved,
        parentSurvivedCrash: crashObserved && state.heartbeats > 0,
      })
      worker.terminate().catch(() => undefined)
    }

    const ping = (): void => {
      state.sentAt = now()
      // node:worker_threads postMessage takes an optional transfer list, not a target origin.
      // oxlint-disable-next-line unicorn/require-post-message-target-origin
      worker.postMessage({ kind: "ping" })
    }

    worker.on("message", (message: unknown) => {
      const kind = typeof message === "object" && message !== null
        ? Reflect.get(message, "kind")
        : undefined
      if (kind === "ready") {
        state.readyAt = now()
        ping()
        return
      }
      if (kind === "pong") {
        roundtrip.push(Number((now() - state.sentAt).toFixed(3)))
        if (roundtrip.length < runs) {
          ping()
        } else {
          // oxlint-disable-next-line unicorn/require-post-message-target-origin
          worker.postMessage({ kind: "crash" })
        }
      }
    })
    worker.on("error", () => {
      finish(true)
    })
    worker.on("exit", () => {
      finish(true)
    })
  })
