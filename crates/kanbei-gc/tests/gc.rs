//! kanbei-gc engine tests: the ObjectStore quarantine primitives and the
//! three-phase GC (root capture, quarantine, grace sweep) with custom
//! collectors, plus the memory-scope collector over a hand-built transition
//! log.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use kanbei_core::digest::Digest;
use kanbei_core::envelope::Envelope;
use kanbei_core::queue::DurabilityQueue;
use kanbei_gc::{Collector, GcConfig, GcRun, MemoryCollector, ReferenceSet};
use kanbei_log::AppendLog;
use kanbei_objects::ObjectStore;
use serde_json::json;

// --- helpers ---------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kb-gc-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Self(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn open_store(tag: &str, dir: &Path) -> (ObjectStore, Arc<DurabilityQueue>) {
    let queue = Arc::new(DurabilityQueue::start(&format!("kb-gc-{tag}")));
    let store = ObjectStore::open(&dir.join("objects"), Arc::clone(&queue)).unwrap();
    (store, queue)
}

/// A collector over exactly the digests given (no store reads).
struct FixedCollector(Vec<Digest>);

impl Collector for FixedCollector {
    fn collect(
        &self,
        _store: &ObjectStore,
        out: &mut ReferenceSet,
    ) -> Result<(), kanbei_gc::GcError> {
        out.extend(self.0.iter().copied());
        Ok(())
    }
}

fn sweep_cfg() -> GcConfig {
    GcConfig {
        grace: Duration::ZERO,
        sweep: true,
    }
}

// --- ObjectStore primitives ------------------------------------------------

#[test]
fn quarantine_moves_and_scan_untouched() {
    let tmp = TempDir::new("prim-move");
    let (mut store, queue) = open_store("prim-move", tmp.path());
    let a = store.install(b"quarantine me").unwrap();
    let b = store.install(b"keep me").unwrap();
    store.flush().unwrap();

    let moved = store.quarantine(&[a]).unwrap();
    assert_eq!(moved, vec![a]);
    assert_eq!(store.scan().unwrap(), vec![b], "scan sees only the store dir");
    assert_eq!(store.quarantined().unwrap(), vec![a]);
    assert!(matches!(
        store.get(&a),
        Err(kanbei_objects::ObjectError::Missing { digest }) if digest == a
    ));
    assert_eq!(store.get(&b).unwrap(), b"keep me");

    // idempotent: re-quarantining a moved object moves nothing
    assert_eq!(store.quarantine(&[a]).unwrap(), Vec::<Digest>::new());
    drop(store);
    drop(queue);
}

#[test]
fn delete_and_restore_are_idempotent() {
    let tmp = TempDir::new("prim-del");
    let (mut store, queue) = open_store("prim-del", tmp.path());
    let a = store.install(b"doomed").unwrap();
    let b = store.install(b"restored").unwrap();

    // delete: quarantine copy first, then store-dir copy
    store.quarantine(&[a]).unwrap();
    assert!(store.delete(&a).unwrap());
    assert!(!store.delete(&a).unwrap(), "missing is a no-op");
    assert!(store.quarantined().unwrap().is_empty());

    // restore: no-op when the main-store copy exists (install dedup)
    assert!(!store.restore(&b).unwrap());
    // restore: moves the quarantine copy back
    store.quarantine(&[b]).unwrap();
    assert!(store.restore(&b).unwrap());
    assert_eq!(store.scan().unwrap(), vec![b]);
    assert!(store.quarantined().unwrap().is_empty());
    // restore: no-op when no quarantine copy exists
    assert!(!store.restore(&b).unwrap());
    drop(store);
    drop(queue);
}

#[test]
fn quarantine_meta_reports_mtimes() {
    let tmp = TempDir::new("prim-meta");
    let (mut store, queue) = open_store("prim-meta", tmp.path());
    let a = store.install(b"meta me").unwrap();
    store.quarantine(&[a]).unwrap();
    let meta = store.gc_quarantine_meta().unwrap();
    assert_eq!(meta.len(), 1);
    assert_eq!(meta[0].0, a);
    assert!(
        meta[0]
            .1
            .elapsed()
            .expect("quarantine mtime is in the past")
            < Duration::from_secs(10)
    );
    drop(store);
    drop(queue);
}

// --- engine: collection / quarantine / sweep --------------------------------

