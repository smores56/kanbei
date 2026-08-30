//! kanbei-gc — automatic canonical-object garbage collection (M8 wave 2, the
//! R-20 deferred item; architecture.md: "A later GC requires coordinated root
//! capture, writer pins, quarantine, and a grace period from last reference").
//!
//! The engine is three-phase ([`GcRun::execute`]):
//!
//! 1. **collect** — a [`Collector`] walks the full canonical reference set
//!    (log refs, `$object` markers, snapshot-manifest closures, live roots,
//!    writer pins) against the store;
//! 2. **quarantine** — every store object outside that set is moved (atomic
//!    rename) into the store's sibling `.gc/` directory, where `scan()` never
//!    sees it;
//! 3. **sweep** (only when configured) — the collector re-runs (re-validating
//!    against concurrent writers), then quarantine files past their grace age
//!    are deleted — unless currently pinned or re-referenced, in which case
//!    the quarantine copy is dropped as a duplicate (or restored when the
//!    main-store copy is missing).
//!
//! The quarantine file's mtime IS the "grace period from last reference"
//! clock (the object store tracks no per-object metadata): an object is
//! deleted only after it has been unreferenced AND sitting in quarantine for
//! ≥ `GcConfig::grace`. Crashes are safe at every boundary — every operation
//! is idempotent and the sweep re-validates references before deleting.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::{Duration, SystemTime};

use kanbei_core::digest::Digest;
use kanbei_core::envelope::Envelope;
use kanbei_core::id::Id128;
use kanbei_objects::ObjectStore;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// GC tuning. The default grace is 7 days and sweep is off — a configured
/// GC quarantines on the first open after an upgrade and leaves the files
/// for a later sweep run (only the sweep consults the grace clock).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GcConfig {
    /// An object is deleted only after it has been unreferenced AND sitting
    /// in quarantine for at least this long.
    pub grace: Duration,
    /// Whether the sweep phase runs (deletes expired quarantine files).
    /// `false` = quarantine pass only.
    pub sweep: bool,
}

impl Default for GcConfig {
    fn default() -> Self {
        Self {
            grace: Duration::from_secs(7 * 24 * 60 * 60),
            sweep: false,
        }
    }
}

/// The canonical record of one GC run — serialized into the `gc.run` event
/// payload so every run is an inspectable fact.
#[derive(Serialize, Deserialize, Clone, Debug, PartialEq, Eq)]
pub struct GcReport {
    pub run_id: String,
    /// Objects on disk at collection time (phase 1 scan).
    pub scanned: u64,
    /// Distinct digests in the canonical reference set.
    pub referenced: u64,
    /// Objects moved into quarantine by this run.
    pub quarantined: u64,
    /// Quarantine files past their grace age deleted by this run.
    pub swept: u64,
    /// Quarantine files this run deleted as re-referenced duplicates or
    /// restored (main-store copy missing).
    pub restored_or_cleaned: u64,
    /// The grace period (whole seconds) this run applied.
    pub grace_secs: u64,
}

/// The accumulated canonical reference set.
#[derive(Default)]
pub struct ReferenceSet(HashSet<Digest>);

