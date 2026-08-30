//! Bounded scheduler (M3 agent spine): the run FSM (R-09/E-10 Wake = Run),
//! canonical wake-acceptance/denial records (R-09/E-09), kernel-owned
//! circuit breakers (R-17/E-02), per-run budgets, and the cognition-step
//! seam (R-18/E-01). The session actor commits the canonical records; this
//! crate owns the decision logic.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use kanbei_core::digest::Digest;
use serde::{Deserialize, Serialize};
use thiserror::Error;

// ---------- run identity ----------

pub type RunId = kanbei_core::id::Id128;

/// Kind discriminator on every RunId (R-09/E-10).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RunKind {
    CognitionStep,
    ResponderTurn,
    Child,
}

// ---------- triggers and wakes ----------

/// Typed trigger provenance of a wake. Raw triggers are policy-private; the
/// canonical records carry only the typed kind plus coalesced trigger
/// digests (R-09/E-09).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TriggerKind {
    UserMessage,
    Timer,
    NewCausalEvent,
    ManualResume,
    ChildDone,
}

/// One observed trigger (ephemeral — never canonical).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Trigger {
    pub kind: TriggerKind,
    /// Digest of the trigger's canonical referent (e.g. the committing event
    /// envelope digest); `None` for purely external triggers.
    pub referent: Option<Digest>,
}

/// A wake batched for acceptance: coalesced triggers share one acceptance
/// record whose trigger-digest list names them (R-09/E-09).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WakeBatch {
    pub kind: RunKind,
    pub triggers: Vec<Trigger>,
}

// ---------- canonical records ----------

/// Canonical wake-acceptance decision payload (R-09/E-09).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WakeAcceptance {
    pub run_id: RunId,
    pub kind: RunKind,
    pub trigger_kind: TriggerKind,
    /// Coalesced trigger digests referenced by this acceptance.
    pub trigger_digests: Vec<Digest>,
}

/// Canonical denial record — the responsible constraint is named.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Denial {
    pub kind: RunKind,
    pub trigger_kind: TriggerKind,
    pub reason: DenialReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DenialReason {
    Paused,
    BreakerTripped(String),
    BudgetExhausted(String),
    ConcurrencyLimit,
    QueueFull,
    PolicyRejected,
}

/// Canonical run-start payload (R-09/E-09).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunStart {
    pub run_id: RunId,
    pub kind: RunKind,
    pub deadline: Option<u64>,
}

/// Terminal outcomes (R-18). `Blocked` records explicit budget exhaustion;
/// `Failed` carries the failure kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TerminalOutcome {
    Progress,
    NoProgress,
    Waiting,
    Blocked,
    Failed(FailureKind),
    CompletedGoal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FailureKind {
    Deadline,
    UserCancelled,
    Provider,
    Tool,
    Internal,
    /// The run was terminated by a branch transition quiesce (M6).
    Quiesced,
}

/// Canonical terminal-outcome payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunOutcome {
    pub run_id: RunId,
    pub outcome: TerminalOutcome,
    pub reason: Option<String>,
}

/// Canonical breaker trip (R-17/E-02): the responsible counter is named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BreakerTrip {
    pub counter: BreakerCounter,
    pub value: u64,
    pub threshold: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BreakerCounter {
    ConsecutiveFailed,
    NoProgressWithoutCausal,
    IdenticalAction,
    Spend,
}

// ---------- budgets ----------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budgets {
    /// Wall-clock deadline from run start (seconds); None = no deadline.
    pub deadline_secs: Option<u64>,
    /// Max provider tokens in+out for the run.
    pub tokens: Option<u64>,
    /// Max tool dispatches for the run.
    pub tools: Option<u64>,
    /// Max child spawns for the run.
    pub children: Option<u64>,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            deadline_secs: Some(120),
            tokens: Some(100_000),
            tools: Some(64),
            children: Some(8),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunUsage {
    pub tokens: u64,
    pub tools: u64,
    pub children: u64,
    pub started_at_secs: u64,
}

/// A spawned child run (R-09 child tool): tracked separately from the single
/// active parent run. Children are bounded by the caller — the session clamps
/// budgets before spawning (the kernel clamp) — and the scheduler records
/// them as given. The parent's `children` usage counter is NOT bumped here:
/// the session does that through [`Scheduler::record_usage`] so the canonical
/// record exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChildRun {
    pub run_id: RunId,
    pub parent: RunId,
    /// Always [`RunKind::Child`].
    pub kind: RunKind,
    pub budgets: Budgets,
    /// Wall clock at spawn, for the deadline check.
    pub started_at_secs: u64,
    pub usage: RunUsage,
}

// ---------- circuit breakers ----------

