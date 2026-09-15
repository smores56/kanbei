//! Session switching: independent forks, adoption of fork outcomes, verbatim import, and the fork helpers.

use crate::recovery::{recover_bound_project, recover_session_id};
use crate::{AdoptReceipt, CheckpointRef, ForkOptions, ForkReceipt, NewEvent, PinnedRoots, QuiesceRecord, QuiescedIntent, Session, SessionConfig, SessionError};
use std::path::{Path, PathBuf};
use std::io;
use kanbei_core::digest::Digest;
use kanbei_core::envelope::Envelope;
use kanbei_core::id::Id128;
use kanbei_modules::PackageManifest;
use kanbei_objects::ObjectError;
use kanbei_snapshot::ExecutionManifest;
use serde_json::json;

impl Session {
    /// Forks an independent session from a committed checkpoint (M9 wave 5a,
    /// R-24/D-08): the new session is created from the checkpoint's snapshot
    /// closure — a fresh SessionId, an explicit `forked` source-reference
    /// fact, and a fork-floor broker (read-only capabilities + approval-gated
    /// `memory.propose`, the attenuated grant recorded in the fact). Unlike
    /// `continue_from` this never touches the source session: it is a pure
    /// snapshot read — no quiesce, no events on the source log. Module state
    /// heads are NOT carried over (the state store is opaque and bound to the
    /// source's live module-manager generation tokens).
    ///
    /// The checkpoint is validated exactly like `continue_from` via
    /// [`Session::validate_checkpoint`]. The snapshot closure objects
    /// (manifest + memory roots + composition + packages; engine/toolchain
    /// digests are kernel-embedded pins, excluded) are copied into
    /// `<target>/objects/`, plus every `workspace_snapshot` manifest + blob
    /// at or before the checkpoint (event-referenced objects, outside the
    /// manifest closure — the manifests join the `forked` fact's refs so
    /// they stay GC-rooted). Memory is seeded by copying the source's
    /// `<memory_root>/lifetime/` (and `projects/` + `projects.jsonl` when the
    /// source has a project) into `<target>/memory/`, then truncating each
    /// copied transition log after the frame committing the checkpoint-pinned
    /// root — the actor replay yields exactly the pinned root as head
    /// (`head.json` is repaired from the log at open; `projection.sqlite` is
    /// disposable and rebuilt at open). The fork's config choice is the last
    /// `branch_transition` `config_choice.current` or `composition_changed`
    /// package digest at or before the checkpoint seq; that package manifest
    /// is activated at open, and a choice whose package is absent from the
    /// source store (a superseded config on a multi-branch history) yields a
    /// storage-only fork. The new session then commits one canonical `forked`
    /// event (schema 1): `{source_session, checkpoint_seq,
    /// checkpoint_snapshot, follow, grants, config, frontier_seq}` with
    /// refs = [snapshot, memory roots, config package, workspace manifests] —
    /// the fork-floor canonical fact (architecture.md R-24/D-08) and the
    /// explicit source reference. Automatic GC is forced off at the fork's
    /// open (the seeded objects are not yet event-referenced; run the
    /// explicit `Session::run_gc` afterwards instead).
    ///
    /// The forked session's memory actors replay the seeded logs, so their
    /// heads ARE the pinned roots by construction (there is no actor-level
    /// set-head seam — the log replay is the authority); the fact records
    /// `PinnedAt` and `pinned_roots` is set on the new session, so the
    /// projection pins the checkpoint roots from the start.
    ///
    /// `target_dir` must be absent or empty; on any failure after creation
    /// the target dir is best-effort removed (an orphan can remain only when
    /// the removal itself fails — the caller may delete it).
    pub fn fork(
        &self,
        checkpoint: &CheckpointRef,
        options: ForkOptions,
    ) -> Result<ForkReceipt, SessionError> {
        let facts = self.validate_checkpoint(checkpoint)?;
        let target_dir = options.target_dir.clone();
        match std::fs::metadata(&target_dir) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Ok(m) if m.is_dir() => {
                let mut entries = std::fs::read_dir(&target_dir)?;
                if entries.next().is_some() {
                    return Err(SessionError::InvalidInput(format!(
                        "fork target dir {} is not empty (refusing to seed into an existing session dir)",
                        target_dir.display()
                    )));
                }
            }
            Ok(_) => {
                return Err(SessionError::InvalidInput(format!(
                    "fork target {} exists and is not a directory",
                    target_dir.display()
                )))
            }
            Err(e) => return Err(e.into()),
        }
        // Best-effort cleanup guard: any failure below removes the target dir
        // (it was absent or empty, so only fork's own writes live inside).
        struct ForkCleanup(PathBuf);
        impl Drop for ForkCleanup {
            fn drop(&mut self) {
                let _ = std::fs::remove_dir_all(&self.0);
            }
        }
        let cleanup = ForkCleanup(target_dir.clone());

