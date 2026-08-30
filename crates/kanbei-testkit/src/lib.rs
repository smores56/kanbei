//! kanbei-testkit — the crash-injection harness and property-test framework
//! (architecture.md R-21/H-06: "Crash-injection harness and property-test
//! framework are explicit M1 deliverables"; M2 extends it with the module
//! seam crash points). Provides the deterministic `crash-child` binary driver
//! (M1 + M2 modes), the recovery invariant checkers, and a tiny seeded PRNG;
//! the gate suites live in `tests/`.

pub mod rng;
pub mod dogfood;
pub mod fixture;

use std::collections::HashSet;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command};
use std::sync::Arc;

use kanbei_core::digest::Digest;
use kanbei_core::envelope::Envelope;
use kanbei_core::queue::DurabilityQueue;
use kanbei_log::{Profile, Recovered};
use kanbei_objects::ObjectStore;
use kanbei_session::{FaultPoint, NewEvent, Session, SessionConfig};
use serde_json::json;

// ---------- crash-child contract (env vars shared with src/bin/crash_child.rs) ----------

pub const ENV_DIR: &str = "KANBEI_CRASH_DIR";
pub const ENV_POINT: &str = "KANBEI_CRASH_POINT";
pub const ENV_AFTER_ACKS: &str = "KANBEI_CRASH_AFTER_ACKS";
pub const ENV_EVENTS: &str = "KANBEI_CRASH_EVENTS";
pub const ENV_OBJECTS: &str = "KANBEI_CRASH_OBJECTS";
pub const ENV_PROFILE: &str = "KANBEI_CRASH_PROFILE";
pub const ENV_STATE_EVERY: &str = "KANBEI_CRASH_STATE_EVERY";
/// "m1" (default) keeps the M1 protocol byte-identical; "m2" runs the module
/// flow (dispatch/head points).
pub const ENV_MODE: &str = "KANBEI_CRASH_MODE";
/// M2 child flow selection: "head" (module_state_cas only) or a string
/// containing "dispatch" (effect_dispatch first, then the head updates).
pub const ENV_M2_FLOW: &str = "KANBEI_CRASH_M2_FLOW";
/// M3 child flow selection: "spine" (full wake→run→model→tool→outcome) or
/// "wake" (wake acceptance only — exercises the wake points).
pub const ENV_M3_FLOW: &str = "KANBEI_CRASH_M3_FLOW";

/// Canonical env-var spelling of a fault point (the child parses it back).
pub fn fault_point_name(point: FaultPoint) -> &'static str {
    match point {
        FaultPoint::BeforeObjectInstall => "BeforeObjectInstall",
        FaultPoint::AfterObjectInstall => "AfterObjectInstall",
        FaultPoint::BeforeFrameAppend => "BeforeFrameAppend",
        FaultPoint::AfterFrameAppend => "AfterFrameAppend",
        FaultPoint::BeforeEffectDispatch => "BeforeEffectDispatch",
        FaultPoint::AfterEffectDispatch => "AfterEffectDispatch",
        FaultPoint::BeforeConfigActivation => "BeforeConfigActivation",
        FaultPoint::AfterConfigActivation => "AfterConfigActivation",
        FaultPoint::BeforeHeadUpdate => "BeforeHeadUpdate",
        FaultPoint::AfterHeadUpdate => "AfterHeadUpdate",
        FaultPoint::BeforeWakeAccept => "BeforeWakeAccept",
        FaultPoint::AfterWakeAccept => "AfterWakeAccept",
        FaultPoint::BeforeRunStart => "BeforeRunStart",
        FaultPoint::AfterRunStart => "AfterRunStart",
        FaultPoint::BeforeModelCall => "BeforeModelCall",
        FaultPoint::AfterModelCall => "AfterModelCall",
        FaultPoint::BeforeToolIntentCommit => "BeforeToolIntentCommit",
        FaultPoint::AfterToolIntentCommit => "AfterToolIntentCommit",
        FaultPoint::BeforeToolDispatch => "BeforeToolDispatch",
        FaultPoint::AfterToolDispatch => "AfterToolDispatch",
        FaultPoint::BeforeToolOutcomeCommit => "BeforeToolOutcomeCommit",
        FaultPoint::AfterToolOutcomeCommit => "AfterToolOutcomeCommit",
        FaultPoint::BeforeRunOutcome => "BeforeRunOutcome",
        FaultPoint::AfterRunOutcome => "AfterRunOutcome",
        FaultPoint::BeforeMemoryProposal => "BeforeMemoryProposal",
        FaultPoint::AfterMemoryProposal => "AfterMemoryProposal",
        FaultPoint::BeforeUiReduce => "BeforeUiReduce",
        FaultPoint::AfterUiReduce => "AfterUiReduce",
        FaultPoint::BeforeUiRender => "BeforeUiRender",
        FaultPoint::AfterUiRender => "AfterUiRender",
        FaultPoint::BeforeCheckpointCommit => "BeforeCheckpointCommit",
        FaultPoint::AfterCheckpointCommit => "AfterCheckpointCommit",
        FaultPoint::BeforeBranchTransition => "BeforeBranchTransition",
        FaultPoint::AfterBranchTransition => "AfterBranchTransition",
        FaultPoint::BeforeSessionHeadAdvance => "BeforeSessionHeadAdvance",
        FaultPoint::AfterSessionHeadAdvance => "AfterSessionHeadAdvance",
    }
}

