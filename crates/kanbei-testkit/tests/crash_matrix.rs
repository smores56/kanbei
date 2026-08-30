//! M1 crash-injection gate (architecture.md acceptance bullet "Crash injection
//! at object install / event commit produces explicit valid recovery";
//! consistency tests 6 Crash + 7 Recovery): abort the child at every kernel
//! fault point under both fast and strict durability, then check the recovery
//! invariants via [`kanbei_testkit::verify_recovery`].

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use kanbei_core::queue::DurabilityQueue;
use kanbei_log::Profile;
use kanbei_objects::ObjectStore;
use kanbei_session::FaultPoint;
use kanbei_testkit::{child_acked, referenced_digests, spawn_crash_child, verify_recovery};

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

fn open_store(dir: &Path) -> (ObjectStore, Arc<DurabilityQueue>) {
    let queue = Arc::new(DurabilityQueue::start("kb-matrix-store"));
    let store = ObjectStore::open(&dir.join("objects"), Arc::clone(&queue)).unwrap();
    (store, queue)
}

fn shutdown_store(store: ObjectStore, queue: Arc<DurabilityQueue>) {
    drop(store);
    let q = Arc::try_unwrap(queue).unwrap_or_else(|_| panic!("store queue still shared"));
    q.shutdown().unwrap();
}

#[test]
fn crash_matrix_object_install_and_event_commit() {
    const POINTS: [FaultPoint; 4] = [
        FaultPoint::BeforeObjectInstall,
        FaultPoint::AfterObjectInstall,
        FaultPoint::BeforeFrameAppend,
        FaultPoint::AfterFrameAppend,
    ];
    // at least one combo must leave orphan objects (a crash before the frame
    // append strands the already-installed objects — R-10: orphans are
    // harmless, dangling refs are not)
    let mut saw_orphans = false;

    for point in POINTS {
        for profile in [Profile::Fast, Profile::Strict] {
            let dir = fresh_session_dir(&format!("matrix-{}", profile.name()));
            let _guard = DirGuard(dir.clone());

            let mut child = spawn_crash_child(&dir, Some(point), 4, 8, 2, profile, 1);
            let status = child.wait().unwrap();
            assert!(
                !status.success(),
                "child for {point:?} {:?} must crash, exited {status:?}",
                profile.name()
            );
            let acked = child_acked(&mut child);
            let rec = verify_recovery(&dir, acked)
                .unwrap_or_else(|e| panic!("{point:?} {:?}: {e}", profile.name()));

            let (store, queue) = open_store(&dir);
            let referenced = referenced_digests(&dir).unwrap();
            let (orphans, total) = store.prune_scan(&referenced).unwrap();
            if orphans > 0 {
                saw_orphans = true;
            }
            shutdown_store(store, queue);

            println!(
                "matrix {point:?} {:>8}: acked={acked} R={} truncated={} orphans={orphans}/{total}",
                profile.name(),
                rec.events,
                rec.truncated
            );
        }
    }
    assert!(saw_orphans, "expected at least one combo with orphan objects");
}

#[test]
fn crash_matrix_no_point_completes_cleanly() {
    let dir = fresh_session_dir("matrix-none");
    let _guard = DirGuard(dir.clone());

    let mut child = spawn_crash_child(&dir, None, 4, 8, 2, Profile::Fast, 1);
    let status = child.wait().unwrap();
    assert!(status.success(), "no-fault child must exit 0, got {status:?}");
    let acked = child_acked(&mut child);
    assert_eq!(acked, 8);

    let rec = verify_recovery(&dir, acked).unwrap();
    assert_eq!(rec.events, 8);
    assert_eq!(rec.last_seq, 8);
    assert!(!rec.truncated, "clean child: no torn tail");
}