        let fresh_id = options.session_id.unwrap_or_else(Id128::generate);
        let (broker, grant_digests) = fork_floor_broker(fresh_id)?;

        // The checkpoint closure: the snapshot manifest, the memory roots,
        // composition, packages, and every other digest the manifest pins —
        // copied into the new store. Engine/toolchain digests are
        // kernel-embedded identity pins, never store objects (mirror of
        // continue_from).
        let objects_dir = target_dir.join("objects");
        std::fs::create_dir_all(&objects_dir)?;
        let mut closure = kanbei_snapshot::store_closure(&facts.manifest);
        // the snapshot object itself is the manifest bytes (not part of its
        // own closure)
        closure.insert(facts.snapshot);
        for d in closure {
            let bytes = self.store.get(&d).map_err(|e| {
                SessionError::Snapshot(format!("fork closure object {d} unreadable: {e}"))
            })?;
            std::fs::write(objects_dir.join(d.to_string()), bytes)?;
        }

        // Workspace snapshots are ordinary store objects referenced by
        // `workspace_snapshot` events — NOT by the execution manifest — so
        // they are outside the checkpoint closure. Copy every snapshot at or
        // before the checkpoint (manifest + blobs) so the fork can restore
        // the checkpoint's workspace state; the manifests join the `forked`
        // fact's refs, keeping them GC-rooted on the fork.
        let mut ws_manifests: Vec<Digest> = Vec::new();
        {
            let log_path = self.log_path.clone();
            kanbei_log::for_each_frame(&log_path, |info| {
                for line in &info.events {
                    let Ok(env) = Envelope::from_line(line) else {
                        continue;
                    };
                    if env.seq <= checkpoint.seq
                        && env.kind == "workspace_snapshot"
                        && let Some(m) = env
                            .payload
                            .get("manifest")
                            .and_then(|m| m.as_str())
                            .and_then(|m| m.parse::<Digest>().ok())
                    {
                        ws_manifests.push(m);
                    }
                }
            })?;
            for manifest in &ws_manifests {
                let bytes = self.store.get(manifest).map_err(|e| {
                    SessionError::Snapshot(format!(
                        "workspace snapshot manifest {manifest} unreadable: {e}"
                    ))
                })?;
                let parsed: kanbei_workspace::Manifest =
                    serde_json::from_slice(&bytes).map_err(|e| {
                        SessionError::Snapshot(format!(
                            "workspace snapshot manifest {manifest} is not a manifest: {e}"
                        ))
                    })?;
                std::fs::write(objects_dir.join(manifest.to_string()), bytes)?;
                for entry in &parsed.entries {
                    if let kanbei_workspace::Entry::File { digest, .. } = entry {
                        let blob = self.store.get(digest).map_err(|e| {
                            SessionError::Snapshot(format!(
                                "workspace snapshot blob {digest} unreadable: {e}"
                            ))
                        })?;
                        std::fs::write(objects_dir.join(digest.to_string()), blob)?;
                    }
                }
            }
        }