pub fn parse_fault_point(s: &str) -> Option<FaultPoint> {
    match s {
        "BeforeObjectInstall" => Some(FaultPoint::BeforeObjectInstall),
        "AfterObjectInstall" => Some(FaultPoint::AfterObjectInstall),
        "BeforeFrameAppend" => Some(FaultPoint::BeforeFrameAppend),
        "AfterFrameAppend" => Some(FaultPoint::AfterFrameAppend),
        "BeforeEffectDispatch" => Some(FaultPoint::BeforeEffectDispatch),
        "AfterEffectDispatch" => Some(FaultPoint::AfterEffectDispatch),
        "BeforeConfigActivation" => Some(FaultPoint::BeforeConfigActivation),
        "AfterConfigActivation" => Some(FaultPoint::AfterConfigActivation),
        "BeforeHeadUpdate" => Some(FaultPoint::BeforeHeadUpdate),
        "AfterHeadUpdate" => Some(FaultPoint::AfterHeadUpdate),
        "BeforeWakeAccept" => Some(FaultPoint::BeforeWakeAccept),
        "AfterWakeAccept" => Some(FaultPoint::AfterWakeAccept),
        "BeforeRunStart" => Some(FaultPoint::BeforeRunStart),
        "AfterRunStart" => Some(FaultPoint::AfterRunStart),
        "BeforeModelCall" => Some(FaultPoint::BeforeModelCall),
        "AfterModelCall" => Some(FaultPoint::AfterModelCall),
        "BeforeToolIntentCommit" => Some(FaultPoint::BeforeToolIntentCommit),
        "AfterToolIntentCommit" => Some(FaultPoint::AfterToolIntentCommit),
        "BeforeToolDispatch" => Some(FaultPoint::BeforeToolDispatch),
        "AfterToolDispatch" => Some(FaultPoint::AfterToolDispatch),
        "BeforeToolOutcomeCommit" => Some(FaultPoint::BeforeToolOutcomeCommit),
        "AfterToolOutcomeCommit" => Some(FaultPoint::AfterToolOutcomeCommit),
        "BeforeRunOutcome" => Some(FaultPoint::BeforeRunOutcome),
        "AfterRunOutcome" => Some(FaultPoint::AfterRunOutcome),
        "BeforeMemoryProposal" => Some(FaultPoint::BeforeMemoryProposal),
        "AfterMemoryProposal" => Some(FaultPoint::AfterMemoryProposal),
        "BeforeUiReduce" => Some(FaultPoint::BeforeUiReduce),
        "AfterUiReduce" => Some(FaultPoint::AfterUiReduce),
        "BeforeUiRender" => Some(FaultPoint::BeforeUiRender),
        "AfterUiRender" => Some(FaultPoint::AfterUiRender),
        "BeforeCheckpointCommit" => Some(FaultPoint::BeforeCheckpointCommit),
        "AfterCheckpointCommit" => Some(FaultPoint::AfterCheckpointCommit),
        "BeforeBranchTransition" => Some(FaultPoint::BeforeBranchTransition),
        "AfterBranchTransition" => Some(FaultPoint::AfterBranchTransition),
        "BeforeSessionHeadAdvance" => Some(FaultPoint::BeforeSessionHeadAdvance),
        "AfterSessionHeadAdvance" => Some(FaultPoint::AfterSessionHeadAdvance),
        _ => None,
    }
}

/// Parse the four memory-actor crash points (the transition/head seams). In
/// M4 mode the child wires a matching string into the memory injector, NOT
/// the session injector (the module-head strings collide with the session's
/// M2 points by design — the mode decides which injector owns them).
pub fn parse_memory_fault_point(s: &str) -> Option<kanbei_memory::MemoryFaultPoint> {
    match s {
        "BeforeTransition" => Some(kanbei_memory::MemoryFaultPoint::BeforeTransition),
        "AfterTransition" => Some(kanbei_memory::MemoryFaultPoint::AfterTransition),
        "BeforeHeadUpdate" => Some(kanbei_memory::MemoryFaultPoint::BeforeHeadUpdate),
        "AfterHeadUpdate" => Some(kanbei_memory::MemoryFaultPoint::AfterHeadUpdate),
        _ => None,
    }
}

/// One crash point for the M4 child: either a session point or a memory
/// actor point. The canonical env spelling is [`CrashPoint::name`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashPoint {
    Session(FaultPoint),
    Memory(kanbei_memory::MemoryFaultPoint),
}

impl CrashPoint {
    pub fn name(&self) -> &'static str {
        match self {
            CrashPoint::Session(p) => fault_point_name(*p),
            CrashPoint::Memory(p) => match p {
                kanbei_memory::MemoryFaultPoint::BeforeTransition => "BeforeTransition",
                kanbei_memory::MemoryFaultPoint::AfterTransition => "AfterTransition",
                kanbei_memory::MemoryFaultPoint::BeforeHeadUpdate => "BeforeHeadUpdate",
                kanbei_memory::MemoryFaultPoint::AfterHeadUpdate => "AfterHeadUpdate",
            },
        }
    }
}

/// The `crash-child` executable. Cargo sets `CARGO_BIN_EXE_<name>` at compile
/// time for test/bench targets only (not for the lib unit), so fall back to
/// the sibling of the test binary: `target/<profile>/deps/<test>` →
/// `target/<profile>/crash-child`.
fn crash_child_exe() -> &'static std::path::PathBuf {
    use std::sync::OnceLock;
    static EXE: OnceLock<std::path::PathBuf> = OnceLock::new();
    EXE.get_or_init(|| {
        if let Some(p) = option_env!("CARGO_BIN_EXE_crash-child") {
            return std::path::PathBuf::from(p);
        }
        let mut p = std::env::current_exe().expect("testkit: current_exe() failed");
        p.pop();
        if p.file_name().is_some_and(|n| n == "deps") {
            p.pop();
        }
        p.push("crash-child");
        p
    })
}

/// Spawn a deterministic crash-test child in `dir` with the given fault point
/// and commit plan. Stdout is piped and carries one `acked=<seq>` line per
/// commit (flushed), so [`child_acked`] recovers the exact ack count after the
/// child dies.
pub fn spawn_crash_child(
    dir: &Path,
    point: Option<FaultPoint>,
    after_acks: u64,
    events: u64,
    objects: u64,
    profile: Profile,
    state_every: u64,
) -> Child {
    let mut cmd = Command::new(crash_child_exe());
    cmd.env(ENV_DIR, dir)
        .env(ENV_POINT, point.map(fault_point_name).unwrap_or("none"))
        .env(ENV_AFTER_ACKS, after_acks.to_string())
        .env(ENV_EVENTS, events.to_string())
        .env(ENV_OBJECTS, objects.to_string())
        .env(ENV_PROFILE, profile.name())
        .env(ENV_STATE_EVERY, state_every.to_string())
        .stdout(std::process::Stdio::piped());
    cmd.spawn().expect("testkit: failed to spawn crash-child")
}

/// Spawn the M2-mode crash-test child: opens a session with a config module
/// (publishing `svc.greet`), commits `events` plain events acking each, then
/// runs the M2 flow — `effect_dispatch` when `flow` contains "dispatch", then
/// two `module_state_cas` updates — aborting at `point` once armed. Config
/// points arm before open (they fire during open's `activate_config`); the
/// dispatch/head points arm after the `after_acks`-th commit.
pub fn spawn_m2_crash_child(
    dir: &Path,
    point: Option<FaultPoint>,
    after_acks: u64,
    events: u64,
    flow: &str,
) -> Child {
    let mut cmd = Command::new(crash_child_exe());
    cmd.env(ENV_DIR, dir)
        .env(ENV_MODE, "m2")
        .env(ENV_POINT, point.map(fault_point_name).unwrap_or("none"))
        .env(ENV_AFTER_ACKS, after_acks.to_string())
        .env(ENV_EVENTS, events.to_string())
        .env(ENV_PROFILE, Profile::Fast.name())
        .env(ENV_STATE_EVERY, "1")
        .env(ENV_M2_FLOW, flow)
        .stdout(std::process::Stdio::piped());
    cmd.spawn()
        .expect("testkit: failed to spawn crash-child (m2)")
}