/// Kernel-owned breaker floors: policy may only raise these, never lower
/// them (R-17/E-02).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BreakerFloors {
    pub consecutive_failed: u32,
    pub no_progress_without_causal: u32,
    pub identical_action: u32,
    /// Window (seconds) and spend (tokens) for the spend breaker.
    pub spend_window_secs: u64,
    pub spend_tokens: u64,
}

impl Default for BreakerFloors {
    fn default() -> Self {
        Self {
            consecutive_failed: 3,
            no_progress_without_causal: 5,
            identical_action: 4,
            spend_window_secs: 300,
            spend_tokens: 500_000,
        }
    }
}

// ---------- step seam (R-18/E-01) ----------

/// Closed set of typed host commands a cognition step may issue; each
/// commits through its owning FSM. M3 implements model_call, tool_intent
/// (child_spawn routes through the tool FSM — R-09), and schedule_wake;
/// memory_* resolve to explicit Unavailable until M4 (never silently
/// dropped).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum StepCommand {
    ModelCall(ModelCallSpec),
    ToolIntent {
        tool: String,
        arguments: serde_json::Value,
    },
    MemoryQuery {
        query: String,
    },
    MemoryPropose {
        claim: serde_json::Value,
    },
    ChildSpawn {
        spec: serde_json::Value,
    },
    ScheduleWake {
        kind: TriggerKind,
        after_secs: u64,
    },
    Finish(TerminalOutcome),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCallSpec {
    /// Rendered-context hash the caller validated.
    pub rendered_hash: Digest,
    pub max_tokens: Option<u32>,
}

/// The generic replaceable cognition seam: `step(context, trigger)` is a
/// bounded orchestration body; the kernel checks the wake deadline/budget at
/// every host-command boundary.
/// The result of the previous host command, fed back into the next step so
/// the orchestration body can react (R-18/E-01: one bounded step per
/// accepted wake; the loop lives in the session actor).
#[derive(Debug, Clone)]
pub enum StepResult {
    Model(serde_json::Value),
    Tool(serde_json::Value),
    Memory(serde_json::Value),
    Child(serde_json::Value),
    Scheduled,
}

pub trait CognitionProvider {
    fn step(
        &mut self,
        context: &StepContext,
        trigger: &Trigger,
        last: Option<&StepResult>,
    ) -> Result<StepCommand, StepError>;
    fn as_any(&self) -> &dyn std::any::Any;
}

/// Frozen immutable projection handed to a step (R-18/E-01). M4 fills the
/// typed staged pipeline; M3 carries the rendered context plus the event
/// selection it came from.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StepContext {
    pub rendered: String,
    pub rendered_hash: Digest,
    pub selected_events: Vec<u64>,
    pub budget: Budgets,
    /// Staged-projection digest (M4); None for the M3 render seam.
    #[serde(default)]
    pub projection_digest: Option<Digest>,
    /// Pinned memory-root digests of the run's scopes (M4): [lifetime,
    /// project] when pinned, empty otherwise.
    #[serde(default)]
    pub memory_roots: Vec<Digest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
pub enum StepError {
    #[error("step unavailable: {0}")]
    Unavailable(String),
    #[error("invalid step: {0}")]
    Invalid(String),
}

// ---------- built-in default policy ----------

/// Scheduler policy seam: proposes wake batching/priority; Rust enforces all
/// bounds. The Luau policy module runtime is deferred (mirror of the R-20
/// retention deferral); the Rust built-in default ships.
pub trait SchedulerPolicy {
    fn name(&self) -> &str;
    /// Coalesce pending triggers into wake batches (kind + priority).
    fn coalesce(&self, pending: &[Trigger]) -> Vec<WakeBatch>;
    fn priority(&self, kind: RunKind) -> u8;
}

/// Built-in default: responder turns always outrank cognition; cognition
/// coalesces by trigger kind; children are lowest.
pub struct DefaultPolicy;

impl DefaultPolicy {
    fn batch_for(kind: RunKind, triggers: &[Trigger]) -> WakeBatch {
        WakeBatch {
            kind,
            triggers: triggers.to_vec(),
        }
    }
}

impl SchedulerPolicy for DefaultPolicy {
    fn name(&self) -> &str {
        "builtin-default"
    }

    fn coalesce(&self, pending: &[Trigger]) -> Vec<WakeBatch> {
        let mut user: Vec<Trigger> = Vec::new();
        let mut causal: Vec<Trigger> = Vec::new();
        let mut manual: Vec<Trigger> = Vec::new();
        let mut other: Vec<Trigger> = Vec::new();
        for t in pending {
            match t.kind {
                TriggerKind::UserMessage => user.push(t.clone()),
                TriggerKind::NewCausalEvent => causal.push(t.clone()),
                TriggerKind::ManualResume => manual.push(t.clone()),
                _ => other.push(t.clone()),
            }
        }
        let mut out = Vec::new();
        if !manual.is_empty() {
            out.push(Self::batch_for(RunKind::CognitionStep, &manual));
        }
        if !user.is_empty() {
            out.push(Self::batch_for(RunKind::ResponderTurn, &user));
        }
        if !causal.is_empty() {
            out.push(Self::batch_for(RunKind::CognitionStep, &causal));
        }
        if !other.is_empty() {
            out.push(Self::batch_for(RunKind::CognitionStep, &other));
        }
        out
    }

