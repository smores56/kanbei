import type { ObservedPosture, SandboxManifest } from "../../src/kernel/manifest.ts"

export const manifest: SandboxManifest = {
  sandboxId: "sbx-1",
  agentId: "agent-1",
  generation: "gen-1",
  cgroupPath: "/kanbei/agents/sbx-1",
  enforcementRequired: "monty_wasm",
  mounts: [
    { path: "/inbox", access: "ro", class: "inbox" },
    { path: "/vault", access: "ro", class: "vault" },
    { path: "/todo", access: "rw", class: "todo" },
    { path: "/scratch", access: "rw", class: "scratch" },
  ],
  channels: ["inbox", "vault", "todo"],
  limits: {
    wallMs: 60_000,
    memoryBytes: 10_000_000,
    pidsMax: 32,
    cpuQuota: 1,
    openFiles: 128,
    todoBytes: 65_536,
  },
}

export const posture = (overrides: Partial<ObservedPosture> = {}): ObservedPosture => ({
  enforcement: "monty_wasm",
  generation: "gen-1",
  addresses: ["lo"],
  channelSockets: ["inbox", "vault", "todo"],
  mounts: manifest.mounts,
  ...overrides,
})