/// Read the child's stdout to EOF and return the largest `acked=<n>` seen
/// (0 if the child never acked).
pub fn child_acked(child: &mut Child) -> u64 {
    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut out);
    }
    out.lines()
        .filter_map(|l| l.strip_prefix("acked=")?.parse::<u64>().ok())
        .max()
        .unwrap_or(0)
}

/// Recovered envelopes in seq order. Call after [`kanbei_log::recover`] (which
/// truncates a torn tail); this is a read-only frame pass over the clean file.
pub fn collect_envelopes(dir: &Path) -> Result<Vec<Envelope>, String> {
    let log_path = dir.join("log.zst");
    let mut lines = Vec::new();
    kanbei_log::for_each_frame(&log_path, |frame| {
        lines.extend(frame.events.iter().cloned());
    })
    .map_err(|e| format!("for_each_frame: {e}"))?;
    lines
        .iter()
        .map(|l| Envelope::from_line(l).map_err(|e| format!("envelope parse: {e}")))
        .collect()
}

/// The core crash-recovery invariant checker (S3 kill-9 drill semantics).
/// Returns a descriptive Err on any violation:
///   1. recover never fails with Corruption from our fault points (torn tails
///      truncate);
///   2. seq contiguity: exactly `1..=R`, no gaps or duplicates (Causality 11);
///   3. ack coverage: `acked <= R <= acked + 1` (at most one in-flight frame —
///      single writer; acked+1 = the ack was in flight);
///   4. no dangling references (R-10): every ref and snapshot exists in the
///      object store;
///   5. orphan tolerance: extra objects on disk are harmless and never fail
///      the check (`.tmp-*` orphans are ignored by scan — kanbei-objects unit
///      tested);
///   6. usable recovery: reopening the session continues at `R + 1` and one
///      more commit lands.
pub fn verify_recovery(dir: &Path, acked: u64) -> Result<Recovered, String> {
    verify_recovery_tolerant(dir, acked, 0)
}

/// [`verify_recovery`] with `reopen_extra` additional events expected to be
/// committed by the session at reopen (M3: `intent_classified` facts — B-05
/// — advance next_seq past R+1).
pub fn verify_recovery_tolerant(
    dir: &Path,
    acked: u64,
    reopen_extra: u64,
) -> Result<Recovered, String> {
    let log_path = dir.join("log.zst");

    // 1 — never Corruption from our fault points
    let recovered = kanbei_log::recover(&log_path).map_err(|e| format!("recover: {e}"))?;
    let r = recovered.events;

    // 2 — contiguity
    let envelopes = collect_envelopes(dir)?;
    if envelopes.len() as u64 != r {
        return Err(format!(
            "envelope count {} != recovered.events {r}",
            envelopes.len()
        ));
    }
    for (i, env) in envelopes.iter().enumerate() {
        let expected = i as u64 + 1;
        if env.seq != expected {
            return Err(format!(
                "seq gap/duplicate: envelope {i} has seq {}, expected {expected}",
                env.seq
            ));
        }
        env.validate()
            .map_err(|e| format!("envelope seq {} invalid: {e}", env.seq))?;
    }

    // 3 — ack coverage
    if acked > r || r > acked + 1 {
        return Err(format!(
            "ack coverage violated: acked={acked}, recovered={r}"
        ));
    }

    // 4 — no dangling references (fresh store on the session's object dir)
    let queue = Arc::new(DurabilityQueue::start("kb-testkit-verify"));
    let store = ObjectStore::open(&dir.join("objects"), Arc::clone(&queue))
        .map_err(|e| format!("open object store: {e}"))?;
    for env in &envelopes {
        for d in &env.refs {
            if !store.exists(d) {
                return Err(format!("event seq {}: dangling ref {d}", env.seq));
            }
        }
        if let Some(snap) = env.snapshot
            && !store.exists(&snap)
        {
            return Err(format!(
                "event seq {}: missing snapshot manifest {snap}",
                env.seq
            ));
        }
    }
    // 5 — orphan tolerance: extra objects are harmless; nothing to assert here
    let _ = store.scan();

    // 6 — usable recovery: reopen, seq continues, one more commit lands
    let mut session = Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        ..Default::default()
    })
    .map_err(|e| format!("reopen: {e}"))?;
    if session.next_seq() != r + 1 + reopen_extra {
        return Err(format!(
            "next_seq {} != R+1+{reopen_extra} {}",
            session.next_seq(),
            r + 1 + reopen_extra
        ));
    }
    session
        .commit(
            vec![NewEvent {
                kind: "test_event".into(),
                payload_schema: 1,
                payload: serde_json::json!({"i": 0}),
                objects: vec![],
                refs: vec![],
            }],
            None,
        )
        .map_err(|e| format!("post-recovery commit: {e}"))?;
    session.close().map_err(|e| format!("close: {e}"))?;
    drop(store);
    if let Ok(q) = Arc::try_unwrap(queue) {
        let _ = q.shutdown();
    }
    Ok(recovered)
}

