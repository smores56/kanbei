//! M8 wave 2: automatic canonical-object GC — the session-side root capture,
//! writer pins, and the session/memory entry points over the kanbei-gc
//! engine (architecture.md: "A later GC requires coordinated root capture,
//! writer pins, quarantine, and a grace period from last reference").

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Mutex;

use kanbei_core::digest::Digest;
use kanbei_core::envelope::Envelope;
use kanbei_gc::{Collector, GcConfig, GcReport, ReferenceSet};
use serde_json::json;

use crate::{NewEvent, Session, SessionError};

impl Session {
    /// The live canonical roots no log record covers: the current snapshot
    /// (the genesis manifest is pinned at open before any event references
    /// it), the activated config package, checkpoint-pinned memory roots,
    /// branch config-choice digests, and compaction summary digests.
    fn gc_live_roots(&self) -> Vec<Digest> {
        let mut roots: Vec<Digest> = Vec::new();
        if let Some(snapshot) = self.current_snapshot {
            roots.push(snapshot);
        }
        if let Some(config) = self.config_digest {
            roots.push(config);
        }
        if let Some(pinned) = &self.pinned_roots {
            roots.push(pinned.lifetime);
            if let Some(project) = pinned.project {
                roots.push(project);
            }
        }
        for record in &self.branch_records {
            if let Some(current) = record.config_choice.current {
                roots.push(current);
            }
            if let Some(historical) = record.config_choice.historical {
                roots.push(historical);
            }
            if let Some(composition) = record.config_choice.composition {
                roots.push(composition);
            }
        }
        for range in &self.compacted {
            roots.push(range.summary_digest);
        }
        roots
    }

