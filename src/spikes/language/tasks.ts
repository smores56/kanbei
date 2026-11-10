import { Schema } from "effect"
import taskFixtureJson from "./task-fixture.json" with { type: "json" }
import { TaskSpec } from "./types.ts"

const TaskFixture = Schema.Struct({
  schemaVersion: Schema.Literal(1),
  suiteId: Schema.String,
  suiteVersion: Schema.String,
  tasks: Schema.Array(TaskSpec),
})

export type TaskSuite = typeof TaskFixture.Type

/** The sealed ten-task workload fixture. Imported as data so every arm receives the same prompts. */
export const TASK_SUITE: TaskSuite = Schema.decodeUnknownSync(TaskFixture)(taskFixtureJson)

export const TASKS: ReadonlyArray<TaskSpec> = TASK_SUITE.tasks

export const taskById = (id: string): TaskSpec | undefined => TASKS.find((task) => task.id === id)

export const adversarialTasks: ReadonlyArray<TaskSpec> = TASKS.filter((task) => task.adversarial)