#[test]
fn orphan_collected_and_swept_only_after_grace() {
    let tmp = TempDir::new("engine-orphan");
    let (mut store, queue) = open_store("engine-orphan", tmp.path());
    let orphan = store.install(b"unreferenced bytes").unwrap();
    let kept = store.install(b"referenced bytes").unwrap();

    // sweep=false: quarantine pass only — the orphan leaves the store, the
    // quarantine copy survives
    let report = GcRun::execute(
        &mut store,
        &FixedCollector(vec![kept]),
        &|_| false,
        &GcConfig {
            grace: Duration::from_secs(3600),
            sweep: false,
        },
    )
    .unwrap();
    assert_eq!(report.scanned, 2);
    assert_eq!(report.referenced, 1);
    assert_eq!(report.quarantined, 1);
    assert_eq!(report.swept, 0);
    assert_eq!(store.quarantined().unwrap(), vec![orphan]);
    assert_eq!(store.scan().unwrap(), vec![kept]);

    // sweep with a grace the quarantine file has NOT aged through: survives
    let report = GcRun::execute(
        &mut store,
        &FixedCollector(vec![kept]),
        &|_| false,
        &GcConfig {
            grace: Duration::from_secs(3600),
            sweep: true,
        },
    )
    .unwrap();
    assert_eq!(report.swept, 0, "younger than grace — stays in quarantine");
    assert_eq!(store.quarantined().unwrap(), vec![orphan]);

    // sweep with grace elapsed (zero): the orphan is deleted
    let report = GcRun::execute(&mut store, &FixedCollector(vec![kept]), &|_| false, &sweep_cfg())
        .unwrap();
    assert_eq!(report.swept, 1);
    assert_eq!(report.restored_or_cleaned, 0);
    assert!(store.quarantined().unwrap().is_empty());
    assert!(!store.exists(&orphan), "orphan deleted from disk");
    drop(store);
    drop(queue);
}

#[test]
fn referenced_objects_survive_quarantine_and_sweep() {
    let tmp = TempDir::new("engine-ref");
    let (mut store, queue) = open_store("engine-ref", tmp.path());
    let kept = store.install(b"referenced").unwrap();
    let report = GcRun::execute(&mut store, &FixedCollector(vec![kept]), &|_| false, &sweep_cfg())
        .unwrap();
    assert_eq!(report.quarantined, 0);
    assert_eq!(report.swept, 0);
    assert_eq!(store.get(&kept).unwrap(), b"referenced");
    drop(store);
    drop(queue);
}

#[test]
fn writer_pins_skip_quarantine_and_sweep() {
    let tmp = TempDir::new("engine-pin");
    let (mut store, queue) = open_store("engine-pin", tmp.path());
    let pinned = store.install(b"writer in flight").unwrap();
    let orphan = store.install(b"orphan").unwrap();

    // phase 2: a pinned digest is never quarantined; an unpinned one is
    // (quarantine pass only)
    GcRun::execute(
        &mut store,
        &FixedCollector(vec![]),
        &|d| *d == pinned,
        &GcConfig {
            grace: Duration::ZERO,
            sweep: false,
        },
    )
    .unwrap();
    assert!(store.exists(&pinned), "pinned digest stays in the store");
    assert!(store.quarantined().unwrap().contains(&orphan));

    // the pin is transient: with no pin, the same pass quarantines it
    GcRun::execute(
        &mut store,
        &FixedCollector(vec![]),
        &|_| false,
        &GcConfig {
            grace: Duration::ZERO,
            sweep: false,
        },
    )
    .unwrap();
    assert!(store.quarantined().unwrap().contains(&pinned));

    // sweep with a live pin at delete time: the pinned quarantine file
    // survives; the unpinned one is swept
    let report = GcRun::execute(
        &mut store,
        &FixedCollector(vec![]),
        &|d| *d == pinned,
        &sweep_cfg(),
    )
    .unwrap();
    assert_eq!(report.swept, 1, "only the unpinned quarantine file");
    assert_eq!(report.restored_or_cleaned, 0);
    assert!(store.quarantined().unwrap().contains(&pinned));
    assert!(!store.quarantined().unwrap().contains(&orphan));

    // unpin: the next sweep collects it
    let report = GcRun::execute(&mut store, &FixedCollector(vec![]), &|_| false, &sweep_cfg())
        .unwrap();
    assert_eq!(report.swept, 1);
    assert!(store.quarantined().unwrap().is_empty());
    drop(store);
    drop(queue);
}

