# A6 — S4/S6 pre-registration

Status: `pre-registered`. No numeric ceiling or quality threshold is set here.

## Scope

A6 defines the measurement protocols consumed by later benchmark tasks. It does not run the release benchmarks and does not close `DG-6` or `DG-7`.

## S4 — cost, latency, freshness, and ceilings

Dataset: `cost-v1@1.0.0` (`fixtures/cost-v1/workload.json`).

Pre-registration: `src/spikes/protocols/cost-v1.ts`.

- Paired, interleaved trials.
- 3 warmup runs, 30 measured runs, 5 cold-start runs per scenario.
- Percentile bootstrap with 10,000 resamples at 95% confidence.
- Cold means after process exit; warm means after warmup.
- Scenarios: full journey, idle, fan-out burst, reconciliation backlog, external conflict, backup/restore.
- Every enforced ceiling must deny before dispatch and be observable after the decision.
- Metrics include p50/p95 latency, CPU, peak RSS, disk/CAS growth, tokens, spend, provider rate, and coverage lag.
- Quality metrics include task success, queue age, decision-to-effect, reconciliation catch-up, stale-retracted reads, and UI comprehension.
- Numeric thresholds: deferred to `DG-6`.

## S6 — UI comprehension and interrupt grouping

Dataset: `ui-v1@1.0.0` (`fixtures/ui-v1/questions.json`).

Pre-registration: `src/spikes/protocols/ui-v1.ts`.

- Ten scripted questions covering contested, retracted, suspect, stale, current, and approval states.
- Baseline: raw-ledger transcript.
- Structured view and raw baseline are scored separately; no collapsed single score.
- Presentation replay preserves selection and gesture state and performs no live model call.
- One interrupt group per run; individual canonical rulings; resume must complete its group.
- Accessibility tree and text fallback are required.
- Numeric thresholds: deferred to `DG-6` and `DG-7`.

## Validation

`tests/protocols/preregistration.test.ts` validates both pre-registration documents against their Effect schemas and asserts that thresholds remain deferred.

## Gate status

- `DG-6`: open. `H4` produces the measured baseline; the human sets the values.
- `DG-7`: open. `H2` produces the measured holdout baseline; the human sets the values.

## Residual risks

- The benchmark depends on the final runtime, broker, and execution-language selections from `DG-1`–`DG-4`.
- UI comprehension still needs a scorer protocol and inter-rater agreement procedure before `H6`.
