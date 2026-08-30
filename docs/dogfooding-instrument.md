# Pre-registered Dogfooding Instrument (ratified at the cognition review, before M3)

Status: **ratified 2026-08-30** — thresholds are fixed at this revision and are not
retroactively tunable before or during M7 evaluation. Any threshold change requires a
constitution-level review, not an evaluation-time adjustment.

Purpose: the M7 dogfooding gate evaluates kanbei with this instrument: unattended outcome
rates, interrupted-recovery success, cost ceiling, and an expert-task battery. The
instrument exists before M3 begins so that M3–M6 design decisions cannot silently move the
goalposts. All metrics are computed from canonical records only (terminal outcome events,
intent/outcome events, breaker events, provider egress entries); nothing depends on
subjective self-assessment or SQLite-derived numbers.

## 1. Unattended outcome rates

Setup: unattended runs are sessions with no interactive input while perpetual cognition is
active, running the battery tasks (section 4) on the dogfooding box. A run = one RunId
with kind `CognitionStep` (R-09/E-10).

Metrics (from terminal-outcome events; vocabulary per R-18):

- M1.1 outcome distribution: share of runs ending `Progress | NoProgress | Waiting |
  Blocked | Failed | CompletedGoal`;
- M1.2 breaker trips per 1000 wakes (breaker events; trip pauses cognition until explicit
  user resume — R-17/E-02);
- M1.3 stall rate: share of runs ending `Waiting` without any new causal event preceding
  the terminal outcome;
- M1.4 progress rate: share of wakes whose run outcome is `Progress` or `CompletedGoal`.

Thresholds:

- T1.1 ≥ 80% of battery task runs end `CompletedGoal`;
- T1.2 `Failed` (all reasons, including `Deadline`) ≤ 5% of runs;
- T1.3 breaker trips ≤ 1 per 1000 wakes; every trip is a canonical inspectable fact
  (R-17/H-05);
- T1.4 stall rate ≤ 2% (no silent stuck runs);
- T1.5 progress rate ≥ 90% of wakes (the other 10% may legitimately be `Waiting`).

## 2. Interrupted-recovery success

Setup: (a) kanbei-testkit crash injection at every M3 fault point (intent commit, effect
dispatch, outcome commit, wake acceptance) and the M1/M2 fault points still in the M3
code paths; (b) a SIGKILL injected at a random event during an unattended battery run,
then reopen.

Metrics (from recovery + classification records):

- M2.1 recovery validity: share of crash-injected recoveries that reopen to explicit valid
  state (log verify, snapshot closure, no dangling references);
- M2.2 classification honesty: among events that are committed-intent-without-outcome
  (B-05), share classified explicitly `interrupted | ambiguous` — never silently dropped,
  never auto-retried without user/root decision, never double-dispatched;
- M2.3 resume success (battery task 6): share of resumed runs that reach `CompletedGoal`
  without re-executing the pre-crash effects (dedup by intent idempotency/outcome
  records).

Thresholds:

- T2.1 recovery validity = 100% on every crash-injected point (invariant, like M1/M2
  crash matrices);
- T2.2 classification honesty = 100% of committed-intent-without-outcome events carry an
  explicit `interrupted | ambiguous` classification (R-02/C-03);
- T2.3 resume success ≥ 90% in battery task 6.

## 3. Cost ceiling

Setup: provider egress entries are canonical (provider identity, token counts in/out,
sensitivity classes egressed — R-15). Cost is computed at the reference rate table below,
fixed at evaluation time, chosen at ratification and recorded here:

| Token stream | Reference rate |
|---|---|
| input | $5.00 / 1M tokens |
| output | $15.00 / 1M tokens |

Metrics (from egress entries + terminal outcomes):

- M3.1 tokens in/out per battery task;
- M3.2 USD per battery task at reference rates;
- M3.3 USD per unattended hour (battery run wall clock);
- M3.4 spend-breaker floor adherence: the kernel spend breaker (R-17/E-02) trips at the
  configured window budget and the trip is canonical.

Thresholds:

- T3.1 per battery task ≤ 250k input + 25k output tokens (≈ $1.63 at reference rates);
- T3.2 battery total ≤ $6.00 at reference rates (6 tasks × $1.00);
- T3.3 unattended hour ≤ $2.00 at reference rates;
- T3.4 every spend-breaker trip is canonical and correct (trips only when the window
  budget is exceeded, within the kernel floor).

## 4. Expert-task battery

Fixed 6-task battery, run on the kanbei repository itself (dogfooding on real development
work). Each task: setup, task statement, success criteria, budget. A task passes iff all
its success criteria hold AND its cost is within T3.1. Evaluation is the first
`CompletedGoal` or the budget expiry, whichever comes first. No task may be tuned by
trying it before evaluation.

1. **Bug fix from failing test.** Setup: one deliberately broken commit on a scratch
   branch of a fixture repo (kanbei testkit fixture). Task: make the failing test pass
   without weakening the test. Success: test suite green on the fix commit; the diff is a
   minimal, behavior-preserving fix; outcome `CompletedGoal`.
2. **Feature with tests.** Setup: a small, precisely specified feature (one new typed
   event + FSM transition + recovery coverage in kanbei-core style). Task: implement with
   tests. Success: new tests + existing suite green; feature matches the spec.
3. **Behavior-preserving refactor.** Setup: one function with a full test suite. Task:
   refactor per a stated goal (e.g., split, rename, extract) without changing behavior.
   Success: suite green, diff matches the stated goal, no test edits except mechanical
   renames.
4. **Investigation report.** Setup: a failing integration test with an obfuscated root
   cause (e.g., a torn-tail recovery edge). Task: trace and produce a written analysis
   (root cause, evidence path, fix proposal) in a committed markdown file. Success: root
   cause is correct and evidence path cites canonical facts; no code change required.
5. **Cross-session continuity.** Setup: task 5 is split into two parts; part A runs in one
   session and ends (normal close), part B resumes in a new session with `continue_from`
   (M6). Task: finish the combined task. Success: part B completes without re-doing
   part A's effects and cites part A's memory; combined criteria of the original task.
6. **Interrupted task.** Setup: task 6 is an effectful multi-step task (git commits +
   file edits); the harness SIGKILLs the process at a random event mid-task. Task: resume
   after reopen. Success: resumes to `CompletedGoal`, no duplicated effects (no duplicate
   commits, no clobbered edits), pre-crash effects intact and correctly recorded.

Pass criteria: the battery passes iff ≥ 5/6 tasks pass and every threshold in sections
1–3 holds. M7 additionally reports the qualitative axes (coherence, memory usefulness,
extension ergonomics) as review input, but the gate decision rests on the thresholds
above.

## 5. Measurement provenance

- All metrics derive from canonical session events and provider egress entries; the
  evaluation script is part of kanbei-testkit (an M6/M7 deliverable) and is itself
  reviewed before the M7 gate.
- Task 5 and task 6 depend on M6 (`continue_from`, interrupted recovery) and are marked
  `deferred-to-M6`; tasks 1–4 are M3-evaluable once the provider gateway and tools exist,
  but the instrument is not exercised before M7 to avoid tuning.
- Hardware caveat: absolute latency numbers are not part of this instrument; the M7
  evaluation runs on the NVMe dogfooding box (M1 report, hardware caveat).