    fn priority(&self, kind: RunKind) -> u8 {
        match kind {
            RunKind::ResponderTurn => 3,
            RunKind::CognitionStep => 2,
            RunKind::Child => 1,
        }
    }
}

// ---------- scheduler ----------

pub type SchedulerResult<T> = Result<T, SchedulerError>;

#[derive(Debug, Error, PartialEq, Eq)]
pub enum SchedulerError {
    #[error("run {0} is not the active run")]
    NotActiveRun(RunId),
    #[error("no active run")]
    NoActiveRun,
    #[error("cognition is paused (breaker trip) until explicit resume")]
    Paused,
    #[error("budget exhausted: {0}")]
    BudgetExhausted(String),
    #[error("invalid transition: {0}")]
    Invalid(String),
}

/// Decision of `accept_wake`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WakeDecision {
    Accepted(WakeAcceptance),
    Denied(Denial),
}

/// Kernel-owned scheduler bounds. The session actor drives it; every
/// decision it returns is meant to be committed as a canonical record.
pub struct Scheduler {
    policy: Box<dyn SchedulerPolicy>,
    floors: BreakerFloors,
    paused: Option<BreakerTrip>,
    /// Pending triggers not yet batched (ephemeral).
    pending: VecDeque<Trigger>,
    /// Active run (one at a time — M3 runs serially through the session).
    active: Option<(RunId, RunKind, RunUsage)>,
    /// Breaker counters.
    consecutive_failed: u32,
    no_progress_run: u32,
    last_causal_event: Option<u64>,
    /// Identical action digests within the window (digest -> count).
    action_window: HashMap<Digest, (u64, u32)>,
    /// Spend within the current window (window start secs -> tokens).
    spend: (u64, u64),
    runs: HashMap<RunId, RunKind>,
    /// Child runs spawned by the active parent (R-09 child tool).
    children: HashMap<RunId, ChildRun>,
    outcomes: Vec<(RunId, TerminalOutcome)>,
    budgets: Budgets,
}

impl Scheduler {
    pub fn new(budgets: Budgets, floors: BreakerFloors) -> Self {
        let now = now_secs();
        Self {
            policy: Box::new(DefaultPolicy),
            floors,
            paused: None,
            pending: VecDeque::new(),
            active: None,
            consecutive_failed: 0,
            no_progress_run: 0,
            last_causal_event: None,
            action_window: HashMap::new(),
            spend: (now, 0),
            runs: HashMap::new(),
            children: HashMap::new(),
            outcomes: Vec::new(),
            budgets,
        }
    }

    pub fn with_policy(mut self, policy: Box<dyn SchedulerPolicy>) -> Self {
        self.policy = policy;
        self
    }

    pub fn policy_name(&self) -> &str {
        self.policy.name()
    }

    pub fn is_paused(&self) -> bool {
        self.paused.is_some()
    }

    pub fn paused_trip(&self) -> Option<BreakerTrip> {
        self.paused
    }

    pub fn active_run(&self) -> Option<RunId> {
        self.active.as_ref().map(|(id, _, _)| *id)
    }

    /// Explicit user resume after a breaker trip (R-17/E-02): clears the
    /// pause and resets the responsible counter.
    pub fn resume(&mut self) -> SchedulerResult<()> {
        match self.paused.take() {
            Some(trip) => {
                match trip.counter {
                    BreakerCounter::ConsecutiveFailed => self.consecutive_failed = 0,
                    BreakerCounter::NoProgressWithoutCausal => self.no_progress_run = 0,
                    BreakerCounter::IdenticalAction => self.action_window.clear(),
                    BreakerCounter::Spend => self.spend = (now_secs(), 0),
                }
                Ok(())
            }
            None => Err(SchedulerError::Invalid("resume without pause".into())),
        }
    }

    /// Record an observed trigger (ephemeral; batched at accept time).
    pub fn observe(&mut self, trigger: Trigger) {
        self.pending.push_back(trigger);
    }