    /// Runs the session-store GC (root capture, quarantine, grace sweep) and
    /// then commits a canonical `gc.run` record event — a state-changing
    /// maintenance fact, snapshot-pinned like any other commit. The report
    /// is the inspectable outcome; the event vocabulary is the free-string
    /// NewEvent kind space (no FSM registration needed — only
    /// `compaction_selected` and `memory_follow_changed` have commit-path
    /// handlers, and neither matches this kind).
    pub fn run_gc(&mut self, config: GcConfig) -> Result<GcReport, SessionError> {
        let collector = SessionCollector {
            log_path: self.log_path.clone(),
            live_roots: self.gc_live_roots(),
        };
        let pins = &self.gc_pins;
        let report = kanbei_gc::GcRun::execute(
            &mut self.store,
            &collector,
            &|digest| pins.lock().expect("gc pins lock poisoned").contains(digest),
            &config,
        )?;
        self.commit(
            vec![NewEvent {
                kind: "gc.run".into(),
                payload_schema: 1,
                payload: json!({
                    "report": serde_json::to_value(&report)
                        .expect("gc report serialization cannot fail"),
                }),
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            Some(self.composition.current().digest),
        )?;
        #[cfg(feature = "otel")]
        {
            self.telemetry_gc(&report);
            // storage gauges after a sweep — best-effort export, never fails
            // the GC
            let _ = self.report_storage();
        }
        Ok(report)
    }

    /// Runs GC over the lifetime and (when bound) project memory stores.
    /// Returns one report per scope, in actor order.
    pub fn run_memory_gc(
        &mut self,
        config: GcConfig,
    ) -> Result<Vec<(kanbei_memory::MemoryScope, GcReport)>, SessionError> {
        let mut reports = Vec::new();
        reports.push((
            self.memory_lifetime.scope().clone(),
            self.memory_lifetime.run_gc(config.clone())?,
        ));
        if let Some(actor) = &mut self.memory_project {
            reports.push((actor.scope().clone(), actor.run_gc(config)?));
        }
        Ok(reports)
    }

    /// The open-time automatic pass, best-effort by design: a GC failure
    /// must never fail session open (the explicit [`Session::run_gc`]
    /// surfaces errors). No `gc.run` record is appended — every open would
    /// otherwise grow the log; the record is the explicit run's fact.
    pub(crate) fn run_auto_gc(&mut self, config: &GcConfig) {
        let collector = SessionCollector {
            log_path: self.log_path.clone(),
            live_roots: self.gc_live_roots(),
        };
        let pins = &self.gc_pins;
        let _ = kanbei_gc::GcRun::execute(
            &mut self.store,
            &collector,
            &|digest| pins.lock().expect("gc pins lock poisoned").contains(digest),
            config,
        );
    }

    /// Writer pin: marks `digest` as in-flight referenced so GC never
    /// quarantines or sweeps it. [`Session::commit`] registers every object
    /// it installs before install and unregisters after the frame append;
    /// external writers installing outside commit (then referencing through
    /// a later commit) should pin the same way.
    pub fn gc_pin(&self, digest: Digest) {
        self.gc_pins
            .lock()
            .expect("gc pins lock poisoned")
            .insert(digest);
    }

    /// Removes a writer pin (see [`Session::gc_pin`]).
    pub fn gc_unpin(&self, digest: Digest) {
        self.gc_pins
            .lock()
            .expect("gc pins lock poisoned")
            .remove(&digest);
    }
}

/// The session-side collector: the full canonical reference set — every log
/// envelope's refs/snapshot/payload digests, every snapshot manifest's
/// closure, and the live roots.
struct SessionCollector {
    log_path: PathBuf,
    live_roots: Vec<Digest>,
}

impl Collector for SessionCollector {
    fn collect(
        &self,
        store: &kanbei_objects::ObjectStore,
        out: &mut ReferenceSet,
    ) -> Result<(), kanbei_gc::GcError> {
        let mut manifests: Vec<Digest> = Vec::new();
        let log_path = self.log_path.clone();
        kanbei_log::for_each_frame(&log_path, |info| {
            for line in &info.events {
                let Ok(env) = Envelope::from_line(line) else {
                    continue;
                };
                out.extend(env.refs.iter().copied());
                if let Some(snapshot) = env.snapshot {
                    out.insert(snapshot);
                    manifests.push(snapshot);
                }
                collect_payload_digests(&env.payload, out, &mut manifests);
            }
        })
        .map_err(|e| kanbei_gc::GcError::Log {
            path: self.log_path.clone(),
            source: e,
        })?;
        out.extend(self.live_roots.iter().copied());
        for root in &self.live_roots {
            manifests.push(*root);
        }
        // Expand every snapshot manifest's closure. Engine/toolchain digests
        // are kernel-embedded identity pins, never store objects — excluded
        // exactly like the bundle export treats them.
        for digest in manifests {
            let Ok(bytes) = store.get(&digest) else {
                continue;
            };
            let Ok(manifest) =
                serde_json::from_slice::<kanbei_snapshot::ExecutionManifest>(&bytes)
            else {
                continue;
            };
            let mut closure = kanbei_snapshot::manifest_closure(&manifest);
            for pin in [manifest.engine_digest, manifest.toolchain_digest]
                .into_iter()
                .flatten()
            {
                closure.remove(&pin);
            }
            out.extend(closure);
        }
        Ok(())
    }
}

/// Payload digests under the canonical digest-bearing keys: `$object`
/// promotion markers, checkpoint pins (snapshot/memory roots/composition),
/// and compaction summaries. Keyed, never a recursive walk — free-form
/// content strings that happen to parse as digests must not pin objects.
fn collect_payload_digests(
    payload: &serde_json::Value,
    out: &mut ReferenceSet,
    manifests: &mut Vec<Digest>,
) {
    for key in [
        "$object",
        "snapshot",
        "memory_root",
        "project_memory_root",
        "composition",
        "summary_digest",
    ] {
        let Some(s) = payload.get(key).and_then(|v| v.as_str()) else {
            continue;
        };
        let Ok(digest) = s.parse::<Digest>() else {
            continue;
        };
        out.insert(digest);
        if key == "snapshot" {
            manifests.push(digest);
        }
    }
}

/// Registers every digest commit installs with the writer-pin set and
/// unregisters them on drop — including every error return path — so a
/// failed commit never leaks pins.
pub(crate) struct GcPinGuard<'a> {
    pins: &'a Mutex<HashSet<Digest>>,
    added: Vec<Digest>,
}

impl<'a> GcPinGuard<'a> {
    pub(crate) fn new(pins: &'a Mutex<HashSet<Digest>>) -> Self {
        Self {
            pins,
            added: Vec::new(),
        }
    }

    /// Registers `digest` before its install (idempotent).
    pub(crate) fn pin(&mut self, digest: Digest) {
        self.pins
            .lock()
            .expect("gc pins lock poisoned")
            .insert(digest);
        self.added.push(digest);
    }
}

impl Drop for GcPinGuard<'_> {
    fn drop(&mut self) {
        let mut pins = self.pins.lock().expect("gc pins lock poisoned");
        for digest in &self.added {
            pins.remove(digest);
        }
    }
}