#[test]
fn re_referenced_quarantine_copy_is_cleaned_or_restored() {
    let tmp = TempDir::new("engine-dupe");
    let (mut store, queue) = open_store("engine-dupe", tmp.path());
    let bytes = b"duplicate cleanup";
    let digest = store.install(bytes).unwrap();

    // quarantine the object (quarantine pass only), then a writer re-installs
    // it (fresh main-store file — content addressing dedups)
    GcRun::execute(
        &mut store,
        &FixedCollector(vec![]),
        &|_| false,
        &GcConfig {
            grace: Duration::ZERO,
            sweep: false,
        },
    )
    .unwrap();
    assert!(store.quarantined().unwrap().contains(&digest));
    assert_eq!(store.install(bytes).unwrap(), digest);
    assert!(store.exists(&digest));

    // sweep: the quarantine copy is a duplicate — deleted, main copy wins
    let report = GcRun::execute(
        &mut store,
        &FixedCollector(vec![digest]),
        &|_| false,
        &sweep_cfg(),
    )
    .unwrap();
    assert_eq!(report.swept, 0);
    assert_eq!(report.restored_or_cleaned, 1);
    assert!(store.quarantined().unwrap().is_empty());
    assert_eq!(store.get(&digest).unwrap(), bytes);

    // restore path: re-referenced with the main copy missing (a reference
    // the quarantine predates) — the quarantine copy comes back
    let d2 = store.install(b"restore me").unwrap();
    GcRun::execute(
        &mut store,
        &FixedCollector(vec![]),
        &|_| false,
        &GcConfig {
            grace: Duration::ZERO,
            sweep: false,
        },
    )
    .unwrap();
    let report = GcRun::execute(
        &mut store,
        &FixedCollector(vec![d2]),
        &|_| false,
        &sweep_cfg(),
    )
    .unwrap();
    assert_eq!(report.restored_or_cleaned, 1);
    assert!(store.quarantined().unwrap().is_empty());
    assert_eq!(store.get(&d2).unwrap(), b"restore me");
    drop(store);
    drop(queue);
}

// --- memory-scope collector -------------------------------------------------

/// Builds a scope dir with one committed transition: one claim object, one
/// root manifest referencing it, and the transition envelope (refs =
/// manifest digest). Returns (store, claim digest, manifest digest).
fn seed_memory_scope(dir: &Path) -> (ObjectStore, Arc<DurabilityQueue>, Digest, Digest) {
    let queue = Arc::new(DurabilityQueue::start("kb-gc-mem-seed"));
    let mut store = ObjectStore::open(&dir.join("objects"), Arc::clone(&queue)).unwrap();
    let claim = b"{\"schema\":1,\"claim_id\":\"clm_test\",\"kind\":\"decision\"}";
    let claim_digest = store.install(claim).unwrap();
    let manifest_bytes = json!({
        "schema": 1,
        "parent": null,
        "scope": "lifetime",
        "added_claims": [claim_digest.to_string()],
        "added_edges": [],
        "retracted": [],
        "transition_id": "tr_test",
    })
    .to_string();
    let manifest_digest = store.install(manifest_bytes.as_bytes()).unwrap();
    let envelope = Envelope {
        env: kanbei_core::envelope::ENVELOPE_SCHEMA,
        seq: 1,
        evt: "evt_test".into(),
        kind: "memory_transition".into(),
        payload_schema: 1,
        payload: json!({}),
        refs: vec![manifest_digest],
        snapshot: None,
    };
    let mut log = AppendLog::open(
        &dir.join("transitions.jsonl.zst"),
        "memory-transitions",
        Arc::clone(&queue),
    )
    .unwrap();
    log.append(&[envelope], kanbei_log::Profile::Strict).unwrap();
    log.flush().unwrap();
    drop(log);
    (store, queue, claim_digest, manifest_digest)
}

#[test]
fn memory_collector_covers_log_manifests_and_expands_closures() {
    let tmp = TempDir::new("mem-collect");
    let scope_dir = tmp.path().join("lifetime");
    std::fs::create_dir_all(&scope_dir).unwrap();
    let (mut store, queue, claim_digest, manifest_digest) =
        seed_memory_scope(&scope_dir);
    let orphan = store.install(b"memory orphan").unwrap();

    // collect: the manifest (log ref), the claim (manifest closure), and the
    // live head are all referenced
    let collector = MemoryCollector::new(
        scope_dir.join("transitions.jsonl.zst"),
        Some(manifest_digest),
        vec![claim_digest],
    );
    let mut set = ReferenceSet::new();
    collector.collect(&store, &mut set).unwrap();
    assert!(set.contains(&manifest_digest));
    assert!(set.contains(&claim_digest));

    // run: the claim + manifest survive; the orphan is swept
    let report = GcRun::execute(&mut store, &collector, &|_| false, &sweep_cfg()).unwrap();
    assert_eq!(report.quarantined, 1);
    assert_eq!(report.swept, 1);
    assert!(!store.exists(&orphan));
    assert!(store.exists(&claim_digest));
    assert!(store.exists(&manifest_digest));
    drop(store);
    drop(queue);
}