    /// Accept the next wake batch under the policy, or deny with the
    /// responsible constraint. Responder priority: when a responder batch is
    /// pending and a cognition run is active, the active run is cancelled
    /// (the session classifies it `Failed(UserCancelled)` at the model-call
    /// boundary).
    pub fn accept_wake(&mut self, force_manual: bool) -> WakeDecision {
        if let Some(trip) = self.paused {
            return WakeDecision::Denied(Denial {
                kind: RunKind::CognitionStep,
                trigger_kind: TriggerKind::NewCausalEvent,
                reason: DenialReason::BreakerTripped(format!("{:?}", trip.counter)),
            });
        }
        if self.pending.is_empty() && !force_manual {
            return WakeDecision::Denied(Denial {
                kind: RunKind::CognitionStep,
                trigger_kind: TriggerKind::Timer,
                reason: DenialReason::PolicyRejected,
            });
        }
        if self.active.is_some() && !force_manual {
            return WakeDecision::Denied(Denial {
                kind: RunKind::CognitionStep,
                trigger_kind: TriggerKind::Timer,
                reason: DenialReason::ConcurrencyLimit,
            });
        }
        let pending: Vec<Trigger> = self.pending.drain(..).collect();
        let batches = self.policy.coalesce(&pending);
        let mut best: Option<(u8, WakeBatch)> = None;
        for b in batches {
            let p = self.policy.priority(b.kind);
            if best.as_ref().is_none_or(|(bp, _)| p > *bp) {
                best = Some((p, b));
            }
        }
        let batch = match best {
            Some((_, b)) => b,
            None => {
                return WakeDecision::Denied(Denial {
                    kind: RunKind::CognitionStep,
                    trigger_kind: TriggerKind::Timer,
                    reason: DenialReason::PolicyRejected,
                });
            }
        };
        // Requeue the losers for later.
        let mut all: Vec<Trigger> = pending;
        for t in &batch.triggers {
            all.retain(|x| x != t);
        }
        self.pending.extend(all);

        let kind = batch.kind;
        let trigger_kind = batch
            .triggers
            .first()
            .map(|t| t.kind)
            .unwrap_or(TriggerKind::Timer);
        let digests: Vec<Digest> = batch.triggers.iter().filter_map(|t| t.referent).collect();
        let run_id = RunId::generate();
        self.active = Some((
            run_id,
            kind,
            RunUsage {
                tokens: 0,
                tools: 0,
                children: 0,
                started_at_secs: now_secs(),
            },
        ));
        self.runs.insert(run_id, kind);
        WakeDecision::Accepted(WakeAcceptance {
            run_id,
            kind,
            trigger_kind,
            trigger_digests: digests,
        })
    }

    pub fn run_start(&mut self, run_id: RunId) -> SchedulerResult<RunStart> {
        // Child runs start straight from the child map — no wake acceptance
        // involved (the child was spawned, not woken).
        if let Some(child) = self.children.get(&run_id) {
            return Ok(RunStart {
                run_id,
                kind: RunKind::Child,
                deadline: child
                    .budgets
                    .deadline_secs
                    .map(|d| child.started_at_secs + d),
            });
        }
        let (id, kind, _) = self
            .active
            .as_ref()
            .filter(|(id, _, _)| *id == run_id)
            .ok_or(SchedulerError::NotActiveRun(run_id))?;
        Ok(RunStart {
            run_id: *id,
            kind: *kind,
            deadline: self.budgets.deadline_secs.map(|d| now_secs() + d),
        })
    }

    /// Record one causal event committed during the run (refreshes the
    /// no-progress breaker).
    pub fn record_causal_event(&mut self, seq: u64) {
        self.last_causal_event = Some(seq);
        self.no_progress_run = 0;
    }

