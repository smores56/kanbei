import { parentPort } from "node:worker_threads"

/**
 * Worker entry for the isolation probe. It crashes exactly once on purpose:
 * the parent must observe the failure and keep running.
 */
const port = parentPort

if (port !== null) {
  port.on("message", (message: unknown) => {
    const kind = typeof message === "object" && message !== null
      ? Reflect.get(message, "kind")
      : undefined
    if (kind === "ping") {
      port.postMessage({ kind: "pong" })
      return
    }
    if (kind === "crash") {
      throw new Error("kanbei/spike: intentional worker crash")
    }
  })
  port.postMessage({ kind: "ready" })
}