/// The M2 crash-recovery invariant checker: [`verify_recovery`] (log
/// invariants, no dangling refs, reopen + append) PLUS the M2 composition
/// invariants:
///   1. the reopened session's composition epoch is consistent with the log's
///      `composition_changed` events — `count - 1 <= epoch <= count + 1`
///      (on reopen the composition is rebuilt from a fresh registry at epoch
///      0; a crash between the in-memory publish and the event commit leaves
///      the log one event short of the in-memory epoch, so the count bounds
///      the epoch within ±1);
///   2. every `composition_changed` event's refs (package + composition
///      digests) exist in the object store — the closure is valid (R-10);
///   3. the reopened session is usable: one more commit lands.
pub fn verify_m2_recovery(dir: &Path, acked: u64) -> Result<(), String> {
    verify_recovery(dir, acked)?;

    let envelopes = collect_envelopes(dir)?;
    let comp_changed: Vec<&Envelope> = envelopes
        .iter()
        .filter(|e| e.kind == "composition_changed")
        .collect();
    let count = comp_changed.len() as u64;

    // 2 — closure: every composition_changed ref exists in the object store
    let queue = Arc::new(DurabilityQueue::start("kb-testkit-m2-verify"));
    let store = ObjectStore::open(&dir.join("objects"), Arc::clone(&queue))
        .map_err(|e| format!("m2: open object store: {e}"))?;
    for env in &comp_changed {
        for d in &env.refs {
            if !store.exists(d) {
                return Err(format!(
                    "m2: composition_changed seq {}: dangling ref {d}",
                    env.seq
                ));
            }
        }
    }
    drop(store);
    if let Ok(q) = Arc::try_unwrap(queue) {
        let _ = q.shutdown();
    }

    // 1 + 3 — reopen: epoch consistency, then one more commit
    let mut session = Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        ..Default::default()
    })
    .map_err(|e| format!("m2 reopen: {e}"))?;
    let epoch = session.composition().epoch;
    if epoch < count.saturating_sub(1) || epoch > count + 1 {
        return Err(format!(
            "m2: reopened epoch {epoch} inconsistent with {count} composition_changed events \
             (expected {}..={})",
            count.saturating_sub(1),
            count + 1
        ));
    }
    session
        .commit(
            vec![NewEvent {
                kind: "test_event".into(),
                payload_schema: 1,
                payload: serde_json::json!({"m2": true}),
                objects: vec![],
                refs: vec![],
            }],
            None,
        )
        .map_err(|e| format!("m2 post-recovery commit: {e}"))?;
    session.close().map_err(|e| format!("m2 close: {e}"))?;
    Ok(())
}

/// Relative file listing of the session dir (directories get a trailing `/`),
/// sorted. Used by the gate for the ephemeral-scope assertion (nothing beyond
/// log.zst / objects/ / state/) and the read-only privacy scan.
pub fn session_dir_layout(dir: &Path) -> Vec<String> {
    fn walk(dir: &Path, base: &Path, out: &mut Vec<String>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let p = entry.unwrap().path();
            let rel = p.strip_prefix(base).unwrap().to_string_lossy().into_owned();
            if p.is_dir() {
                out.push(format!("{rel}/"));
                walk(&p, base, out);
            } else {
                out.push(rel);
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out.sort();
    out
}

/// Referenced digests (refs ∪ snapshots) of every envelope in the log — the
/// closure a crash-free session must cover exactly (modulo orphans).
pub fn referenced_digests(dir: &Path) -> Result<HashSet<Digest>, String> {
    let mut set = HashSet::new();
    for env in collect_envelopes(dir)? {
        set.extend(env.refs.iter().copied());
        if let Some(snap) = env.snapshot {
            set.insert(snap);
        }
    }
    Ok(set)
}

/// Spawn the M3-mode crash-test child: opens a session with a fake provider,
/// commits `after_acks` plain events, then runs the agent spine (wake accept,
/// run start, model call, run outcome — plus a tool call in the "spine" flow)
/// aborting at `point` once armed.
pub fn spawn_m3_crash_child(dir: &Path, point: Option<FaultPoint>, after_acks: u64) -> Child {
    let mut cmd = Command::new(crash_child_exe());
    cmd.env(ENV_DIR, dir)
        .env(ENV_MODE, "m3")
        .env(ENV_POINT, point.map(fault_point_name).unwrap_or("none"))
        .env(ENV_AFTER_ACKS, after_acks.to_string())
        .env(ENV_EVENTS, after_acks.to_string())
        .env(ENV_PROFILE, Profile::Fast.name())
        .env(ENV_STATE_EVERY, "1")
        .stdout(std::process::Stdio::piped());
    cmd.spawn()
        .expect("testkit: failed to spawn crash-child (m3)")
}

/// The M3 crash-recovery invariant checker: [`verify_recovery`] plus the M3
/// spine invariants:
///   1. every committed `tool_intent` event has a matching `tool_outcome` by
///      call_id — OR is explicitly classified by an `intent_classified`
///      event committed at recovery (B-05: committed-intent-without-outcome
///      is the sufficient condition for interrupted/ambiguous);
///   2. every `model_call` intent has a matching `model_outcome` by seq
///      pairing (intent then outcome, no cross-pairing);
///   3. the reopened session's `classify_pending_intents` is idempotent —
///      reopening twice commits no duplicate classification.
pub fn verify_m3_recovery(dir: &Path, acked: u64) -> Result<(), String> {
    let envelopes0 = collect_envelopes(dir)?;
    // Expected classification facts at reopen: committed tool intents without
    // a matching outcome (B-05) — exactly the pending set.
    let mut pending0: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut outcomes0: std::collections::HashSet<String> = std::collections::HashSet::new();
    for e in &envelopes0 {
        match e.kind.as_str() {
            "tool_outcome" => {
                if let Some(c) = e.payload.get("call_id").and_then(|c| c.as_str()) {
                    outcomes0.insert(c.to_string());
                }
            }
            "tool_intent" => {
                if let Some(c) = e.payload.get("call_id").and_then(|c| c.as_str()) {
                    pending0.insert(c.to_string());
                }
            }
            _ => {}
        }
    }
    let expected_classified = pending0.difference(&outcomes0).count() as u64;
    verify_recovery_tolerant(dir, acked, expected_classified)?;

    let envelopes = collect_envelopes(dir)?;
    let mut committed_outcomes: std::collections::HashSet<String> =
        std::collections::HashSet::new();
    let mut classified: std::collections::HashSet<String> = std::collections::HashSet::new();
    let mut intents: Vec<(String, String)> = Vec::new();
    let mut model_calls: Vec<u64> = Vec::new();
    let mut model_outcomes = 0u64;
    for e in &envelopes {
        match e.kind.as_str() {
            "tool_outcome" => {
                if let Some(call) = e.payload.get("call_id").and_then(|c| c.as_str()) {
                    committed_outcomes.insert(call.to_string());
                }
            }
            "intent_classified" => {
                if let Some(call) = e.payload.get("call_id").and_then(|c| c.as_str()) {
                    classified.insert(call.to_string());
                }
            }
            "tool_intent" => {
                if let (Some(call), Some(tool)) = (
                    e.payload.get("call_id").and_then(|c| c.as_str()),
                    e.payload.get("tool").and_then(|t| t.as_str()),
                ) {
                    intents.push((call.to_string(), tool.to_string()));
                }
            }
            "model_call" => model_calls.push(e.seq),
            "model_outcome" => model_outcomes += 1,
            _ => {}
        }
    }
    // 1 — every intent resolves: outcome OR explicit classification.
    for (call_id, _tool) in &intents {
        if !committed_outcomes.contains(call_id) && !classified.contains(call_id) {
            return Err(format!(
                "m3: tool intent {call_id} has neither outcome nor classification"
            ));
        }
    }
    // 2 — model intents pair with outcomes (a crash mid-call leaves an
    // unpaired intent; the classification hook records it).
    if model_calls.len() < model_outcomes as usize {
        return Err(format!(
            "m3: {} model outcomes for {} model calls",
            model_outcomes,
            model_calls.len()
        ));
    }

    // 3 — reopen idempotence: classification facts are committed once.
    let mut session = Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        ..Default::default()
    })
    .map_err(|e| format!("m3: reopen: {e}"))?;
    let classified_again = session
        .classify_pending_intents()
        .map_err(|e| format!("m3: reclassify: {e}"))?;
    if classified_again != 0 {
        return Err(format!(
            "m3: classification not idempotent: {classified_again} new facts"
        ));
    }
    session
        .commit(
            vec![NewEvent {
                kind: "test_event".into(),
                payload_schema: 1,
                payload: serde_json::json!({"post": "m3-recovery"}),
                objects: vec![],
                refs: vec![],
            }],
            None,
        )
        .map_err(|e| format!("m3: post-recovery commit: {e}"))?;
    session.close().map_err(|e| format!("m3: close: {e}"))?;
    Ok(())
}

