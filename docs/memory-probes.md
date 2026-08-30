# Memory usefulness probes (post-M4)

Pre-registered probe plan for the M4 memory substrate (R-11/R-12: claims,
edges, root-manifest transitions, salience projection, memory tools). The
M4 gate proves the substrate is crash-safe, deterministic, and canonical;
these probes measure whether it is *useful* — the dogfooding instrument
(see docs/dogfooding-instrument.md) applied to memory. Thresholds are
tuned before M7; nothing here is a pass/fail gate at M4.

## Dimensions and probes

1. **Writing fidelity** — does the kernel capture what matters?
   - Probe W1: run 10 scripted fix sessions; after each, propose 2-3
     claims covering the root cause + the accepted fix. Count claims the
     human reviewer would also have written (precision of the proposal
     flow).
   - Probe W2: measure the fraction of `memory.propose` calls that reach
     `approved` vs `proposed`/`deferred` — a high deferred rate flags CAS
     contention or approval friction, not memory quality.

2. **Evidence retrieval** — does `memory.query` return the right claims?
   - Probe R1: from a seeded fold of 50 claims (mix of kinds, 3
     contradictions, 2 supersessions), run 20 scripted queries; measure
     recall@5 against a hand-labeled relevance set.
   - Probe R2: one-hop expansion behavior — queries whose best match is a
     superseded claim must still surface the survivor with the
     `supersedes: true` annotation (M-04 validity filter before fusion).

3. **Temporal/stale-fact handling** — supersession must work in practice.
   - Probe T1: across sessions, intentionally re-propose a claim that
     contradicts an older one, with `supersedes`; verify the older claim
     disappears from later queries and the annotation text is preserved.
   - Probe T2: measure the age distribution of claims returned — a
     healthy fold returns recent claims unless older ones are pinned by
     recency decay (RECENCY_WINDOW) or graph centrality.

4. **Answer support / abstention** — does memory change answers?
   - Probe A1: for 10 recurring questions, compare answer quality with
     memory on vs off (same prompts, fresh vs seeded sessions); record
     whether the seeded fold changed the answer at all.
   - Probe A2: count `cache_outcome` distribution — `Hit` means the
     stable prefix actually reused; a high `Invalidated` rate flags
     churn in the memory fragments.

5. **Downstream coding utility** — memory must help the coding loop.
   - Probe C1: in a multi-turn fix task, count repeated tool calls that a
     memory claim would have short-circuited (e.g. re-discovering a file
     layout fact already claimed).
   - Probe C2: child runs — measure how often a child's `memory.query`
     returns useful project claims despite the attenuated scope
     (project-only), and whether `ChildDone` wakes trigger productive
     follow-ups.

6. **Latency / cost** — projection must stay cheap.
   - Probe L1: wall-clock of `project_context` on a 64-event ring +
     50-claim fold, with and without the salience projector; record p50
     and p95.
   - Probe L2: tokens per model call attributable to memory fragments
     (mem.lifetime + mem.project + ev.memory) vs the projection budget
     (8192 total / 4096 volatile).

7. **Graph growth** — the fold must stay tractable.
   - Probe G1: after 50 sessions over one project, record claims, edges,
     retracted counts, and the salience reachability denominator; flag
     folds where retracted claims outgrow active ones 2:1 (supersession
     without consolidation).
   - Probe G2: index build + reconcile time at 200/500/1000 claims;
     thresholds inform the compaction design (R-18/E-06).

## Method

- Probes run against the testkit harness with `ScriptedProvider`-style
  drivers and the fake engine; no live provider needed.
- Each probe logs its raw numbers to stdout; the gate records them in
  the milestone report. No thresholds are enforced until M7 tuning.