        // Memory seeding: copy the source's scope dirs into `<target>/memory`
        // and truncate each copied transition log after the checkpoint root's
        // committing frame. A checkpoint without a pinned root means the
        // actor had no head at the fork point — nothing is copied and the
        // new actor opens empty.
        let source_memory_root = self
            .cfg
            .memory_root
            .clone()
            .unwrap_or_else(|| self.cfg.dir.join("memory"));
        let target_memory_root = target_dir.join("memory");
        if let Some(lifetime_root) = facts.memory_root {
            let scope_dir = kanbei_memory::MemoryScope::Lifetime.dir_name();
            copy_dir_all(
                &source_memory_root.join(&scope_dir),
                &target_memory_root.join(&scope_dir),
            )?;
            truncate_log_at(
                &target_memory_root.join(&scope_dir).join("transitions.jsonl.zst"),
                lifetime_root,
            )?;
        }
        let source_project = self.cfg.project;
        if let Some(project_id) = source_project {
            std::fs::create_dir_all(&target_memory_root)?;
            let registry = source_memory_root.join("projects.jsonl");
            if registry.exists() {
                std::fs::copy(&registry, target_memory_root.join("projects.jsonl"))?;
            }
            if let Some(project_root) = facts.project_memory_root {
                let scope_dir = kanbei_memory::MemoryScope::Project(project_id).dir_name();
                copy_dir_all(
                    &source_memory_root.join(&scope_dir),
                    &target_memory_root.join(&scope_dir),
                )?;
                truncate_log_at(
                    &target_memory_root.join(&scope_dir).join("transitions.jsonl.zst"),
                    project_root,
                )?;
            }
        }

        // The config choice at the checkpoint: the FULL ordered layer stack is
        // activated at open (F5) — not just the top layer — so the fork's
        // merged settings/composition reflect built-in defaults + every user/
        // project layer. A digest missing from the source store is skipped
        // (best-effort); the top survives as the `config` payload/ref meaning.
        let layer_digests: Vec<Digest> =
            self.config_choice_at(checkpoint.seq)?.unwrap_or_default();
        let config_layers: Vec<PackageManifest> = layer_digests
            .iter()
            .filter_map(|digest| {
                self.store
                    .get(digest)
                    .ok()
                    .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            })
            .collect();
        let loaded_digests: Vec<Digest> = config_layers
            .iter()
            .map(|m| {
                Digest::new(
                    &serde_json::to_vec(m).expect("package manifest serialization cannot fail"),
                )
            })
            .collect();
        let config_digest = loaded_digests.last().copied();

        // Open the forked session: the overridden lane fields (dir, identity,
        // policy, broker, memory root, config layers, project) beat anything
        // the caller set in `options.config`. GC is forced off: the seeded
        // objects are not yet event-referenced at open, so the automatic
        // quarantine pass would move them all (the caller can run the
        // explicit `Session::run_gc` after the fork, when the `forked` fact
        // roots them).
        let mut target_cfg = options.config;
        target_cfg.dir = target_dir.clone();
        target_cfg.session_id = Some(fresh_id);
        target_cfg.policy = options.policy;
        target_cfg.broker = broker;
        target_cfg.memory_root = None;
        target_cfg.config_layers = config_layers;
        target_cfg.gc = None;
        if source_project.is_some() {
            target_cfg.project = source_project;
        }
        let mut fork = Session::open(target_cfg)?;