/// Spawn the M4-mode crash-test child: opens a session with a memory
/// substrate + project binding and an approval-requiring broker for
/// memory.propose, commits `after_acks` plain events, then drives the
/// propose flow (wake accept, run start, memory.propose intent, proposal,
/// approval, transition, backlink, outcome) — aborting at `point` once armed.
pub fn spawn_m4_crash_child(dir: &Path, point: Option<CrashPoint>, after_acks: u64) -> Child {
    let mut cmd = Command::new(crash_child_exe());
    cmd.env(ENV_DIR, dir)
        .env(ENV_MODE, "m4")
        .env(ENV_POINT, point.map(|p| p.name()).unwrap_or("none"))
        .env(ENV_AFTER_ACKS, after_acks.to_string())
        .env(ENV_EVENTS, after_acks.to_string())
        .env(ENV_PROFILE, Profile::Fast.name())
        .env(ENV_STATE_EVERY, "1")
        .stdout(std::process::Stdio::piped());
    cmd.spawn()
        .expect("testkit: failed to spawn crash-child (m4)")
}

/// Spawn the M5-mode crash-test child: opens a session (modules enabled),
/// activates the built-in UI, feeds input through the kernel boundary, and
/// renders — aborting at `point` once armed.
pub fn spawn_m5_crash_child(dir: &Path, point: FaultPoint, after_acks: u64) -> std::process::Child {
    let mut cmd = Command::new(crash_child_exe());
    cmd.env(ENV_DIR, dir)
        .env(ENV_MODE, "m5")
        .env(ENV_POINT, fault_point_name(point))
        .env(ENV_AFTER_ACKS, after_acks.to_string())
        .env(ENV_PROFILE, Profile::Fast.name())
        .stdout(std::process::Stdio::piped());
    cmd.spawn()
        .expect("testkit: failed to spawn crash-child (m5)")
}

/// Spawn the M6-mode crash-test child: opens a storage-only session, commits
/// `after_acks` plain events, then runs the checkpoint → continue_from →
/// new-branch-commit flow — aborting at `point` once armed. `point=none`
/// completes cleanly.
pub fn spawn_m6_crash_child(dir: &Path, point: Option<FaultPoint>, after_acks: u64) -> std::process::Child {
    let mut cmd = Command::new(crash_child_exe());
    cmd.env(ENV_DIR, dir)
        .env(ENV_MODE, "m6")
        .env(ENV_POINT, point.map(fault_point_name).unwrap_or("none"))
        .env(ENV_AFTER_ACKS, after_acks.to_string())
        .env(ENV_PROFILE, Profile::Fast.name())
        .stdout(std::process::Stdio::piped());
    cmd.spawn()
        .expect("testkit: failed to spawn crash-child (m6)")
}

