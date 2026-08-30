//! kanbei-testkit — the crash-injection harness and property-test framework
//! (architecture.md R-21/H-06: "Crash-injection harness and property-test
//! framework are explicit M1 deliverables"; M2 extends it with the module
//! seam crash points). Provides the deterministic `crash-child` binary driver
//! (M1 + M2 modes), the recovery invariant checkers, and a tiny seeded PRNG;
//! the gate suites live in `tests/`.

pub mod rng;

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
        _ => None,
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
    cmd.spawn().expect("testkit: failed to spawn crash-child (m2)")
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
        return Err(format!("envelope count {} != recovered.events {r}", envelopes.len()));
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
        return Err(format!("ack coverage violated: acked={acked}, recovered={r}"));
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
            return Err(format!("event seq {}: missing snapshot manifest {snap}", env.seq));
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
    cmd.spawn().expect("testkit: failed to spawn crash-child (m3)")
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
        return Err(format!("m3: classification not idempotent: {classified_again} new facts"));
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