        // The canonical fork-floor fact: the explicit source reference and
        // the attenuated grant record. All refs were copied into the new
        // store above (R-10); the workspace manifests join so the inherited
        // workspace snapshots stay GC-rooted.
        let mut refs = vec![facts.snapshot];
        if let Some(lifetime_root) = facts.memory_root {
            refs.push(lifetime_root);
        }
        if let Some(project_root) = facts.project_memory_root {
            refs.push(project_root);
        }
        // every restored config-layer package joins the refs, so all of them
        // stay GC-rooted on the fork (F5).
        refs.extend(loaded_digests.iter().copied());
        refs.extend(ws_manifests.iter().copied());
        let commit_result = fork.commit(
            vec![NewEvent {
                kind: "forked".into(),
                payload_schema: 1,
                payload: json!({
                    "source_session": self.session_id.to_string(),
                    "checkpoint_seq": checkpoint.seq,
                    "checkpoint_snapshot": facts.snapshot.to_string(),
                    "follow": serde_json::to_value(&facts.follow)
                        .expect("follow serialization cannot fail"),
                    "grants": grant_digests
                        .iter()
                        .map(|d| d.to_string())
                        .collect::<Vec<_>>(),
                    "config": config_digest.map(|d| d.to_string()),
                    "frontier_seq": checkpoint.seq,
                }),
                objects: Vec::new(),
                refs,
            }],
            None,
        );
        if let Err(e) = commit_result {
            let _ = fork.close();
            return Err(e);
        }
        // The actors' heads are the pinned roots by construction (seeded log
        // replay); pin them for the projection like continue_from does.
        fork.pinned_roots = facts.memory_root.map(|lifetime| PinnedRoots {
            lifetime,
            project: facts.project_memory_root,
        });
        let branch = fork.branch;
        let session_id = fork.session_id;
        std::mem::forget(cleanup);
        Ok(ForkReceipt {
            session: fork,
            session_id,
            checkpoint_seq: checkpoint.seq,
            branch,
            follow: facts.follow,
        })
    }

    /// Adopts a fork's outcome as the active perpetual root (M9 wave 5b,
    /// architecture.md "adopt(fork) explicitly changes the active perpetual
    /// root after reconciling domain state"): the fork's HEAD snapshot
    /// manifest + full closure + memory roots are copied into THIS session's
    /// store, the active run is quiesced exactly like `continue_from`, and
    /// one canonical `fork_adopted` fact (schema 1) records the adoption.
    /// The fork session is never modified — adoption is a source-side
    /// decision (the caller decides what the fork's outcome means).
    ///
    /// Validation (all before any mutation of self; a failure commits
    /// nothing): the fork's log must carry a `forked` fact whose
    /// `source_session` is this session (else `InvalidInput` naming both
    /// ids), and the fork must have committed events past that fact — a head
    /// to adopt (else `InvalidInput` "fork has no outcome"). The fork's head
    /// snapshot (its `current_snapshot`, falling back to the last envelope's
    /// pre-event snapshot for a resumed fork) must parse as a manifest and
    /// every closure digest must resolve in the fork's session or memory
    /// stores (a post-fork memory root legitimately lives only in the memory
    /// actor's store).
    ///
    /// Reconciliation: the head manifest, the full closure, and the fork's
    /// memory roots (its lifetime/project actor heads) are installed into
    /// THIS store — every digest is resolved (hash-verified) in the fork
    /// first, so a missing object aborts with nothing installed yet; an
    /// install failure after resolution leaves only orphan objects (the
    /// commit semantics for the referencing `fork_adopted` fact — R-10
    /// tolerates orphans). The head manifest is installed first. The fact's
    /// refs = [fork_snapshot, fork memory roots].
    ///
    /// The committed fact mirrors `branch_transition`'s quiesce record: an
    /// active run is cancelled as `Failed(Quiesced)` (run_outcome committed
    /// first), pending intents become `quiesce.cancelled`, and
    /// interrupted/ambiguous classified intents in `(frontier, transition)`
    /// become `quiesce.ambiguous`. `frontier_seq` is the fork's origin
    /// checkpoint seq (its `forked` fact's `checkpoint_seq`) — the point the
    /// adopted path diverged. The commit is pure (`state_head` None — the
    /// fork's manifest is a copied, referenced artifact, not self's live
    /// manifest; self's `current_snapshot` is unchanged, mirroring the
    /// `forked` fact's pure commit). Post-commit self's `pinned_roots`
    /// mirrors the follow policy exactly like `continue_from` (the
    /// projection folds the pinned roots; the actors' heads are untouched —
    /// there is no actor set-head seam).
    pub fn adopt(
        &mut self,
        fork: &mut Session,
        label: Option<String>,
    ) -> Result<AdoptReceipt, SessionError> {
        // --- validate the fork: the canonical forked fact + a head to adopt
        let mut found: Option<(u64, serde_json::Value)> = None;
        {
            let fork_log = fork.log_path.clone();
            kanbei_log::for_each_frame(&fork_log, |info| {
                if found.is_some() {
                    return;
                }
                for line in &info.events {
                    let Ok(env) = Envelope::from_line(line) else {
                        continue;
                    };
                    if env.kind == "forked" {
                        found = Some((env.seq, env.payload.clone()));
                        return;
                    }
                }
            })?;
        }
        let (forked_seq, forked_payload) = found.ok_or_else(|| {
            SessionError::InvalidInput(format!(
                "fork session {} carries no `forked` fact — not a fork of this or any session",
                fork.session_id
            ))
        })?;
        let source_session: Id128 = forked_payload
            .get("source_session")
            .and_then(|s| s.as_str())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| {
                SessionError::InvalidInput(format!(
                    "fork session {} has a `forked` fact without a source_session",
                    fork.session_id
                ))
            })?;
        if source_session != self.session_id {
            return Err(SessionError::InvalidInput(format!(
                "fork session {} belongs to source session {}, not {}",
                fork.session_id, source_session, self.session_id
            )));
        }
        let frontier_seq = forked_payload
            .get("checkpoint_seq")
            .and_then(|c| c.as_u64())
            .ok_or_else(|| {
                SessionError::InvalidInput(format!(
                    "fork session {} has a `forked` fact without a checkpoint_seq",
                    fork.session_id
                ))
            })?;
        // the adopted head is the fork's last committed event — past the
        // forked fact itself (a fresh fork has no outcome to adopt)
        let head_seq = fork.next_seq() - 1;
        if head_seq <= forked_seq {
            return Err(SessionError::InvalidInput(format!(
                "fork {} has no outcome (head seq {head_seq} is the forked fact itself)",
                fork.session_id
            )));
        }

        // --- reconcile domain state: the fork's HEAD snapshot + closure
        // The head snapshot is the fork's current_snapshot (the pre-event
        // snapshot of its last envelope; advanced by a state-changing last
        // commit); a resumed fork loses the in-memory pin, so fall back to
        // the last envelope's snapshot field.
        let head_snapshot = fork
            .current_snapshot()
            .or_else(|| fork.envelope_at(head_seq).ok().and_then(|env| env.snapshot))
            .ok_or_else(|| {
                SessionError::InvalidInput(format!(
                    "fork {} head seq {head_seq} pins no snapshot manifest",
                    fork.session_id
                ))
            })?;
        let head_bytes = fork
            .store
            .get(&head_snapshot)
            .map_err(|e| SessionError::Snapshot(format!("fork head snapshot {head_snapshot} unreadable: {e}")))?;
        let manifest: ExecutionManifest = serde_json::from_slice(&head_bytes).map_err(|e| {
            SessionError::Snapshot(format!("fork head snapshot {head_snapshot} is not a manifest: {e}"))
        })?;
        // engine/toolchain digests are kernel-embedded identity pins, never
        // store objects (shared exclusion in kanbei-snapshot)
        let closure = kanbei_snapshot::store_closure(&manifest);
        // Resolve every closure digest in the fork's stores FIRST (get
        // hash-verifies): a missing object aborts with nothing installed.
        let mut to_install: Vec<Vec<u8>> = Vec::with_capacity(closure.len() + 1);
        to_install.push(head_bytes);
        for d in closure {
            to_install.push(resolve_fork_object(fork, &d)?);
        }
        // The fork's memory roots are its actor heads — the lifetime head,
        // plus the project head when the fork is project-bound. Root
        // manifests live in the fork's memory stores (the session store
        // carries them only as checkpoint event objects).
        let lifetime_root = fork.memory_lifetime().head();
        let project_root = fork.memory_project.as_ref().and_then(|a| a.head());
        let follow = match lifetime_root {
            Some(lifetime) => kanbei_memory::MemoryFollowPolicy::PinnedAt {
                lifetime_root: lifetime,
                project_root,
            },
            None => kanbei_memory::MemoryFollowPolicy::FollowHead,
        };
        if let Some(root) = lifetime_root {
            let bytes = fork
                .memory_lifetime
                .store()
                .get(&root)
                .map_err(|e| SessionError::Snapshot(format!("fork lifetime memory root {root} unreadable: {e}")))?;
            to_install.push(bytes);
        }
        if let Some(actor) = fork.memory_project.as_ref()
            && let Some(root) = actor.head()
        {
            let bytes = actor
                .store()
                .get(&root)
                .map_err(|e| SessionError::Snapshot(format!("fork project memory root {root} unreadable: {e}")))?;
            to_install.push(bytes);
        }
        // All resolved — install into self's store, the head manifest first.
        for bytes in to_install {
            self.store.install(&bytes)?;
        }

        // --- quiesce the active run EXACTLY like continue_from: the run is
        // cancelled (its run_outcome Failed(Quiesced) records the
        // termination), then pending intents become cancelled and
        // interrupted/ambiguous tail intents become ambiguous. No
        // intent_classified facts are committed here.
        if let Some(run_id) = self.scheduler.active_run() {
            let usage = self.scheduler.current_usage(run_id);
            let (record, _) = self.scheduler.record_outcome(
                run_id,
                kanbei_scheduler::TerminalOutcome::Failed(kanbei_scheduler::FailureKind::Quiesced),
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
            .filter(|i| i.seq > frontier_seq && i.seq < transition_seq)
            .collect();
        let quiesce = QuiesceRecord { cancelled, ambiguous };

        // --- the canonical adoption fact
        let mut refs = vec![head_snapshot];
        if let kanbei_memory::MemoryFollowPolicy::PinnedAt {
            lifetime_root,
            project_root,
        } = &follow
        {
            refs.push(*lifetime_root);
            if let Some(project_root) = project_root {
                refs.push(*project_root);
            }
        }
        self.commit(
            vec![NewEvent {
                kind: "fork_adopted".into(),
                payload_schema: 1,
                payload: json!({
                    "fork_session": fork.session_id.to_string(),
                    "fork_seq": head_seq,
                    "fork_snapshot": head_snapshot.to_string(),
                    "follow": serde_json::to_value(&follow)
                        .expect("follow serialization cannot fail"),
                    "label": label,
                    "quiesce": serde_json::to_value(&quiesce)
                        .expect("quiesce serialization cannot fail"),
                    "frontier_seq": frontier_seq,
                }),
                objects: Vec::new(),
                refs,
            }],
            None,
        )?;
        // Post-commit state: the projection pins the fork's roots exactly
        // like continue_from pins a checkpoint's roots.
        self.pinned_roots = match follow {
            kanbei_memory::MemoryFollowPolicy::FollowHead => None,
            kanbei_memory::MemoryFollowPolicy::PinnedAt {
                lifetime_root,
                project_root,
            } => Some(PinnedRoots {
                lifetime: lifetime_root,
                project: project_root,
            }),
        };
        Ok(AdoptReceipt {
            fork_session: fork.session_id,
            fork_seq: head_seq,
            follow,
        })
    }

    /// Imports a session directory verbatim (M9 wave 5b backup/restore):
    /// `<source>/log.zst` is byte-copied, `objects/` recursively, and
    /// `memory/` + `state/` when present (the memory projection.sqlite is
    /// disposable and rebuilt at open; `state/` heads are opaque bytes and
    /// copied as-is). The copied target is then opened and returned. The
    /// canonical facts — envelopes (event ids, seqs, payloads, refs),
    /// branch records, memory roots — are preserved by construction: they
    /// are the copied bytes.
    ///
    /// The session id is NOT part of the on-disk layout (open derives it
    /// from the config); import recovers it from the canonical identity
    /// markers the source left behind — a `memory_proposal` owner, a memory
    /// transition's `origin_session`, or the project registry's
    /// `created_session` (in that order). A source with none of these
    /// markers imports with a fresh id (the caller can pin the original by
    /// reopening with `SessionConfig::session_id`). A bound project is
    /// recovered from the log's `project_bound` fact so the project memory
    /// actor wires up like the source.
    ///
    /// Validation: the source must carry a readable `log.zst` (framing
    /// verified read-only — the source is never truncated; a torn tail is
    /// recovered on the copy at open), and the target must be absent or
    /// empty. Every copy error surfaces as a typed `SessionError` naming the
    /// path. On failure the target dir may hold partial copies (the caller
    /// may delete it; the source is untouched).
    pub fn import(source_dir: &Path, target_dir: &Path) -> Result<Session, SessionError> {
        // source validation — a readable log.zst (read-only framing scan;
        // recover/truncate happens on the copy at open, never the source)
        let source_log = source_dir.join("log.zst");
        match std::fs::metadata(&source_log) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Err(SessionError::InvalidInput(format!(
                    "import source {} has no log.zst",
                    source_dir.display()
                )))
            }
            Ok(m) if !m.is_file() => {
                return Err(SessionError::InvalidInput(format!(
                    "import source log {} is not a file",
                    source_log.display()
                )))
            }
            Ok(_) => {}
            Err(e) => return Err(e.into()),
        }
        kanbei_log::scan_frames(&source_log)?;
        // target must be absent or empty (refusing to merge into an
        // existing session dir — mirror of fork)
        match std::fs::metadata(target_dir) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Ok(m) if m.is_dir() => {
                let mut entries = std::fs::read_dir(target_dir)?;
                if entries.next().is_some() {
                    return Err(SessionError::InvalidInput(format!(
                        "import target dir {} is not empty (refusing to merge into an existing session dir)",
                        target_dir.display()
                    )));
                }
            }
            Ok(_) => {
                return Err(SessionError::InvalidInput(format!(
                    "import target {} exists and is not a directory",
                    target_dir.display()
                )))
            }
            Err(e) => return Err(e.into()),
        }
        std::fs::create_dir_all(target_dir).map_err(|e| {
            SessionError::Io(io::Error::new(
                e.kind(),
                format!("import create target {}: {e}", target_dir.display()),
            ))
        })?;
        std::fs::copy(&source_log, target_dir.join("log.zst")).map_err(|e| {
            SessionError::Io(io::Error::new(
                e.kind(),
                format!("import copy {}: {e}", source_log.display()),
            ))
        })?;
        for sub in ["objects", "memory", "state"] {
            let src = source_dir.join(sub);
            if src.is_dir() {
                copy_dir_all(&src, &target_dir.join(sub)).map_err(|e| {
                    SessionError::Io(io::Error::new(
                        e.kind(),
                        format!("import copy {}: {e}", src.display()),
                    ))
                })?;
            }
        }
        let session_id = recover_session_id(source_dir)?;
        let project_id = recover_bound_project(source_dir)?;
        Session::open(SessionConfig {
            dir: target_dir.to_path_buf(),
            session_id,
            project: project_id,
            ..Default::default()
        })
    }
}
// M9 wave 5a helpers (independent-session fork): the fork-floor broker, the
// memory scope-dir copy, and the copied-log truncation at a pinned root.