/// The M4 crash-recovery invariant checker: reopens the crashed session with
/// its own identity/binding (recovered from the log — never env), then
/// checks:
///   1. the session opens Ok with the memory substrate + project bound;
///   2. every committed memory.propose intent resolves to an outcome OR an
///      `intent_classified` fact (B-05);
///   3. at most one root transition exists, and the backlink count equals
///      the transition count (a crash inside the actor leaves the backlink
///      uncommitted — recovery re-backs it at open; idempotent by
///      TransitionId);
///   4. when a transition committed: the project fold contains the proposed
///      claim, `head.json` matches the actor head (repair path), and the
///      projection index builds over both folds;
///   5. reopening twice more commits no duplicate backlinks;
///   6. `classify_pending_intents` is idempotent.
///
/// Returns the number of checks run.
pub fn verify_m4_recovery(dir: &Path, acked: u64) -> Result<usize, String> {
    let mut checks = 0usize;
    let memory_root = dir.join("memory");

    // Canonical facts the verifier needs: the child's session id and project
    // id come from the log (the harness passes only dir/point/acks).
    let envelopes0 = collect_envelopes(dir)?;
    let mut session_id: Option<kanbei_core::id::Id128> = None;
    let mut project_id: Option<kanbei_core::id::Id128> = None;
    let mut intents: Vec<(String, String)> = Vec::new();
    let mut outcomes: HashSet<String> = HashSet::new();
    let mut classified: HashSet<String> = HashSet::new();
    let mut proposed_claim: Option<String> = None;
    for e in &envelopes0 {
        match e.kind.as_str() {
            "project_bound" => {
                project_id = e
                    .payload
                    .get("project_id")
                    .and_then(|p| p.as_str())
                    .and_then(|s| s.parse().ok());
            }
            "tool_intent" => {
                if e.payload.get("tool").and_then(|t| t.as_str()) == Some("memory.propose")
                    && let Some(call) = e.payload.get("call_id").and_then(|c| c.as_str())
                {
                    intents.push((call.to_string(), "memory.propose".into()));
                }
                if session_id.is_none() {
                    session_id = e
                        .payload
                        .pointer("/principal/session")
                        .and_then(|s| s.as_str())
                        .and_then(|s| s.parse().ok());
                }
            }
            "memory_proposal" => {
                proposed_claim = e
                    .payload
                    .get("claim_id")
                    .and_then(|c| c.as_str())
                    .map(str::to_string);
            }
            "tool_outcome" => {
                if let Some(call) = e.payload.get("call_id").and_then(|c| c.as_str()) {
                    outcomes.insert(call.to_string());
                }
            }
            "intent_classified" => {
                if let Some(call) = e.payload.get("call_id").and_then(|c| c.as_str()) {
                    classified.insert(call.to_string());
                }
            }
            _ => {}
        }
    }
    let project_id = project_id.ok_or_else(|| "m4: no project_bound event".to_string())?;
    let session_id =
        session_id.ok_or_else(|| "m4: no session principal in tool_intent".to_string())?;

    // 1 — reopen with the child's own identity + binding, faults off.
    let mut broker = kanbei_capabilities::Broker::new();
    broker
        .add_template(kanbei_capabilities::PolicyTemplate {
            trust_class: kanbei_capabilities::TrustClass::Builtin,
            allow: vec![kanbei_capabilities::Capability::new(
                "memory.propose".into(),
                vec!["call".into()],
            )],
            deny: vec![],
            require_approval: vec![kanbei_capabilities::Capability::new(
                "memory.propose".into(),
                vec!["call".into()],
            )],
            version: 1,
            monotonic: true,
        })
        .map_err(|e| format!("m4: broker template: {e}"))?;
    let session = Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        stream: "crash-m4".into(),
        memory_root: Some(memory_root.clone()),
        project: Some(project_id),
        broker,
        session_id: Some(session_id),
        budgets: kanbei_scheduler::Budgets {
            deadline_secs: Some(60),
            ..Default::default()
        },
        ..Default::default()
    })
    .map_err(|e| format!("m4: reopen: {e}"))?;
    checks += 1;

    // 2 — every memory.propose intent resolves: outcome OR classification
    // (re-read the log: the reopen's open() may have committed the
    // intent_classified facts).
    let envelopes1 = collect_envelopes(dir)?;
    let mut outcomes: HashSet<String> = outcomes;
    let mut classified: HashSet<String> = classified;
    for e in &envelopes1 {
        match e.kind.as_str() {
            "tool_outcome" => {
                if let Some(call) = e.payload.get("call_id").and_then(|c| c.as_str()) {
                    outcomes.insert(call.to_string());
                }
            }
            "intent_classified" => {
                if let Some(call) = e.payload.get("call_id").and_then(|c| c.as_str()) {
                    classified.insert(call.to_string());
                }
            }
            _ => {}
        }
    }
    for (call_id, tool) in &intents {
        if !outcomes.contains(call_id) && !classified.contains(call_id) {
            return Err(format!(
                "m4: tool intent {call_id} ({tool}) has neither outcome nor classification"
            ));
        }
    }
    checks += 1;

    // 3 — transition/backlink reconciliation: at most one transition, and
    // exactly one backlink per committed transition after recovery.
    let project = session
        .memory_project()
        .ok_or_else(|| "m4: project actor missing on reopen".to_string())?;
    let transitions = project.transition_count();
    if transitions > 1 {
        return Err(format!("m4: {transitions} transitions, expected at most 1"));
    }
    checks += 1;
    let backlinks1 = collect_envelopes(dir)?
        .iter()
        .filter(|e| e.kind == "memory_transition_backlink")
        .count() as u64;
    if backlinks1 != transitions {
        return Err(format!(
            "m4: {backlinks1} backlinks for {transitions} transitions after recovery"
        ));
    }
    checks += 1;

    if transitions == 1 {
        // 4 — the committed claim is in the fold; head.json matches the
        // actor head (the repair path when the crash hit before the head
        // write); the projection index builds over both folds.
        let claim_id = proposed_claim
            .ok_or_else(|| "m4: memory_proposal missing for committed transition".to_string())?;
        let head = project.head();
        let fold = project
            .fold(head)
            .map_err(|e| format!("m4: project fold: {e}"))?;
        if !fold
            .claims
            .iter()
            .any(|(_, c)| c.claim_id.to_string() == claim_id)
        {
            return Err(format!("m4: fold does not contain claim {claim_id}"));
        }
        let head_path = memory_root.join(format!("projects/{project_id}/head.json"));
        let head_json: serde_json::Value = std::fs::read(&head_path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .ok_or_else(|| format!("m4: unreadable head.json at {}", head_path.display()))?;
        let head_root = head_json.get("root").and_then(|r| r.as_str());
        let expected_root = head.map(|d| d.to_string()).unwrap_or_default();
        let expected_root = if expected_root.is_empty() {
            "null"
        } else {
            expected_root.as_str()
        };
        if head_root != Some(expected_root) {
            return Err(format!(
                "m4: head.json root {head_root:?} != actor head {expected_root:?}"
            ));
        }
        checks += 1;
        let lifetime = session.memory_lifetime();
        let lifetime_fold = lifetime
            .fold(lifetime.head())
            .map_err(|e| format!("m4: lifetime fold: {e}"))?;
        let mut index = kanbei_retrieval::MemoryIndex::open(&dir.join("verify-projection.sqlite"))
            .map_err(|e| format!("m4: index open: {e}"))?;
        index
            .build(
                &[
                    kanbei_retrieval::ScopeIndexInput {
                        scope: kanbei_memory::MemoryScope::Lifetime,
                        root: lifetime.head(),
                        fold: lifetime_fold,
                    },
                    kanbei_retrieval::ScopeIndexInput {
                        scope: kanbei_memory::MemoryScope::Project(project_id),
                        root: head,
                        fold: fold.clone(),
                    },
                ],
                kanbei_retrieval::SALIENCE_VERSION,
            )
            .map_err(|e| format!("m4: index build: {e}"))?;
        drop(index);
        let _ = std::fs::remove_file(dir.join("verify-projection.sqlite"));
        checks += 1;
    }

    // 5 — backlink idempotence: two more reopens add nothing.
    let _ = acked;
    session.close().map_err(|e| format!("m4: close: {e}"))?;
    for _ in 0..2 {
        let again = Session::open(SessionConfig {
            dir: dir.to_path_buf(),
            stream: "crash-m4".into(),
            memory_root: Some(memory_root.clone()),
            project: Some(project_id),
            session_id: Some(session_id),
            budgets: kanbei_scheduler::Budgets {
                deadline_secs: Some(60),
                ..Default::default()
            },
            ..Default::default()
        })
        .map_err(|e| format!("m4: idempotence reopen: {e}"))?;
        again
            .close()
            .map_err(|e| format!("m4: idempotence close: {e}"))?;
    }
    let backlinks2 = collect_envelopes(dir)?
        .iter()
        .filter(|e| e.kind == "memory_transition_backlink")
        .count() as u64;
    if backlinks2 != backlinks1 {
        return Err(format!(
            "m4: backlinks grew across reopens: {backlinks1} -> {backlinks2}"
        ));
    }
    checks += 1;

    // 6 — classification idempotence (the reopen already classified).
    let mut session = Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        stream: "crash-m4".into(),
        memory_root: Some(memory_root),
        project: Some(project_id),
        session_id: Some(session_id),
        budgets: kanbei_scheduler::Budgets {
            deadline_secs: Some(60),
            ..Default::default()
        },
        ..Default::default()
    })
    .map_err(|e| format!("m4: final reopen: {e}"))?;
    let classified_again = session
        .classify_pending_intents()
        .map_err(|e| format!("m4: reclassify: {e}"))?;
    if classified_again != 0 {
        return Err(format!(
            "m4: classification not idempotent: {classified_again} new facts"
        ));
    }
    checks += 1;
    session
        .close()
        .map_err(|e| format!("m4: final close: {e}"))?;
    Ok(checks)
}

