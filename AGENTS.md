# Kanbei engineering rules

## Source of truth

- Requirements: `docs/design/00-requirements.md`
- Technical specification: `docs/design/07-technical-specification.md`
- Implementation plan and task graph: `docs/design/08-implementation-plan.md`
- If this file conflicts with the specification, the specification wins.
- Every behavior change starts with a failing test. Every task closes with evidence and independent review.

## Toolchain

- Runtime: Deno 2.9.2, TypeScript 6.
- Program model: Effect 4.1.2 from `effect.website/docs/v4`.
- Formatting: `deno fmt`.
- Linting: `deno lint` plus `oxlint` with warnings denied.
- Types: `deno check` with strict compiler options in `deno.json`.
- Tests: `deno test`; tests use the Effect runtime and virtual services.
- Pre-commit runs `deno task verify`. Install once with `deno task hooks:install`.
- Never bypass the hook with `--no-verify`. A broken gate is a defect.

## Effect is the program model

Every domain operation returns an `Effect`. Raw promises, callbacks, and `async` functions are allowed only in adapters and process entrypoints, immediately wrapped into Effect.

```ts
import { Effect } from "effect"

export const answer = Effect.gen(function* () {
  const value = yield* loadValue
  return value + 1
})
```

- Use `Effect.gen` for sequential composition; use `Effect.pipe` for transformations.
- Use `Effect.forEach` / `Effect.all` for concurrency. Never use `Promise.all` in domain code.
- Use `Effect.acquireRelease` or `Effect.scoped` for resources. Never leak handles or fibers.
- Use `Effect.timeout`, `Effect.retry`, and schedules instead of hand-written retry loops.
- Use `Clock`, `Random`, and `Config` services. Never call `Date.now()` or `Math.random()` in domain code.
- Use `Effect.log`, `Effect.annotateLogs`, and `Effect.withSpan`. Never use `console` outside a process entrypoint.
- Run effects only at the edge: `main.ts`, a script, a worker bootstrap, or a test.

## Services, layers, and composition

Dependencies are services, not module-level singletons.

```ts
import { Context, Layer } from "effect"

export class Ledger extends Context.Service<Ledger, {
  readonly append: (record: Record) => Effect.Effect<RecordId, AppendError>
}>()("kanbei/Ledger") {}
```

- Use `Context.Service<Self, Shape>()("namespace/Name")` for class-style services.
- Use `Layer.effect`, `Layer.succeed`, `Layer.scoped`, and `Layer.mergeAll`.
- Define one application layer per entrypoint. Tests provide test layers.
- Do not use `Effect.runPromise` inside services.
- Keep interfaces narrow and capability-oriented. Do not pass a whole runtime context through domain functions.
- No global mutable state. State is a service, a `Ref`, or canonical data.

## Data, schemas, and immutability

- All boundary data is parsed with `Schema`; never cast or trust `unknown`.
- Use branded IDs and schema classes for domain values.
- Use `Readonly`, `readonly` fields, and immutable updates.
- Use `Option` for meaningful absence and `Either` for data-level branching. `null` and `undefined` are reserved for encoded wire formats.
- Use `Data.TaggedClass` for values and `Data.TaggedError` for expected errors.
- Unknown component payloads must round-trip byte-identically; never drop fields you do not understand.
- Persisted formats are versioned. A schema change requires a migration or a compatibility adapter.

## Errors

- Expected failures belong in the Effect error channel with a narrow tagged error type.
- Programmer errors are defects: use `Effect.die` only when continuing is impossible.
- Never throw from domain code. Never use `catch (e: any)`.
- Add context with `Effect.mapError` / `Effect.catchTag`; do not stringify errors into messages.
- Zero-tolerance failures never retry. A flaky integrity check is a defect.

## Purity and style

- Prefer small pure functions and composition over inheritance.
- One reason to change per module. One exported behavior per file where practical.
- Avoid boolean parameters; use named variants or tagged unions.
- Avoid `let`, loops, and mutation. Use `Effect.forEach`, `Array` combinators, or recursion.
- No `any`, non-null assertions, enums, namespaces, or TypeScript parameter properties.
- No type assertions except `as const`; parse instead.
- Imports are explicit named imports from public module entrypoints. No barrel files.
- Comments explain why, never what. Public kernel contracts need concise doc comments.

## Capability boundaries

- Domain and kernel code cannot import `node:*`, `Deno.*`, or HTTP clients directly.
- Filesystem, process, network, model, and secret access go through capability services and adapters.
- The canonical append path, sandbox supervisor, broker, global ceilings, receipt verification, activation supervisor, audit sink, and consent surface are frozen trust-root code.
- Agents and plugins never receive a canonical SQLite handle or trust-root write capability.
- Secrets never enter agent env, argv, logs, model context, CAS, or canonical records.
- On denied or ambiguous external effects, fail closed and record the outcome. Never blind-retry an `outcome_unknown` non-idempotent effect.

## Tests and evidence

- Tests live under `tests/` mirroring `src/`.
- Unit tests cover pure functions and schemas.
- Property tests cover canonicalization, hash chains, and lifecycle transitions.
- Conformance tests bind to the validation IDs in `07 §7` and emit an `EvidenceEnvelope`.
- Use virtual time (`TestClock`) and synthetic transport. Deterministic tests never call a live model.
- Every bug fix includes a regression test that failed before the fix.
- Tests must be deterministic. Flaky tests are defects, not retries.
- Test names state the behavior and the expected outcome.

## Git and delivery

- Conventional Commits: `type(scope): description`.
- Scopes follow the module or phase: `evidence`, `ledger`, `cas`, `broker`, `ui`.
- Commits are small and reviewable. Do not mix formatting churn with behavior.
- Push immediately after committing.
- Feature work stays in its worktree. Merge to `main` only after review.
- Never commit generated state, databases, secrets, or local fixture output.

## Definition of done

A task is done only when all of these are true:

1. The task-specific validation in `07 §7` passes with an `EvidenceEnvelope`.
2. `deno task verify` passes from a clean checkout.
3. An independent reviewer checks scope, evidence, and invariants.
4. Findings are fixed and affected checks are rerun.
5. No silent default was introduced for a decision gate.