/// The fork-floor broker (R-24/D-08): READ-ONLY capabilities (`fs.read`,
/// `fs.search`, `git.status`, `git.diff`, `memory.query`) plus an
/// approval-gated `memory.propose` (the approval path is required for
/// consequential effects — the m6 memory_broker allow/require_approval
/// split), one session-scoped grant per resource for the new session's
/// principal, template version 1 monotonic. Returns the broker and the
/// derived grant digests — the `forked` fact's canonical grant record.
fn fork_floor_broker(
    session_id: Id128,
) -> Result<(kanbei_capabilities::Broker, Vec<Digest>), SessionError> {
    let read_only = ["fs.read", "fs.search", "git.status", "git.diff", "memory.query"]
        .map(|r| kanbei_capabilities::Capability::new(r.into(), vec!["call".into()]))
        .to_vec();
    let propose =
        kanbei_capabilities::Capability::new("memory.propose".into(), vec!["call".into()]);
    let mut broker = kanbei_capabilities::Broker::new();
    broker
        .add_template(kanbei_capabilities::PolicyTemplate {
            trust_class: kanbei_capabilities::TrustClass::Builtin,
            allow: {
                let mut allow = read_only.clone();
                allow.push(propose.clone());
                allow
            },
            deny: vec![],
            require_approval: vec![propose.clone()],
            version: 1,
            monotonic: true,
        })
        .map_err(|e| SessionError::InvalidInput(format!("fork-floor template: {e}")))?;
    let mut digests = Vec::new();
    for resource in read_only.into_iter().chain([propose]) {
        let mut grant = kanbei_capabilities::Grant {
            grant_digest: Digest::new(b"placeholder"),
            principal: kanbei_capabilities::Principal {
                session: session_id,
                generation: 0,
                run: None,
            },
            module_generation: 0,
            capability: resource,
            scope: kanbei_capabilities::GrantScope::Session,
            expiry: None,
            budget: None,
            purpose: Some("fork-floor".into()),
            policy_version: 1,
        };
        grant.grant_digest = grant.derive_digest();
        broker
            .add_grant(grant.clone())
            .map_err(|e| SessionError::InvalidInput(format!("fork-floor grant: {e}")))?;
        digests.push(grant.grant_digest);
    }
    Ok((broker, digests))
}