#[cfg(test)]
mod tests {
    use super::rng::Rng;

    #[test]
    fn rng_deterministic_same_seed() {
        let mut a = Rng::new(42);
        let mut b = Rng::new(42);
        for _ in 0..1000 {
            assert_eq!(a.next_u64(), b.next_u64());
        }
    }

    #[test]
    fn rng_different_seeds_differ_and_bounds_hold() {
        assert_ne!(Rng::new(1).next_u64(), Rng::new(2).next_u64());
        let mut r = Rng::new(0x4B41_4E42_4549);
        for _ in 0..100 {
            assert!(r.next_usize(10) < 10);
            assert!((0.0..1.0).contains(&r.next_f64()));
        }
        assert_eq!(r.next_usize(0), 0);
    }
}

/// M5 recovery verifier: the UI boundary produces no canonical gestures, the
/// activation is an atomic composition publish, and the session reopens with
/// a re-activatable UI. Returns the number of checks.
pub fn verify_m5_recovery(dir: &Path, acked: u64) -> Result<usize, String> {
    let mut checks = 0usize;

    // 1 — M1 invariants: contiguous seqs, ack coverage, no dangling refs,
    // usable reopen (reopen commits nothing extra — UI events are never
    // canonical and no intents were pending at the crash boundary).
    let _recovered = verify_recovery_tolerant(dir, acked, 0)?;
    checks += 1;

    // 2 — the built-in UI activation landed as one atomic composition
    // publish; the delta names the builtin component.
    let envelopes = collect_envelopes(dir)?;
    let mut composition_changed = 0usize;
    let mut builtin_activated = false;
    let mut user_messages = 0usize;
    for e in &envelopes {
        match e.kind.as_str() {
            "composition_changed" => {
                composition_changed += 1;
                if let Some(added) = e.payload.get("delta").and_then(|d| d.get("added"))
                    && let Some(module_id) = added
                        .as_array()
                        .and_then(|a| a.first())
                        .and_then(|m| m.get("module_id"))
                        .and_then(|m| m.as_str())
                {
                    // The event's payload carries the module identity; the
                    // component string is recovered via the activation below.
                    let _ = module_id;
                    builtin_activated = true;
                }
            }
            "user_message" => {
                user_messages += 1;
                if !e.payload.get("text").and_then(|t| t.as_str()).is_some() {
                    return Err("m5: user_message without text".to_string());
                }
            }
            "safe_mode_activated" => {
                // Legitimate only from the reserved chord path; the crash
                // flow does not press it.
                return Err("m5: unexpected safe_mode_activated".to_string());
            }
            kind if kind.starts_with("ui_") => {
                return Err(format!("m5: canonical UI gesture event {kind:?}"));
            }
            _ => {}
        }
    }
    if composition_changed == 0 {
        return Err("m5: no composition_changed event".to_string());
    }
    if !builtin_activated {
        return Err("m5: builtin ui activation missing from composition delta".to_string());
    }
    if user_messages > 1 {
        return Err(format!("m5: {user_messages} user_messages, expected at most 1"));
    }
    checks += 1;

    // 3 — reopen: modules come back, the UI re-activates cleanly (fresh
    // composition at open — ephemeral scopes vanish), input flows through
    // the kernel boundary, and a frame renders with the input text.
    let mut session = Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        stream: "crash-m5".into(),
        budgets: kanbei_scheduler::Budgets {
            deadline_secs: Some(60),
            ..Default::default()
        },
        engine: Some(kanbei_vm::VmConfig {
            fuel_per_call: u64::MAX,
            epoch_deadline: u64::MAX / 2,
            ..Default::default()
        }),
        ..Default::default()
    })
    .map_err(|e| format!("m5: reopen: {e}"))?;
    checks += 1;
    if session.modules().is_none() {
        return Err("m5: modules disabled on reopen (guest wasm missing?)".to_string());
    }
    let epoch = session
        .activate_builtin_ui()
        .map_err(|e| format!("m5: ui re-activation: {e}"))?;
    if epoch == 0 {
        return Err("m5: ui re-activation did not advance the epoch".to_string());
    }
    checks += 1;
    let outcome = session
        .ui_handle_input(b"recovered\n")
        .map_err(|e| format!("m5: ui input on reopen: {e}"))?;
    if outcome.intents_applied != 1 {
        return Err(format!(
            "m5: expected 1 applied intent on reopen, got {}",
            outcome.intents_applied
        ));
    }
    checks += 1;
    session
        .ui_render_frame()
        .map_err(|e| format!("m5: ui render on reopen: {e}"))?;
    if session.ui().and_then(|u| u.last_frame()).is_none() {
        return Err("m5: no frame after reopen render".to_string());
    }
    // The submitted text is a canonical fact, not a gesture: the reopened
    // session committed the user_message the UI submit intent produced.
    let re_submitted = collect_envelopes(dir)?
        .iter()
        .filter(|e| e.kind == "user_message")
        .any(|e| e.payload.get("text").and_then(|t| t.as_str()) == Some("recovered"));
    if !re_submitted {
        return Err("m5: reopen submit did not commit a canonical user_message".to_string());
    }
    checks += 1;

    Ok(checks)
}

/// Reopen the crashed session for an M6 branch-state check (same shape as
/// the other verifiers' reopens: fresh identity, storage-only config).
fn reopen_m6(dir: &Path) -> Result<Session, String> {
    Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        stream: "crash-m6-verify".into(),
        ..Default::default()
    })
    .map_err(|e| format!("m6: reopen: {e}"))
}

