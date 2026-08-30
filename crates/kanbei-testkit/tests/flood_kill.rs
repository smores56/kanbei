//! M1 property-test framework deliverable (architecture.md R-21/H-06): seeded
//! SIGKILL floods. A child commits at full speed under the fast profile while
//! the test kills it after a seeded delay (≤ 50 ms); whatever the kill point,
//! recovery must be explicit and valid: every acked event survives, the
//! recovered log is contiguous, no committed reference dangles, and reopening
//! the session continues at R + 1. A torn tail is handled (truncated may be
//! true or false — both valid), never an error.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use kanbei_log::Profile;
use kanbei_testkit::{child_acked, rng::Rng, spawn_crash_child, verify_recovery};

/// 0x4B414E424549 = ASCII "KANBEI".
const SEED_BASE: u64 = 0x4B41_4E42_4549;

fn fresh_session_dir(name: &str) -> PathBuf {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let dir = std::env::temp_dir().join(format!(
        "kanbei-gate-{name}-{}-{}",
        std::process::id(),
        COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Best-effort cleanup at test end; never fail a test on cleanup errors.
struct DirGuard(PathBuf);
impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn flood_kill_seeded() {
    for seed in 0..5u64 {
        let dir = fresh_session_dir(&format!("flood-{seed}"));
        let _guard = DirGuard(dir.clone());
        let mut rng = Rng::new(SEED_BASE + seed);

        let mut child = spawn_crash_child(&dir, None, 0, 100_000, 2, Profile::Fast, 3);
        let delay_us = rng.next_usize(50_000);
        std::thread::sleep(Duration::from_micros(delay_us as u64));
        child.kill().expect("SIGKILL");
        let status = child.wait().unwrap();
        assert!(!status.success(), "flood seed {seed}: child must be killed");

        let acked = child_acked(&mut child);
        let rec = verify_recovery(&dir, acked)
            .unwrap_or_else(|e| panic!("flood seed {seed}: {e}"));
        println!(
            "flood seed={seed} delay={delay_us}us acked={acked} R={} truncated={}",
            rec.events, rec.truncated
        );
    }
}
