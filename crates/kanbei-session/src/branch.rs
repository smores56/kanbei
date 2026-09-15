//! Branching: checkpoints, branch transitions, config choice, path filtering, and memory-follow policy.

use crate::{BranchRecord, Session, CHECKPOINT_LABEL_MAX, CheckpointFacts, CheckpointRef, ConfigChoiceRecord, FaultPoint, NewEvent, PinnedRoots, QuiesceRecord, QuiescedIntent, SessionError};
use kanbei_core::digest::Digest;
use kanbei_core::envelope::Envelope;
use kanbei_core::id::BranchId;
use kanbei_snapshot::ExecutionManifest;
use serde_json::json;

impl Session {
    // ---------- M6 historical correction (branching) ----------

    /// Commit one canonical `checkpoint_created` event (M6): a record event
    /// freezing the current frontier — its own seq — with the post-event
    /// manifest digest, the pinned memory roots, the composition digest, and
    /// the current branch. The manifest is built with the same
    /// [`Session::build_manifest`] helper commit step 5 uses, so its digest
    /// (computed before the commit) is byte-exact with the receipt's
    /// post_snapshot.
    pub fn create_checkpoint(&mut self, label: Option<String>) -> Result<CheckpointRef, SessionError> {
        if label.as_ref().is_some_and(|l| l.chars().count() > CHECKPOINT_LABEL_MAX) {
            return Err(SessionError::InvalidInput(format!(
                "checkpoint label exceeds {CHECKPOINT_LABEL_MAX} characters"
            )));
        }
        let seq = self.next_seq;
        let state_head = Some(self.composition.current().digest);
        let manifest = self.build_manifest(state_head, &[1]);
        let snapshot = Digest::new(&manifest.to_bytes());
        // The manifest pins memory roots whose objects live in the memory
        // stores; install them into the session store so the checkpoint's
        // snapshot closure is verifiable from the session store alone (the
        // checkpoint event's refs then cover its pinned roots, R-10).
        let mut objects: Vec<Vec<u8>> = Vec::new();
        if let Some(root) = self.memory_lifetime.head() {
            let bytes = self
                .memory_lifetime
                .store()
                .get(&root)
                .map_err(|e| SessionError::Snapshot(format!("checkpoint lifetime root {root} unreadable: {e}")))?;
            objects.push(bytes);
        }
        if let Some(root) = self.memory_project.as_ref().and_then(|a| a.head()) {
            let bytes = self
                .memory_project
                .as_ref()
                .expect("project head implies actor")
                .store()
                .get(&root)
                .map_err(|e| SessionError::Snapshot(format!("checkpoint project root {root} unreadable: {e}")))?;
            objects.push(bytes);
        }
        self.fault(FaultPoint::BeforeCheckpointCommit);
        let receipt = self.commit(
            vec![NewEvent {
                kind: "checkpoint_created".into(),
                payload_schema: 1,
                payload: json!({
                    "label": label,
                    "frontier_seq": seq,
                    "snapshot": snapshot.to_string(),
                    "memory_root": self.memory_lifetime.head().map(|d| d.to_string()),
                    "project_memory_root": self
                        .memory_project
                        .as_ref()
                        .and_then(|a| a.head())
                        .map(|d| d.to_string()),
                    "composition": self.composition.current().digest.to_string(),
                    "branch": self.branch.to_string(),
                }),
                objects,
                refs: Vec::new(),
            }],
            state_head,
        )?;
        self.fault(FaultPoint::AfterCheckpointCommit);
        debug_assert_eq!(
            receipt.post_snapshot,
            Some(snapshot),
            "checkpoint manifest digest must match the pinned post-snapshot"
        );
        #[cfg(feature = "otel")]
        self.telemetry_checkpoint(receipt.last_seq, self.memory_lifetime.head());
        Ok(CheckpointRef {
            session_id: self.session_id,
            seq: receipt.last_seq,
        })
    }