    /// Record a run's usage (tokens/tools/children) and its terminal outcome.
    /// Returns the breaker trip when one fires.
    pub fn record_outcome(
        &mut self,
        run_id: RunId,
        outcome: TerminalOutcome,
        usage: RunUsage,
        action_digests: &[Digest],
    ) -> SchedulerResult<(RunOutcome, Option<BreakerTrip>)> {
        // Child runs: drop the entry and return without any breaker
        // interaction — breakers are cognition/responder concerns, and the
        // parent's budgets bound the child through the session's clamp.
        if self.children.remove(&run_id).is_some() {
            return Ok((
                RunOutcome {
                    run_id,
                    outcome,
                    reason: None,
                },
                None,
            ));
        }
        let (id, _kind, _) = match self.active {
            Some((id, kind, _)) if id == run_id => (id, kind, ()),
            _ => return Err(SchedulerError::NotActiveRun(run_id)),
        };
        self.active = None;
        self.runs.remove(&run_id);
        self.outcomes.push((run_id, outcome));

        let now = now_secs();
        let budget_ok = self.budgets.tokens.is_none_or(|b| usage.tokens <= b)
            && self.budgets.tools.is_none_or(|b| usage.tools <= b)
            && self.budgets.children.is_none_or(|b| usage.children <= b);
        let final_outcome = if budget_ok {
            outcome
        } else {
            TerminalOutcome::Blocked
        };

        // spend breaker
        if now - self.spend.0 > self.floors.spend_window_secs {
            self.spend = (now, 0);
        }
        self.spend.1 += usage.tokens;

        // identical-action breaker
        for d in action_digests {
            let entry = self.action_window.entry(*d).or_insert((now, 0));
            if now - entry.0 > self.floors.spend_window_secs {
                entry.0 = now;
                entry.1 = 0;
            }
            entry.1 += 1;
        }

        let trip = match final_outcome {
            TerminalOutcome::Failed(_) => {
                self.consecutive_failed += 1;
                if self.consecutive_failed >= self.floors.consecutive_failed {
                    Some(BreakerTrip {
                        counter: BreakerCounter::ConsecutiveFailed,
                        value: self.consecutive_failed as u64,
                        threshold: self.floors.consecutive_failed as u64,
                    })
                } else {
                    None
                }
            }
            TerminalOutcome::NoProgress | TerminalOutcome::Waiting => {
                if self.last_causal_event.is_none()
                    || self.outcomes.iter().rev().any(|(_, o)| {
                        matches!(
                            o,
                            TerminalOutcome::Progress | TerminalOutcome::CompletedGoal
                        )
                    })
                {
                    self.no_progress_run += 1;
                }
                if self.no_progress_run >= self.floors.no_progress_without_causal {
                    Some(BreakerTrip {
                        counter: BreakerCounter::NoProgressWithoutCausal,
                        value: self.no_progress_run as u64,
                        threshold: self.floors.no_progress_without_causal as u64,
                    })
                } else {
                    None
                }
            }
            TerminalOutcome::Progress | TerminalOutcome::CompletedGoal => {
                self.consecutive_failed = 0;
                self.no_progress_run = 0;
                None
            }
            TerminalOutcome::Blocked => None,
        };
        let trip = trip.or_else(|| {
            let ident = self
                .action_window
                .values()
                .map(|(_, c)| *c)
                .max()
                .unwrap_or(0);
            if ident >= self.floors.identical_action {
                Some(BreakerTrip {
                    counter: BreakerCounter::IdenticalAction,
                    value: ident as u64,
                    threshold: self.floors.identical_action as u64,
                })
            } else if self.spend.1 >= self.floors.spend_tokens {
                Some(BreakerTrip {
                    counter: BreakerCounter::Spend,
                    value: self.spend.1,
                    threshold: self.floors.spend_tokens,
                })
            } else {
                None
            }
        });
        if let Some(t) = trip {
            self.paused = Some(t);
        }
        Ok((
            RunOutcome {
                run_id: id,
                outcome: final_outcome,
                reason: None,
            },
            trip,
        ))
    }

    /// Check the wake deadline/budget at a host-command boundary (R-18/E-01).
    pub fn check_boundary(&self, run_id: RunId) -> SchedulerResult<()> {
        // Child runs enforce their own budgets (the caller clamped them at
        // spawn); the parent's budget is the session's boundary.
        if let Some(child) = self.children.get(&run_id) {
            if let Some(deadline) = child.budgets.deadline_secs
                && now_secs() > child.started_at_secs + deadline
            {
                return Err(SchedulerError::BudgetExhausted(format!(
                    "child {run_id}: deadline"
                )));
            }
            if child.budgets.tokens.is_some_and(|b| child.usage.tokens > b) {
                return Err(SchedulerError::BudgetExhausted(format!(
                    "child {run_id}: tokens"
                )));
            }
            return Ok(());
        }
        let (_id, _, usage) = self
            .active
            .as_ref()
            .filter(|(id, _, _)| *id == run_id)
            .ok_or(SchedulerError::NotActiveRun(run_id))?;
        if let Some(deadline) = self.budgets.deadline_secs
            && now_secs() > usage.started_at_secs + deadline
        {
            return Err(SchedulerError::BudgetExhausted("deadline".into()));
        }
        if self.budgets.tokens.is_some_and(|b| usage.tokens >= b) {
            return Err(SchedulerError::BudgetExhausted("tokens".into()));
        }
        if self.budgets.tools.is_some_and(|b| usage.tools >= b) {
            return Err(SchedulerError::BudgetExhausted("tools".into()));
        }
        Ok(())
    }

    /// Current accumulated usage of the active run (zeros when none).
    pub fn current_usage(&self, run_id: RunId) -> RunUsage {
        if let Some(child) = self.children.get(&run_id) {
            return child.usage;
        }
        self.active
            .as_ref()
            .filter(|(id, _, _)| *id == run_id)
            .map(|(_, _, usage)| *usage)
            .unwrap_or(RunUsage {
                tokens: 0,
                tools: 0,
                children: 0,
                started_at_secs: 0,
            })
    }