impl ReferenceSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, digest: Digest) {
        self.0.insert(digest);
    }

    pub fn extend(&mut self, digests: impl IntoIterator<Item = Digest>) {
        self.0.extend(digests);
    }

    pub fn contains(&self, digest: &Digest) -> bool {
        self.0.contains(digest)
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// A canonical-reference walker: adds every digest it can see (log refs,
/// closures, live roots) to `out`. Receives the store so manifest expansion
/// can fetch bytes; the collector itself must not retain it.
pub trait Collector {
    fn collect(&self, store: &ObjectStore, out: &mut ReferenceSet) -> Result<(), GcError>;
}

/// The three-phase GC engine (see the module doc).
pub struct GcRun;

impl GcRun {
    /// Phase 1 collect (reference_set = collector ∪ live writer pins), phase
    /// 2 quarantine (scan − reference_set → `.gc/`), phase 3 sweep (fresh
    /// collector run, grace age, live pin check at delete time).
    pub fn execute(
        store: &mut ObjectStore,
        collector: &dyn Collector,
        live_pins: &dyn Fn(&Digest) -> bool,
        config: &GcConfig,
    ) -> Result<GcReport, GcError> {
        let run_id = Id128::generate().to_string();

        // phase 1 — canonical root capture
        let mut set1 = ReferenceSet::new();
        collector.collect(store, &mut set1)?;
        let scanned_set: HashSet<Digest> = store.scan()?.into_iter().collect();
        let scanned = scanned_set.len() as u64;
        let referenced = set1.len() as u64;

        // phase 2 — quarantine everything unreferenced (move, never delete);
        // writer-pinned digests count as referenced and are never moved
        let candidates: Vec<Digest> = scanned_set
            .iter()
            .filter(|d| !set1.contains(d) && !live_pins(d))
            .copied()
            .collect();
        let quarantined = store.quarantine(&candidates)?.len() as u64;

        // phase 3 — sweep only when configured: re-run the collector (a
        // concurrent writer's references since phase 1 are re-validated),
        // then delete only quarantine files past their grace age, consulting
        // the live pin set at delete time.
        let mut swept = 0u64;
        let mut restored_or_cleaned = 0u64;
        if config.sweep {
            let mut set2 = ReferenceSet::new();
            collector.collect(store, &mut set2)?;
            let now = SystemTime::now();
            for (digest, mtime) in store.gc_quarantine_meta()? {
                if age(now, mtime) < config.grace {
                    // not unreferenced long enough — the grace period from
                    // last reference has not elapsed
                    continue;
                }
                if live_pins(&digest) {
                    // a writer has the object in flight — leave it for a
                    // later run
                    continue;
                }
                if set2.contains(&digest) {
                    // re-referenced: the main-store copy (install-dedup
                    // guarantee) wins — drop the duplicate; a missing main
                    // copy is restored rather than lost
                    if store.exists(&digest) {
                        store.delete(&digest)?;
                    } else {
                        store.restore(&digest)?;
                    }
                    restored_or_cleaned += 1;
                } else {
                    store.delete(&digest)?;
                    swept += 1;
                }
            }
        }

        Ok(GcReport {
            run_id,
            scanned,
            referenced,
            quarantined,
            swept,
            restored_or_cleaned,
            grace_secs: config.grace.as_secs(),
        })
    }
}

fn age(now: SystemTime, mtime: SystemTime) -> Duration {
    // a future mtime (clock skew) reads as age zero — it stays quarantined
    now.duration_since(mtime).unwrap_or(Duration::ZERO)
}

/// The memory-scope collector: every transition envelope references its
/// committed root manifest in `refs`; each manifest is expanded through its
/// digest fields (`parent`, `added_claims`, `added_edges`, `retracted`).
/// Plus the live actor head and fold digests.
pub struct MemoryCollector {
    log_path: PathBuf,
    live_roots: Vec<Digest>,
}

impl MemoryCollector {
    /// `fold_digests` = the live fold's claim/edge/retracted/history digests
    /// (defensive — the log walk already covers every committed manifest and
    /// its closure).
    pub fn new(
        log_path: impl Into<PathBuf>,
        head: Option<Digest>,
        fold_digests: Vec<Digest>,
    ) -> Self {
        let mut live_roots = fold_digests;
        if let Some(head) = head {
            live_roots.push(head);
        }
        Self {
            log_path: log_path.into(),
            live_roots,
        }
    }
}

impl Collector for MemoryCollector {
    fn collect(&self, store: &ObjectStore, out: &mut ReferenceSet) -> Result<(), GcError> {
        let mut manifests: Vec<Digest> = Vec::new();
        let log_path = self.log_path.clone();
        kanbei_log::for_each_frame(&log_path, |info| {
            for line in &info.events {
                let Ok(env) = Envelope::from_line(line) else {
                    continue;
                };
                for r in &env.refs {
                    out.insert(*r);
                    manifests.push(*r);
                }
            }
        })
        .map_err(|e| GcError::Log {
            path: self.log_path.clone(),
            source: e,
        })?;
        out.extend(self.live_roots.iter().copied());
        for digest in manifests {
            expand_memory_manifest(store, digest, out);
        }
        Ok(())
    }
}

/// Expands one root-manifest object: the parent chain link plus the
/// claim/edge digest fields. Unreadable manifests are skipped — the digest
/// itself stays referenced (the log records it); only its closure is
/// unknowable, and sweeping an unreachable closure member would be a
/// pre-existing inconsistency, never a GC decision.
fn expand_memory_manifest(store: &ObjectStore, digest: Digest, out: &mut ReferenceSet) {
    let Ok(bytes) = store.get(&digest) else {
        return;
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) else {
        return;
    };
    for key in ["parent", "added_claims", "added_edges", "retracted"] {
        match &value[key] {
            serde_json::Value::String(s) => add_parsed(s, out),
            serde_json::Value::Array(items) => {
                for item in items {
                    if let serde_json::Value::String(s) = item {
                        add_parsed(s, out);
                    }
                }
            }
            _ => {}
        }
    }
}

fn add_parsed(s: &str, out: &mut ReferenceSet) {
    if let Ok(digest) = s.parse::<Digest>() {
        out.insert(digest);
    }
}

/// GC failures: io errors and log-scan errors, each with enough context.
#[derive(Debug, Error)]
pub enum GcError {
    #[error("gc io: {0}")]
    Io(#[from] std::io::Error),
    #[error("gc log scan of {path}: {source}")]
    Log {
        path: PathBuf,
        #[source]
        source: kanbei_log::RecoveryError,
    },
}
