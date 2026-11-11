import { assertEquals } from "@std/assert"
import {
  forkBombHolds,
  orphanContainmentHolds,
} from "../../../src/spikes/isolation/arms/containment.ts"
import type {
  ForkBombEvidence,
  OrphanEvidence,
} from "../../../src/spikes/isolation/arms/containment.ts"

const orphanEvidence = (overrides: Partial<OrphanEvidence> = {}): OrphanEvidence => ({
  markerPidsBefore: 1,
  pidsCurrentBefore: 3,
  orphanPidPresent: true,
  orphanAliveAfter: false,
  hostCountBefore: 200,
  hostCountAfter: 200,
  populatedAfter: 0,
  procsAfter: 0,
  markerPidsAfter: 0,
  cgroupRemoved: true,
  ...overrides,
})

Deno.test("orphan containment passes only when every post-kill signal agrees", () => {
  assertEquals(orphanContainmentHolds(orphanEvidence()), true)
})

Deno.test("orphan containment fails when the child was never observed alive", () => {
  assertEquals(orphanContainmentHolds(orphanEvidence({ markerPidsBefore: 0 })), false)
  assertEquals(orphanContainmentHolds(orphanEvidence({ pidsCurrentBefore: 1 })), false)
  assertEquals(orphanContainmentHolds(orphanEvidence({ orphanPidPresent: false })), false)
})

Deno.test("orphan containment fails on any surviving signal", () => {
  assertEquals(orphanContainmentHolds(orphanEvidence({ orphanAliveAfter: true })), false)
  assertEquals(orphanContainmentHolds(orphanEvidence({ populatedAfter: 1 })), false)
  assertEquals(orphanContainmentHolds(orphanEvidence({ procsAfter: 1 })), false)
  assertEquals(orphanContainmentHolds(orphanEvidence({ markerPidsAfter: 1 })), false)
  assertEquals(orphanContainmentHolds(orphanEvidence({ cgroupRemoved: false })), false)
})

const forkBombEvidence = (overrides: Partial<ForkBombEvidence> = {}): ForkBombEvidence => ({
  markerPidsBefore: 5,
  pidsMaxEvents: 12,
  populatedAfter: 0,
  procsAfter: 0,
  markerPidsAfter: 0,
  ...overrides,
})

Deno.test("fork bomb passes on observed refusals and an empty cgroup", () => {
  assertEquals(forkBombHolds(forkBombEvidence()), true)
})

Deno.test("fork bomb fails without a pre-kill marker, refusals, or empty teardown", () => {
  assertEquals(forkBombHolds(forkBombEvidence({ markerPidsBefore: 0 })), false)
  assertEquals(forkBombHolds(forkBombEvidence({ pidsMaxEvents: 0 })), false)
  assertEquals(forkBombHolds(forkBombEvidence({ populatedAfter: 1 })), false)
  assertEquals(forkBombHolds(forkBombEvidence({ procsAfter: 2 })), false)
  assertEquals(forkBombHolds(forkBombEvidence({ markerPidsAfter: 1 })), false)
})