/// Recursive directory copy (the memory-seeding path; the target never
/// pre-exists, so copies never merge).
fn copy_dir_all(src: &Path, dst: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let from = entry.path();
        let to = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_all(&from, &to)?;
        } else {
            std::fs::copy(&from, &to)?;
        }
    }
    Ok(())
}

/// Truncates a copied memory-scope transition log after the frame that
/// commits `root`, so replaying the copied log yields exactly `root` as the
/// actor head — the fork snapshot point; a later source transition never
/// leaks into the fork. Dropping only complete trailing frames keeps the
/// frame chain/digest verification valid. Errors when no frame commits
/// `root`: the fork aborts rather than silently seeding a newer head.
fn truncate_log_at(log_path: &Path, root: Digest) -> Result<(), SessionError> {
    let (boundaries, _truncated) = kanbei_log::scan_frames(log_path)?;
    let mut cut: Option<(u64, u64)> = None;
    let mut frame_idx = 0usize;
    kanbei_log::for_each_frame(log_path, |info| {
        for line in &info.events {
            let Ok(env) = Envelope::from_line(line) else {
                continue;
            };
            if env.kind == "memory_transition"
                && env
                    .payload
                    .get("accepted_new_root")
                    .and_then(|r| r.as_str())
                    .and_then(|r| r.parse::<Digest>().ok())
                    == Some(root)
            {
                cut = Some(boundaries[frame_idx]);
            }
        }
        frame_idx += 1;
    })?;
    let Some((start, len)) = cut else {
        return Err(SessionError::Snapshot(format!(
            "memory root {root} is not committed by the copied transition log {}",
            log_path.display()
        )));
    };
    let end = start + len;
    let f = std::fs::OpenOptions::new().write(true).open(log_path)?;
    f.set_len(end)?;
    Ok(())
}

