//! kanbei-testkit — the M1 crash-injection harness and property-test framework
//! (architecture.md R-21/H-06: "Crash-injection harness and property-test
//! framework are explicit M1 deliverables"). Provides the deterministic
//! `crash-child` binary driver, the recovery invariant checker, and a tiny
//! seeded PRNG; the gate suites live in `tests/`.

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

/// Canonical env-var spelling of a fault point (the child parses it back).
pub fn fault_point_name(point: FaultPoint) -> &'static str {
    match point {
        FaultPoint::BeforeObjectInstall => "BeforeObjectInstall",
        FaultPoint::AfterObjectInstall => "AfterObjectInstall",
        FaultPoint::BeforeFrameAppend => "BeforeFrameAppend",
        FaultPoint::AfterFrameAppend => "AfterFrameAppend",
    }
}

pub fn parse_fault_point(s: &str) -> Option<FaultPoint> {
    match s {
        "BeforeObjectInstall" => Some(FaultPoint::BeforeObjectInstall),
        "AfterObjectInstall" => Some(FaultPoint::AfterObjectInstall),
        "BeforeFrameAppend" => Some(FaultPoint::BeforeFrameAppend),
        "AfterFrameAppend" => Some(FaultPoint::AfterFrameAppend),
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
    if session.next_seq() != r + 1 {
        return Err(format!("next_seq {} != R+1 {}", session.next_seq(), r + 1));
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