    pub fn record_usage(&mut self, run_id: RunId, usage: RunUsage) -> SchedulerResult<()> {
        if let Some(child) = self.children.get_mut(&run_id) {
            child.usage.tokens += usage.tokens;
            child.usage.tools += usage.tools;
            child.usage.children += usage.children;
            return Ok(());
        }
        let (_id, _, cur) = self
            .active
            .as_mut()
            .filter(|(id, _, _)| *id == run_id)
            .ok_or(SchedulerError::NotActiveRun(run_id))?;
        cur.tokens += usage.tokens;
        cur.tools += usage.tools;
        cur.children += usage.children;
        Ok(())
    }

    /// Spawn a child run under the active parent (R-09 child tool). The
    /// caller clamps `budgets` first (the kernel clamp is the session's job);
    /// the scheduler records them as given. The parent's `children` usage
    /// counter is NOT bumped here — the session does it via `record_usage` so
    /// the canonical record exists (documented on [`ChildRun`]).
    pub fn spawn_child(&mut self, parent: RunId, budgets: Budgets) -> SchedulerResult<RunId> {
        self.active
            .as_ref()
            .filter(|(id, _, _)| *id == parent)
            .ok_or(SchedulerError::NotActiveRun(parent))?;
        let run_id = RunId::generate();
        let now = now_secs();
        self.children.insert(
            run_id,
            ChildRun {
                run_id,
                parent,
                kind: RunKind::Child,
                budgets,
                started_at_secs: now,
                usage: RunUsage {
                    tokens: 0,
                    tools: 0,
                    children: 0,
                    started_at_secs: now,
                },
            },
        );
        Ok(run_id)
    }

    /// The child run's record, if `run_id` is a live child.
    pub fn child(&self, run_id: RunId) -> Option<&ChildRun> {
        self.children.get(&run_id)
    }

    pub fn run_kind(&self, run_id: RunId) -> Option<RunKind> {
        self.runs
            .get(&run_id)
            .copied()
            .or_else(|| self.children.get(&run_id).map(|c| c.kind))
    }

    pub fn budgets(&self) -> Budgets {
        self.budgets
    }

    pub fn set_budgets(&mut self, b: Budgets) {
        self.budgets = b;
    }

    pub fn completed_outcomes(&self) -> &[(RunId, TerminalOutcome)] {
        &self.outcomes
    }
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn trigger(kind: TriggerKind) -> Trigger {
        Trigger {
            kind,
            referent: None,
        }
    }

    #[test]
    fn wake_acceptance_and_run_lifecycle() {
        let mut s = Scheduler::new(Budgets::default(), BreakerFloors::default());
        s.observe(trigger(TriggerKind::NewCausalEvent));
        match s.accept_wake(false) {
            WakeDecision::Accepted(a) => {
                assert_eq!(a.kind, RunKind::CognitionStep);
                let start = s.run_start(a.run_id).unwrap();
                assert_eq!(start.run_id, a.run_id);
                let (outcome, trip) = s
                    .record_outcome(
                        a.run_id,
                        TerminalOutcome::CompletedGoal,
                        RunUsage {
                            tokens: 10,
                            tools: 1,
                            children: 0,
                            started_at_secs: 0,
                        },
                        &[],
                    )
                    .unwrap();
                assert_eq!(outcome.outcome, TerminalOutcome::CompletedGoal);
                assert!(trip.is_none());
            }
            other => panic!("expected accepted, got {other:?}"),
        }
    }

    #[test]
    fn responder_outranks_cognition() {
        let mut s = Scheduler::new(Budgets::default(), BreakerFloors::default());
        s.observe(trigger(TriggerKind::NewCausalEvent));
        s.observe(trigger(TriggerKind::UserMessage));
        match s.accept_wake(false) {
            WakeDecision::Accepted(a) => assert_eq!(a.kind, RunKind::ResponderTurn),
            other => panic!("expected responder, got {other:?}"),
        }
    }

    #[test]
    fn denial_names_responsible_constraint() {
        let mut s = Scheduler::new(Budgets::default(), BreakerFloors::default());
        let d = match s.accept_wake(false) {
            WakeDecision::Denied(d) => d,
            other => panic!("expected denial, got {other:?}"),
        };
        assert_eq!(d.reason, DenialReason::PolicyRejected);
    }