// ---------- M9 wave 5b helpers (adopt + import) ----------


/// Resolves `digest` in the fork's session store, falling back to its
/// lifetime/project memory stores (a post-fork memory root manifest
/// legitimately exists only in the actor's store — the session store carries
/// root manifests only as checkpoint event objects). `get` hash-verifies, so
/// a resolved object is trusted. Typed `Snapshot` errors name the digest.
fn resolve_fork_object(fork: &Session, digest: &Digest) -> Result<Vec<u8>, SessionError> {
    match fork.store.get(digest) {
        Ok(bytes) => return Ok(bytes),
        Err(ObjectError::Missing { .. }) => {}
        Err(e) => {
            return Err(SessionError::Snapshot(format!(
                "fork object {digest} unreadable: {e}"
            )))
        }
    }
    for store in std::iter::once(fork.memory_lifetime.store())
        .chain(fork.memory_project.as_ref().map(|a| a.store()))
    {
        match store.get(digest) {
            Ok(bytes) => return Ok(bytes),
            Err(ObjectError::Missing { .. }) => {}
            Err(e) => {
                return Err(SessionError::Snapshot(format!(
                    "fork object {digest} unreadable: {e}"
                )))
            }
        }
    }
    Err(SessionError::Snapshot(format!(
        "fork object {digest} is missing from the fork session and memory stores"
    )))
}
