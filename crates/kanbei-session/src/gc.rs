//! M8 wave 2: automatic canonical-object GC — the session-side root capture,
//! writer pins, and the session/memory entry points over the kanbei-gc
//! engine (architecture.md: "A later GC requires coordinated root capture,
//! writer pins, quarantine, and a grace period from last reference").

use std::path::PathBuf;

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
        // Every live config-layer package (the ordered stack, F5), so a
        // higher layer's package is rooted as well as the top `config_digest`.
        roots.extend(self.config_layers.iter().map(|l| l.package));
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
            roots.extend(record.config_choice.layers.iter().copied());
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
                // Workspace snapshot/restore events pin the manifest alone —
                // its file blobs are referenced only inside the manifest
                // object, so the collector must walk that closure or every
                // opted-in GC run quarantines the snapshot's contents.
                if matches!(env.kind.as_str(), "workspace_snapshot" | "workspace_restore")
                    && let Some(d) = env
                        .payload
                        .get("manifest")
                        .and_then(|v| v.as_str())
                        .and_then(|s| s.parse::<Digest>().ok())
                    && let Ok(bytes) = store.get(&d)
                    && let Ok(m) = serde_json::from_slice::<kanbei_workspace::Manifest>(&bytes)
                {
                    for entry in &m.entries {
                        if let kanbei_workspace::Entry::File { digest, .. } = entry {
                            out.insert(*digest);
                        }
                    }
                }
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
            // fail-closed on future schemas only; a corrupt/missing
            // manifest object cannot classify its closure, so it stays
            // referenced (below)
            let Ok(manifest) = kanbei_snapshot::ExecutionManifest::from_bytes(&bytes) else {
                continue;
            };
            let closure = kanbei_snapshot::store_closure(&manifest);
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{SessionConfig, builtin_config_manifest};
    use kanbei_capabilities::TrustClass;
    use kanbei_core::id::Id128;
    use kanbei_modules::{ModuleOrigin, PackageManifest};
    use kanbei_services::ScopePath;
    use kanbei_vm::{GuestError, Vm, VmConfig};
    use std::path::PathBuf;

    fn no_epoch() -> VmConfig {
        VmConfig {
            fuel_per_call: u64::MAX,
            epoch_deadline: u64::MAX,
            ..Default::default()
        }
    }

    fn root() -> ScopePath {
        ScopePath(vec![])
    }

    fn settings_manifest(
        id: Id128,
        origin: ModuleOrigin,
        trust_class: TrustClass,
        payload: &str,
    ) -> PackageManifest {
        PackageManifest {
            schema: kanbei_modules::PACKAGE_SCHEMA,
            module_id: id,
            origin,
            trust_class,
            scope: root(),
            deps: vec![],
            capabilities: vec![],
            source: format!(
                "function kb_on_activate(ctx) ctx.contribution_publish('{payload}') end\nfunction kb_hot(x) return x end"
            ),
            state_schema: None,
            state_key: None,
        }
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "kb-session-gc-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// F7(d)/A1: after a safe-mode drop the session's live GC roots name only
    /// the surviving built-in config package — the dropped layer's package is
    /// no longer rooted by the stale `config_digest`.
    #[test]
    fn safe_mode_gc_roots_exclude_dropped_layer() {
        match Vm::load(no_epoch()) {
            Ok(_) => {}
            Err(GuestError::NotBuilt) => {
                panic!("guest wasm not built: run `cargo xtask build-guest` from the workspace root")
            }
            Err(e) => panic!("Vm::load failed: {e}"),
        }
        let dir = temp_dir("safe-roots");
        let builtin = builtin_config_manifest();
        let builtin_digest = Digest::new(&serde_json::to_vec(&builtin).unwrap());
        let user = settings_manifest(
            Id128::generate(),
            ModuleOrigin::UserConfig,
            TrustClass::User,
            r#"{"kind":"settings","provider":{"model":"user-model"}}"#,
        );
        let user_digest = Digest::new(&serde_json::to_vec(&user).unwrap());
        let bad_project = PackageManifest {
            schema: kanbei_modules::PACKAGE_SCHEMA,
            module_id: Id128::generate(),
            origin: ModuleOrigin::WorkspaceConfig,
            trust_class: TrustClass::Workspace,
            scope: root(),
            deps: vec![],
            capabilities: vec![],
            source: "local x = = 1".to_string(),
            state_schema: None,
            state_key: None,
        };
        let session = Session::open(SessionConfig {
            dir: dir.clone(),
            engine: Some(no_epoch()),
            config_layers: vec![builtin, user, bad_project],
            ..Default::default()
        })
        .unwrap();
        let roots = session.gc_live_roots();
        assert!(
            roots.contains(&builtin_digest),
            "the surviving built-in config stays a live root"
        );
        assert!(
            !roots.contains(&user_digest),
            "the dropped user layer's package is not a live root"
        );
        session.close().unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}