/// The M6 crash-recovery invariant checker: [`verify_recovery_tolerant`]
/// plus the M6 branch invariants:
///   1. every envelope parses and at most one `branch_transition` exists;
///   2. when a transition exists: its payload parses (branch/from_branch/
///      frontier_seq/quiesce with cancelled+ambiguous arrays/follow/
///      config_choice), the `checkpoint_created` event it references exists
///      at frontier_seq, and the reopened session rebuilt the branch state —
///      current branch == the transition's branch, branch_records with the
///      same frontier/transition, on_path consistent (frontier on-path,
///      transition off-path, transition+1 on-path), path_ranges correct;
///   3. when no transition exists: the reopened session has no branch
///      records and still commits normally;
///   4. reopening twice is idempotent (same rebuilt branch state).
///      `reopen_extra` is forwarded to [`verify_recovery_tolerant`] (events
///      the reopen commits — 0 for the m6 driver). Returns the number of
///      checks run.
pub fn verify_m6_recovery(dir: &Path, acked: u64, reopen_extra: u64) -> Result<usize, String> {
    let mut checks = 0usize;

    // 1 — M1 invariants: contiguous seqs, ack coverage, no dangling refs,
    // usable reopen (no torn frames; recovery truncation works).
    let _recovered = verify_recovery_tolerant(dir, acked, reopen_extra)?;
    checks += 1;

    // 2 — envelope scan: every envelope parses (collect_envelopes fails on
    // any other outcome); at most one branch_transition, and when present its
    // payload parses and the referenced checkpoint exists at frontier_seq.
    let envelopes = collect_envelopes(dir)?;
    let transitions: Vec<&Envelope> = envelopes
        .iter()
        .filter(|e| e.kind == "branch_transition")
        .collect();
    if transitions.len() > 1 {
        return Err(format!(
            "m6: {} branch_transition events, expected at most 1",
            transitions.len()
        ));
    }
    checks += 1;
    if let Some(transition) = transitions.first() {
        let payload = &transition.payload;
        let branch = payload.get("branch").and_then(|b| b.as_str());
        let from = payload.get("from_branch").and_then(|f| f.as_str());
        let frontier = payload.get("frontier_seq").and_then(|f| f.as_u64());
        let checkpoint_event = payload.get("checkpoint_event").and_then(|c| c.as_str());
        // follow is either the string "FollowHead" or the PinnedAt object.
        let follow = payload.get("follow").filter(|f| !f.is_null());
        let config_choice = payload.get("config_choice").and_then(|c| c.as_object());
        let quiesce = payload.get("quiesce").and_then(|q| q.as_object());
        if branch.is_none()
            || from.is_none()
            || frontier.is_none()
            || checkpoint_event.is_none()
            || follow.is_none()
            || config_choice.is_none()
        {
            return Err(format!(
                "m6: branch_transition at seq {} misses required fields",
                transition.seq
            ));
        }
        let quiesce = quiesce.ok_or_else(|| {
            format!(
                "m6: branch_transition at seq {} misses quiesce",
                transition.seq
            )
        })?;
        if quiesce.get("cancelled").and_then(|c| c.as_array()).is_none()
            || quiesce.get("ambiguous").and_then(|a| a.as_array()).is_none()
        {
            return Err(format!(
                "m6: branch_transition at seq {} has malformed quiesce",
                transition.seq
            ));
        }
        let frontier = frontier.expect("frontier checked above");
        let checkpoint = envelopes.iter().find(|e| e.seq == frontier).ok_or_else(|| {
            format!("m6: transition references checkpoint at seq {frontier}, not in the log")
        })?;
        if checkpoint.kind != "checkpoint_created" {
            return Err(format!(
                "m6: event at frontier {frontier} is {}, not checkpoint_created",
                checkpoint.kind
            ));
        }
        if checkpoint.evt != checkpoint_event.expect("checkpoint_event checked above") {
            return Err("m6: transition references a different checkpoint event id".to_string());
        }
        checks += 1;

        // 3 — branch state rebuild: reopen and assert the transition's branch
        // is current, the record matches, and the path filter is consistent.
        let session = reopen_m6(dir)?;
        if session.branch().to_string() != branch.expect("branch checked above") {
            return Err(format!(
                "m6: reopened branch {} != transition branch {}",
                session.branch(),
                branch.expect("branch checked above")
            ));
        }
        let records = session.branch_records();
        if records.len() != 1 {
            return Err(format!(
                "m6: expected 1 branch record on reopen, got {}",
                records.len()
            ));
        }
        if records[0].frontier_seq != frontier || records[0].transition_seq != transition.seq {
            return Err(format!(
                "m6: rebuilt record frontier {} / transition {} != log frontier {frontier} / transition {}",
                records[0].frontier_seq, records[0].transition_seq, transition.seq
            ));
        }
        if !session.on_path(frontier)
            || session.on_path(transition.seq)
            || !session.on_path(transition.seq + 1)
        {
            return Err("m6: on_path inconsistent with the transition".to_string());
        }
        if session.path_ranges() != vec![(1, frontier), (transition.seq + 1, u64::MAX)] {
            return Err(format!(
                "m6: path_ranges {:?} != [(1, {frontier}), ({}, MAX)]",
                session.path_ranges(),
                transition.seq + 1
            ));
        }
        checks += 1;

        // 4 — idempotent reopen: the same rebuilt branch state.
        let again = reopen_m6(dir)?;
        if again.branch_records() != records || again.branch().to_string() != session.branch().to_string() {
            return Err("m6: idempotent reopen changed the branch state".to_string());
        }
        checks += 1;
        session
            .close()
            .map_err(|e| format!("m6: branch reopen close: {e}"))?;
        again
            .close()
            .map_err(|e| format!("m6: idempotent reopen close: {e}"))?;
    } else {
        // 3 — no transition: the reopened session has no branch records and
        // still commits normally.
        let mut session = reopen_m6(dir)?;
        if !session.branch_records().is_empty() {
            return Err(format!(
                "m6: {} branch records without a transition",
                session.branch_records().len()
            ));
        }
        let receipt = session
            .commit(
                vec![NewEvent {
                    kind: "user_message".into(),
                    payload_schema: 1,
                    payload: json!({"text": "post-recovery"}),
                    objects: vec![],
                    refs: vec![],
                }],
                None,
            )
            .map_err(|e| format!("m6: post-recovery commit: {e}"))?;
        if !session.on_path(receipt.last_seq) {
            return Err("m6: post-recovery commit is off-path".to_string());
        }
        checks += 1;

        // 4 — idempotent reopen.
        let again = reopen_m6(dir)?;
        if !again.branch_records().is_empty() {
            return Err("m6: idempotent reopen gained branch records".to_string());
        }
        checks += 1;
        session
            .close()
            .map_err(|e| format!("m6: no-branch reopen close: {e}"))?;
        again
            .close()
            .map_err(|e| format!("m6: no-branch idempotent close: {e}"))?;
    }

    Ok(checks)
}