    /// Branch off a committed checkpoint (M6): validate the checkpoint
    /// (session, committed seq, event kind, snapshot closure), quiesce
    /// (cancel the active run as `Failed(Quiesced)`; list pending and
    /// abandoned-tail intents), then commit one canonical
    /// `branch_transition` event and switch the session to the new branch.
    /// History is never rewritten — the transition is appended and the new
    /// path is derived by the path filter.
    pub fn continue_from(&mut self, checkpoint: &CheckpointRef) -> Result<BranchRecord, SessionError> {
        let facts = self.validate_checkpoint(checkpoint)?;
        let snapshot = facts.snapshot;
        let memory_root = facts.memory_root;
        let project_memory_root = facts.project_memory_root;
        let follow = facts.follow;
        let env = facts.env;
        let manifest = facts.manifest;

        // Quiesce BEFORE the transition commits: an active run is cancelled
        // (its `run_outcome Failed(Quiesced)` records the termination), then
        // the pending intents (any committed intent-kind event without its
        // outcome-kind event) become the cancelled list and tail intents with
        // an interrupted/ambiguous classification become the ambiguous list.
        // No `intent_classified` facts are committed here — the transition
        // event's listing is the record; a crash before the transition leaves
        // open()'s classification to handle it.
        if let Some(run_id) = self.scheduler.active_run() {
            let usage = self.scheduler.current_usage(run_id);
            let (record, _) = self.scheduler.record_outcome(
                run_id,
                kanbei_scheduler::TerminalOutcome::Failed(
                    kanbei_scheduler::FailureKind::Quiesced,
                ),
                usage,
                &[],
            )?;
            self.commit(
                vec![NewEvent {
                    kind: "run_outcome".into(),
                    payload_schema: 1,
                    payload: serde_json::to_value(&record).map_err(|e| {
                        SessionError::InvalidInput(format!("run outcome payload: {e}"))
                    })?,
                    objects: Vec::new(),
                    refs: Vec::new(),
                }],
                None,
            )?;
            #[cfg(feature = "otel")]
            self.telemetry_close_run(
                kanbei_scheduler::TerminalOutcome::Failed(kanbei_scheduler::FailureKind::Quiesced),
                usage,
            );
        }
        let cancelled: Vec<QuiescedIntent> = self
            .scan_pending_intents()?
            .into_iter()
            .map(|i| QuiescedIntent {
                seq: i.seq,
                kind: i.kind,
                id: i.id,
            })
            .collect();
        let transition_seq = self.next_seq;
        let ambiguous: Vec<QuiescedIntent> = self
            .scan_classified_intents()?
            .into_iter()
            .filter(|i| i.seq > checkpoint.seq && i.seq < transition_seq)
            .collect();
        let quiesce = QuiesceRecord { cancelled, ambiguous };

        let new_branch = BranchId::generate();
        // The config choice at the branch point: the live config manifest
        // digest (the package digest `activate_config` retained — the
        // canonical content digest), the checkpoint manifest's
        // `provider_config` pin (the historical choice), and the live epoch
        // composition. Config restoration is out of scope — the record is
        // the deliverable.
        let config_choice = ConfigChoiceRecord {
            mode: "Current".into(),
            current: self.config_digest,
            historical: manifest.provider_config,
            composition: Some(self.composition.current().digest),
            // F5: the full ordered stack, so a later restore replays every
            // layer (built-in defaults + user + project), not just the top.
            layers: self.config_layer_digests(),
        };
        self.fault(FaultPoint::BeforeBranchTransition);
        let receipt = self.commit(
            vec![NewEvent {
                kind: "branch_transition".into(),
                payload_schema: 1,
                payload: json!({
                    "branch": new_branch.to_string(),
                    "from_branch": self.branch.to_string(),
                    "frontier_seq": checkpoint.seq,
                    "checkpoint_event": env.evt,
                    "checkpoint_snapshot": snapshot.to_string(),
                    "follow": serde_json::to_value(&follow)
                        .expect("follow serialization cannot fail"),
                    "config_choice": serde_json::to_value(&config_choice)
                        .expect("config choice serialization cannot fail"),
                    "quiesce": serde_json::to_value(&quiesce)
                        .expect("quiesce serialization cannot fail"),
                    "memory_root": memory_root.map(|d| d.to_string()),
                    "project_memory_root": project_memory_root.map(|d| d.to_string()),
                }),
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            Some(self.composition.current().digest),
        )?;
        self.fault(FaultPoint::AfterBranchTransition);
        let record = BranchRecord {
            id: new_branch,
            from: Some(self.branch),
            frontier_seq: checkpoint.seq,
            transition_seq: receipt.last_seq,
            follow,
            config_choice,
            quiesce,
        };
        self.branch = new_branch;
        self.branch_records.push(record.clone());
        // Wave 2 consumes the pinned roots; None when the checkpoint pinned
        // no lifetime root.
        self.pinned_roots = memory_root.map(|lifetime| PinnedRoots {
            lifetime,
            project: project_memory_root,
        });
        #[cfg(feature = "otel")]
        self.telemetry_continue_from(record.transition_seq, &record.id);
        Ok(record)
    }

    /// Validates a committed `checkpoint_created` event (M6): the session
    /// match, the committed seq, the event kind + frontier, the snapshot
    /// manifest readability, the full closure walk, and the pinned memory
    /// roots' membership in the memory actors' histories. Shared by
    /// `continue_from` (which then quiesces + transitions) and `fork` (which
    /// then seeds a new session) — both treat an invalid checkpoint as an
    /// explicit error with no side effects.
    pub(crate) fn validate_checkpoint(&self, checkpoint: &CheckpointRef) -> Result<CheckpointFacts, SessionError> {
        if checkpoint.session_id != self.session_id {
            return Err(SessionError::InvalidInput(
                "checkpoint belongs to a different session".into(),
            ));
        }
        if checkpoint.seq == 0 || checkpoint.seq >= self.next_seq {
            return Err(SessionError::InvalidInput(format!(
                "checkpoint seq {} is not a committed event",
                checkpoint.seq
            )));
        }
        let env = self.envelope_at(checkpoint.seq)?;
        if env.kind != "checkpoint_created"
            || env.payload.get("frontier_seq").and_then(|f| f.as_u64()) != Some(checkpoint.seq)
        {
            return Err(SessionError::InvalidInput(format!(
                "event at seq {} is not a checkpoint",
                checkpoint.seq
            )));
        }
        let snapshot: Digest = env
            .payload
            .get("snapshot")
            .and_then(|s| s.as_str())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| {
                SessionError::InvalidInput(format!(
                    "checkpoint at seq {} pins no snapshot",
                    checkpoint.seq
                ))
            })?;
        let bytes = self
            .store
            .get(&snapshot)
            .map_err(|e| SessionError::Snapshot(format!("checkpoint snapshot {snapshot} unreadable: {e}")))?;
        let manifest: ExecutionManifest = serde_json::from_slice(&bytes).map_err(|e| {
            SessionError::Snapshot(format!("checkpoint snapshot {snapshot} is not a manifest: {e}"))
        })?;
        // Full closure walk (M6 wave 2): every digest field the manifest
        // pins must resolve in the session store — modules' packages,
        // composition, memory roots, and the tool-registry/provider-config
        // objects (all installed before the pin). The engine/toolchain
        // digests are kernel-embedded build-time artifacts (the guest wasm
        // is a kanbei-vm `include_bytes!` constant that never enters the
        // object store), so they are the only digest fields excepted from
        // the store verification.
        let closure = kanbei_snapshot::store_closure(&manifest);
        kanbei_snapshot::verify_closure(&self.store, &closure)
            .map_err(|e| SessionError::Snapshot(format!("checkpoint snapshot closure failed: {e}")))?;
        let memory_root: Option<Digest> = env
            .payload
            .get("memory_root")
            .and_then(|r| r.as_str())
            .and_then(|r| r.parse().ok());
        let project_memory_root: Option<Digest> = env
            .payload
            .get("project_memory_root")
            .and_then(|r| r.as_str())
            .and_then(|r| r.parse().ok());

        // The follow policy: the checkpoint's pinned roots must be roots the
        // memory actors know — a corrupted checkpoint event is rejected
        // explicitly, with no branch/fork. A checkpoint without a pinned
        // lifetime root cannot pin (the policy's lifetime_root is required)
        // → FollowHead.
        let follow = match memory_root {
            Some(lifetime_root) => {
                if !self.memory_lifetime.contains_root(&lifetime_root) {
                    return Err(SessionError::InvalidInput(format!(
                        "checkpoint pins memory root {lifetime_root} unknown to the lifetime actor"
                    )));
                }
                if let Some(project_root) = project_memory_root
                    && !self
                        .memory_project
                        .as_ref()
                        .is_some_and(|a| a.contains_root(&project_root))
                {
                    return Err(SessionError::InvalidInput(format!(
                        "checkpoint pins project memory root {project_root} unknown to the project actor"
                    )));
                }
                kanbei_memory::MemoryFollowPolicy::PinnedAt {
                    lifetime_root,
                    project_root: project_memory_root,
                }
            }
            None => kanbei_memory::MemoryFollowPolicy::FollowHead,
        };
        Ok(CheckpointFacts {
            env,
            snapshot,
            manifest,
            memory_root,
            project_memory_root,
            follow,
        })
    }

    /// The ORDERED (LOW→HIGH) config-layer package digests active at `at_seq`
    /// (F5): replaying `composition_changed` `initiator: "config"` events —
    /// `delta.added` upserts by module id (a replacement updates in place),
    /// `delta.removed` drops the named module — and re-baselining at a
    /// `branch_transition`'s recorded `config_choice`. None for sessions that
    /// never activated a config layer.
    pub(crate) fn config_choice_at(&self, at_seq: u64) -> Result<Option<Vec<Digest>>, SessionError> {
        let log_path = self.log_path.clone();
        // (module-id key, package digest), in activation order.
        let mut layers: Vec<(String, Digest)> = Vec::new();
        kanbei_log::for_each_frame(&log_path, |info| {
            for line in &info.events {
                let Ok(env) = Envelope::from_line(line) else {
                    continue;
                };
                if env.seq > at_seq {
                    continue;
                }
                match env.kind.as_str() {
                    // A branch point re-baselines the stack it recorded (the
                    // ordered `layers` when present, else the top `current`).
                    "branch_transition" => {
                        let Some(choice) = env.payload.get("config_choice") else {
                            continue;
                        };
                        if let Some(arr) = choice.get("layers").and_then(|l| l.as_array()) {
                            layers = arr
                                .iter()
                                .filter_map(|v| v.as_str())
                                .filter_map(|s| s.parse::<Digest>().ok())
                                .map(|d| (d.to_string(), d))
                                .collect();
                        } else if let Some(current) = choice
                            .get("current")
                            .and_then(|c| c.as_str())
                            .and_then(|c| c.parse::<Digest>().ok())
                        {
                            layers = vec![(current.to_string(), current)];
                        }
                    }
                    // Config-layer activation/replacement/removal deltas.
                    "composition_changed" => {
                        if env.payload.get("initiator").and_then(|i| i.as_str()) != Some("config") {
                            continue;
                        }
                        let Some(delta) = env.payload.get("delta") else {
                            continue;
                        };
                        if let Some(removed) = delta.get("removed").and_then(|r| r.as_array()) {
                            for r in removed {
                                let Some(id) = r.get("module_id").and_then(|m| m.as_str()) else {
                                    continue;
                                };
                                layers.retain(|(k, _)| k != id);
                            }
                        }
                        if let Some(added) = delta.get("added").and_then(|a| a.as_array()) {
                            for a in added {
                                let Some(pkg) = a
                                    .get("package")
                                    .and_then(|p| p.as_str())
                                    .and_then(|p| p.parse::<Digest>().ok())
                                else {
                                    continue;
                                };
                                let key = a
                                    .get("module_id")
                                    .and_then(|m| m.as_str())
                                    .map(str::to_string)
                                    .unwrap_or_else(|| pkg.to_string());
                                match layers.iter_mut().find(|(k, _)| *k == key) {
                                    Some(existing) => existing.1 = pkg,
                                    None => layers.push((key, pkg)),
                                }
                            }
                        }
                    }
                    _ => {}
                }
            }
        })?;
        if layers.is_empty() {
            Ok(None)
        } else {
            Ok(Some(layers.into_iter().map(|(_, d)| d).collect()))
        }
    }

    /// Switch the memory-follow policy (M6 wave 2): `FollowHead` releases the
    /// pinned roots (the projection resolves against the live actor heads
    /// again); `PinnedAt` pins the projection to the given roots — which must
    /// be roots the memory actors committed ([`MemoryRootActor::contains_root`]),
    /// else `InvalidInput` and no event. Commits one canonical
    /// `memory_follow_changed` record event (schema 1, `state_head` None).
    pub fn memory_follow(&mut self, policy: kanbei_memory::MemoryFollowPolicy) -> Result<(), SessionError> {
        match &policy {
            kanbei_memory::MemoryFollowPolicy::FollowHead => {}
            kanbei_memory::MemoryFollowPolicy::PinnedAt {
                lifetime_root,
                project_root,
            } => {
                if !self.memory_lifetime.contains_root(lifetime_root) {
                    return Err(SessionError::InvalidInput(format!(
                        "pinned lifetime root {lifetime_root} is not a committed root"
                    )));
                }
                if let Some(project_root) = project_root
                    && !self
                        .memory_project
                        .as_ref()
                        .is_some_and(|a| a.contains_root(project_root))
                {
                    return Err(SessionError::InvalidInput(format!(
                        "pinned project root {project_root} is not a committed root"
                    )));
                }
            }
        }
        let at = self.next_seq;
        self.commit(
            vec![NewEvent {
                kind: "memory_follow_changed".into(),
                payload_schema: 1,
                payload: json!({
                    "policy": serde_json::to_value(&policy)
                        .expect("follow policy serialization cannot fail"),
                    "at": at,
                }),
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            None,
        )?;
        self.pinned_roots = match policy {
            kanbei_memory::MemoryFollowPolicy::FollowHead => None,
            kanbei_memory::MemoryFollowPolicy::PinnedAt {
                lifetime_root,
                project_root,
            } => Some(PinnedRoots {
                lifetime: lifetime_root,
                project: project_root,
            }),
        };
        Ok(())
    }

    /// Whether `seq` is on the current branch's path: false exactly for the
    /// abandoned tails `(frontier_seq, transition_seq]` of every committed
    /// branch record. The checkpoint event at the frontier stays on-path; the
    /// `branch_transition` event itself is excluded from the new path.
    pub fn on_path(&self, seq: u64) -> bool {
        !self
            .branch_records
            .iter()
            .any(|r| r.frontier_seq < seq && seq <= r.transition_seq)
    }

    /// The current branch's on-path ranges (inclusive on both ends):
    /// `[1..=first.frontier]`, then per record
    /// `[records[i].transition + 1 ..= records[i+1].frontier]`, and
    /// `[last.transition + 1 ..= u64::MAX]` for the last — the transition
    /// event itself is off-path. No records → the whole seq space.
    pub fn path_ranges(&self) -> Vec<(u64, u64)> {
        if self.branch_records.is_empty() {
            return vec![(1, u64::MAX)];
        }
        let mut ranges = vec![(1, self.branch_records[0].frontier_seq)];
        for pair in self.branch_records.windows(2) {
            ranges.push((pair[0].transition_seq + 1, pair[1].frontier_seq));
        }
        let last = self.branch_records.last().expect("non-empty records");
        ranges.push((last.transition_seq + 1, u64::MAX));
        ranges
    }
}