    #[test]
    fn consecutive_failed_trips_breaker_and_pauses() {
        let floors = BreakerFloors {
            consecutive_failed: 2,
            ..Default::default()
        };
        let mut s = Scheduler::new(Budgets::default(), floors);
        for _ in 0..2 {
            s.observe(trigger(TriggerKind::NewCausalEvent));
            let a = match s.accept_wake(false) {
                WakeDecision::Accepted(a) => a,
                other => panic!("expected accepted, got {other:?}"),
            };
            let (_, trip) = s
                .record_outcome(
                    a.run_id,
                    TerminalOutcome::Failed(FailureKind::Provider),
                    RunUsage {
                        tokens: 0,
                        tools: 0,
                        children: 0,
                        started_at_secs: 0,
                    },
                    &[],
                )
                .unwrap();
            if trip.is_some() {
                break;
            }
        }
        assert!(s.is_paused());
        match s.accept_wake(false) {
            WakeDecision::Denied(d) => assert!(matches!(d.reason, DenialReason::BreakerTripped(_))),
            other => panic!("expected breaker denial, got {other:?}"),
        }
        s.resume().unwrap();
        assert!(!s.is_paused());
    }

    #[test]
    fn budget_exhaustion_blocks_run() {
        let budgets = Budgets {
            tokens: Some(5),
            ..Default::default()
        };
        let mut s = Scheduler::new(budgets, BreakerFloors::default());
        s.observe(trigger(TriggerKind::NewCausalEvent));
        let a = match s.accept_wake(false) {
            WakeDecision::Accepted(a) => a,
            other => panic!("expected accepted, got {other:?}"),
        };
        let (outcome, _) = s
            .record_outcome(
                a.run_id,
                TerminalOutcome::Progress,
                RunUsage {
                    tokens: 100,
                    tools: 0,
                    children: 0,
                    started_at_secs: 0,
                },
                &[],
            )
            .unwrap();
        assert_eq!(outcome.outcome, TerminalOutcome::Blocked);
    }

    #[test]
    fn boundary_check_enforces_deadline_and_budget() {
        let budgets = Budgets {
            tokens: Some(5),
            deadline_secs: None,
            ..Default::default()
        };
        let mut s = Scheduler::new(budgets, BreakerFloors::default());
        s.observe(trigger(TriggerKind::NewCausalEvent));
        let a = match s.accept_wake(false) {
            WakeDecision::Accepted(a) => a,
            other => panic!("expected accepted, got {other:?}"),
        };
        s.record_usage(
            a.run_id,
            RunUsage {
                tokens: 5,
                tools: 0,
                children: 0,
                started_at_secs: 0,
            },
        )
        .unwrap();
        let err = s.check_boundary(a.run_id).unwrap_err();
        assert!(matches!(err, SchedulerError::BudgetExhausted(_)));
    }

    #[test]
    fn identical_action_breaker_fires() {
        let floors = BreakerFloors {
            identical_action: 2,
            ..Default::default()
        };
        let mut s = Scheduler::new(Budgets::default(), floors);
        let d = Digest::new(b"same-action");
        for _ in 0..2 {
            s.observe(trigger(TriggerKind::NewCausalEvent));
            let a = match s.accept_wake(false) {
                WakeDecision::Accepted(a) => a,
                other => panic!("expected accepted, got {other:?}"),
            };
            let (_, trip) = s
                .record_outcome(
                    a.run_id,
                    TerminalOutcome::NoProgress,
                    RunUsage {
                        tokens: 0,
                        tools: 0,
                        children: 0,
                        started_at_secs: 0,
                    },
                    &[d],
                )
                .unwrap();
            if trip.is_some() {
                break;
            }
        }
        assert!(s.is_paused());
        let trip = s.paused_trip().unwrap();
        assert_eq!(trip.counter, BreakerCounter::IdenticalAction);
    }

    #[test]
    fn causal_events_reset_no_progress() {
        let floors = BreakerFloors {
            no_progress_without_causal: 2,
            ..Default::default()
        };
        let mut s = Scheduler::new(Budgets::default(), floors);
        for i in 0..2 {
            s.observe(trigger(TriggerKind::NewCausalEvent));
            let a = match s.accept_wake(false) {
                WakeDecision::Accepted(a) => a,
                other => panic!("expected accepted, got {other:?}"),
            };
            s.record_causal_event(i);
            let (_, trip) = s
                .record_outcome(
                    a.run_id,
                    TerminalOutcome::NoProgress,
                    RunUsage {
                        tokens: 0,
                        tools: 0,
                        children: 0,
                        started_at_secs: 0,
                    },
                    &[],
                )
                .unwrap();
            assert!(trip.is_none(), "causal events must reset the breaker");
        }
        assert!(!s.is_paused());
    }

    /// Accepts a wake and returns the accepted run id (helper).
    fn accept(s: &mut Scheduler) -> RunId {
        s.observe(trigger(TriggerKind::NewCausalEvent));
        match s.accept_wake(false) {
            WakeDecision::Accepted(a) => a.run_id,
            other => panic!("expected accepted, got {other:?}"),
        }
    }

    #[test]
    fn spawn_child_requires_active_parent() {
        let mut s = Scheduler::new(Budgets::default(), BreakerFloors::default());
        let ghost = RunId::generate();
        // No active run at all.
        assert_eq!(
            s.spawn_child(ghost, Budgets::default()).unwrap_err(),
            SchedulerError::NotActiveRun(ghost)
        );
        // Active run present, but not the named parent.
        let parent = accept(&mut s);
        assert_eq!(
            s.spawn_child(ghost, Budgets::default()).unwrap_err(),
            SchedulerError::NotActiveRun(ghost)
        );
        // Correct parent spawns; the active run is untouched.
        let child = s.spawn_child(parent, Budgets::default()).unwrap();
        assert_eq!(s.active_run(), Some(parent));
        assert!(s.child(child).is_some());
    }

    #[test]
    fn child_run_start_outcome_roundtrip() {
        let mut s = Scheduler::new(Budgets::default(), BreakerFloors::default());
        let parent = accept(&mut s);
        let child = s.spawn_child(parent, Budgets::default()).unwrap();

        // run_kind resolves children (and the active run).
        assert_eq!(s.run_kind(child), Some(RunKind::Child));
        assert_eq!(s.run_kind(parent), Some(RunKind::CognitionStep));

        let start = s.run_start(child).unwrap();
        assert_eq!(start.run_id, child);
        assert_eq!(start.kind, RunKind::Child);
        assert!(
            start.deadline.is_some(),
            "child deadline derives from its budgets"
        );

        let (record, trip) = s
            .record_outcome(
                child,
                TerminalOutcome::CompletedGoal,
                RunUsage {
                    tokens: 3,
                    tools: 1,
                    children: 0,
                    started_at_secs: 0,
                },
                &[],
            )
            .unwrap();
        assert_eq!(record.outcome, TerminalOutcome::CompletedGoal);
        assert!(trip.is_none(), "children never trip breakers");
        assert!(s.child(child).is_none(), "outcome removes the child");
        assert_eq!(s.run_kind(child), None);
        // The parent run is unaffected.
        assert_eq!(s.run_kind(parent), Some(RunKind::CognitionStep));
    }

    #[test]
    fn child_boundary_enforces_deadline_and_tokens() {
        let mut s = Scheduler::new(Budgets::default(), BreakerFloors::default());
        let parent = accept(&mut s);
        let budgets = Budgets {
            deadline_secs: Some(10),
            tokens: Some(5),
            ..Default::default()
        };
        let child = s.spawn_child(parent, budgets).unwrap();

        // At exactly the token budget the boundary still passes (child rule:
        // over-budget errors).
        s.record_usage(
            child,
            RunUsage {
                tokens: 5,
                tools: 0,
                children: 0,
                started_at_secs: 0,
            },
        )
        .unwrap();
        assert!(s.check_boundary(child).is_ok());

        // Over the token budget → error naming the child and constraint.
        s.record_usage(
            child,
            RunUsage {
                tokens: 1,
                tools: 0,
                children: 0,
                started_at_secs: 0,
            },
        )
        .unwrap();
        let err = s.check_boundary(child).unwrap_err();
        assert!(matches!(err, SchedulerError::BudgetExhausted(m) if m.contains("tokens")));

        // Past the deadline → deadline error (independent of tokens).
        s.children.get_mut(&child).unwrap().started_at_secs -= 1_000;
        let err = s.check_boundary(child).unwrap_err();
        assert!(matches!(err, SchedulerError::BudgetExhausted(m) if m.contains("deadline")));
        // A fresh child (no deadline elapsed, within budget) passes.
        let fresh = s.spawn_child(parent, budgets).unwrap();
        assert!(s.check_boundary(fresh).is_ok());
    }

    #[test]
    fn child_record_usage_accumulates() {
        let mut s = Scheduler::new(Budgets::default(), BreakerFloors::default());
        let parent = accept(&mut s);
        let child = s.spawn_child(parent, Budgets::default()).unwrap();
        s.record_usage(
            child,
            RunUsage {
                tokens: 2,
                tools: 1,
                children: 0,
                started_at_secs: 0,
            },
        )
        .unwrap();
        s.record_usage(
            child,
            RunUsage {
                tokens: 3,
                tools: 1,
                children: 0,
                started_at_secs: 0,
            },
        )
        .unwrap();
        assert_eq!(s.current_usage(child).tokens, 5);
        assert_eq!(s.current_usage(child).tools, 2);
        assert_eq!(s.child(child).unwrap().usage.tokens, 5);
        // Unknown id: zeros, as before.
        assert_eq!(s.current_usage(RunId::generate()).tokens, 0);
    }
}
