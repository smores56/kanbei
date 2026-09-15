//! Commit path: object install + event frame commit, manifest construction, and envelope access/replay.

use crate::{CommitReceipt, PostManifest, Session, CompactedRange, FaultPoint, NewEvent, RECENT_RING, SessionError};
use kanbei_core::digest::Digest;
use kanbei_core::envelope::Envelope;
use kanbei_snapshot::ExecutionManifest;

impl Session {
    /// Serialized single-writer commit path: install objects (R-10), verify
    /// explicit refs, classify payloads (§7), build envelopes against the
    /// pre-event snapshot (R-08), append one frame, then pin a post-event
    /// manifest iff `state_head` is given. Ack = write + enqueue on the
    /// durability queue (§3); call [`Session::flush`] before consequential
    /// effects.
    ///
    /// M2: the post-event manifest is the schema-2 bootstrap extended with
    /// the module-generation pins (`ModuleManager::snapshot`; scope "/" — M2
    /// activates root-scope modules only), the current composition digest
    /// (R-01: EpochId = composition digest), the engine digest, and
    /// `module_abi = 1`. The toolchain digest stays None — M2 sessions do not
    /// track a toolchain. Content addressing keeps dedup semantics: identical
    /// manifests pin to the same digest.
    pub fn commit(
        &mut self,
        mut events: Vec<NewEvent>,
        state_head: Option<Digest>,
    ) -> Result<CommitReceipt, SessionError> {
        if events.is_empty() {
            return Err(SessionError::InvalidInput("empty commit".into()));
        }

        // R-18/E-06 compaction FSM: a new event whose payload carries a
        // fragment id folded into a committed compaction selection is
        // rejected — its causal parents live inside the compacted range.
        for ev in &events {
            if let Some(fragment) = ev.payload.get("fragment").and_then(|f| f.as_str())
                && self
                    .compacted
                    .iter()
                    .any(|c| c.covered_fragments.iter().any(|f| f == fragment))
            {
                return Err(SessionError::CompactionViolation(fragment.to_string()));
            }
        }

        // steps 2–4 — the tier-1 commit path (objects-first install, ref
        // verification, payload classification, one appended frame) lives in
        // the enforcement kernel; the session owns the tier-2 post-manifest
        // and its own bookkeeping below.
        let fault = self.cfg.fault.clone();
        let outcome = {
            let mut path = kanbei_kernel::commit::CommitPath::new(
                &mut self.log,
                &mut self.store,
                &self.gc_pins,
            );
            path.commit(
                &mut events,
                kanbei_kernel::commit::CommitParams {
                    profile: self.cfg.profile,
                    next_seq: self.next_seq,
                    current_snapshot: self.current_snapshot,
                    inline_max: self.cfg.inline_max,
                },
                fault.as_deref(),
            )?
        };
        self.fault(FaultPoint::BeforeSessionHeadAdvance);
        self.next_seq = outcome.last_seq + 1;
        self.fault(FaultPoint::AfterSessionHeadAdvance);

        // The bounded recent-event ring (the trajectory render source):
        // every committed event enters it; the oldest fall off past
        // RECENT_RING entries.
        for (i, ev) in events.iter().enumerate() {
            self.recent_events.push_back((
                outcome.first_seq + i as u64,
                ev.kind.clone(),
                ev.payload.clone(),
            ));
        }
        while self.recent_events.len() > RECENT_RING {
            self.recent_events.pop_front();
        }
        // Envelope observer + transcript projection (UI seam, decision 30):
        // the transcript is a pure projection of committed envelopes (R-19),
        // and promoted payloads must reach it resolved — a `$object` marker is
        // dereferenced to the full record.
        let resolved: Vec<Envelope> = outcome
            .envelopes
            .iter()
            .map(|env| {
                let mut resolved = env.clone();
                resolved.payload = resolve_payload(&self.store, env);
                resolved
            })
            .collect();
        for env in &resolved {
            self.transcript.apply(env);
        }
        if let Some(listener) = &self.commit_listener {
            for env in &resolved {
                listener(env);
            }
        }
        self.notify_transcript();
        // A committed compaction selection joins the FSM's covered set (the
        // check above rejects its covered fragments from then on).
        for ev in &events {
            if ev.kind != "compaction_selected" {
                continue;
            }
            if let Some(range) = ev.payload.get("range").and_then(|r| r.as_array())
                && range.len() == 2
                && let Some(start) = range[0].as_u64()
                && let Some(end) = range[1].as_u64()
                && let Some(summary) = ev.payload.get("summary_digest").and_then(|d| d.as_str())
                && let Ok(summary) = summary.parse::<Digest>()
            {
                let covered = ev
                    .payload
                    .get("covered_fragments")
                    .and_then(|f| f.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|f| f.as_str().map(str::to_string))
                            .collect()
                    })
                    .unwrap_or_default();
                self.compacted.push(CompactedRange {
                    range: (start, end),
                    summary_digest: summary,
                    covered_fragments: covered,
                });
            }
        }

        // step 5 — state-changing commits pin a post-event manifest; the
        // composition/config objects install first so the closure verifies
        // (R-10). The manifest is built here, after the frame append.
        let post_snapshot = match state_head {
            Some(head) => {
                let payload_schemas: Vec<u32> =
                    events.iter().map(|e| e.payload_schema).collect();
                let post = PostManifest {
                    manifest: self.build_manifest(Some(head), &payload_schemas),
                    composition_bytes: self.composition.current().to_canonical_bytes(),
                    config_objects: self.manifest_config_objects(),
                };
                let digest = kanbei_kernel::commit::pin_post_manifest(
                    &mut self.store,
                    &self.gc_pins,
                    post,
                )?;
                self.current_snapshot = Some(digest);
                Some(digest)
            }
            None => None,
        };

        let receipt = CommitReceipt {
            first_seq: outcome.first_seq,
            last_seq: outcome.last_seq,
            count: outcome.count,
            frame_len: outcome.frame_len,
            objects: outcome.objects,
            pre_snapshot: outcome.pre_snapshot,
            post_snapshot,
        };
        #[cfg(feature = "otel")]
        self.telemetry_commit(&receipt);
        Ok(receipt)
    }

    /// The post-event execution manifest for `state_head` and the committed
    /// payload schemas — the exact byte layout commit step 5 pins (and
    /// [`Session::create_checkpoint`] pre-computes for its payload).
    pub(crate) fn build_manifest(&self, state_head: Option<Digest>, payload_schemas: &[u32]) -> ExecutionManifest {
        let mut manifest = ExecutionManifest::bootstrap();
        manifest.state_head = state_head;
        manifest.modules = self
            .modules
            .as_ref()
            .map(|m| {
                m.snapshot()
                    .into_iter()
                    .map(
                        |(module_id, generation, package)| kanbei_snapshot::ModulePin {
                            module_id,
                            generation,
                            package,
                            // M2 activates root-scope modules only.
                            scope: "/".into(),
                        },
                    )
                    .collect()
            })
            .unwrap_or_default();
        manifest.composition = Some(self.composition.current().digest);
        manifest.engine_digest = self.vm_engine_digest;
        // R-11: model calls and consequential events pin the exact memory
        // roots at commit time.
        manifest.memory_root = self.memory_lifetime.head();
        manifest.project_memory_root = self.memory_project.as_ref().and_then(|a| a.head());
        // M6 wave 2: the tool-registry and provider-config pins are content
        // digests over the canonical bytes; the caller installs those bytes
        // before pinning (closure-valid, R-10). The scheduler policy name is
        // the canonical R-09/E-09 surface. `provider`/`policy`/`projection`
        // versions stay None — no versioned surfaces exist yet.
        manifest.tool_registry = Some(Digest::new(&self.tool_registry.to_canonical_bytes()));
        manifest.provider_config = self
            .provider_config
            .as_ref()
            .map(|cfg| Digest::new(&cfg.to_canonical_bytes()));
        manifest.scheduler_policy = Some(self.scheduler.policy_name().to_string());
        let mut schema_versions = payload_schemas.to_vec();
        schema_versions.push(kanbei_snapshot::MANIFEST_SCHEMA);
        schema_versions.sort_unstable();
        schema_versions.dedup();
        manifest.schema_versions = schema_versions;
        manifest
    }

    /// The canonical config-object bytes the manifest's `tool_registry` and
    /// `provider_config` digests reference — installed into the session store
    /// before the manifest is pinned so the snapshot closure verifies from
    /// the session store alone (R-10; content addressing dedups).
    fn manifest_config_objects(&self) -> Vec<Vec<u8>> {
        let mut objects = vec![self.tool_registry.to_canonical_bytes()];
        if let Some(cfg) = &self.provider_config {
            objects.push(cfg.to_canonical_bytes());
        }
        objects
    }

    /// The envelope at `seq`, scanning the log (M6 checkpoint validation).
    pub fn envelope_at(&self, seq: u64) -> Result<Envelope, SessionError> {
        let log_path = self.log_path.clone();
        let mut found: Option<Envelope> = None;
        kanbei_log::for_each_frame(&log_path, |info| {
            for line in &info.events {
                let Ok(env) = Envelope::from_line(line) else {
                    continue;
                };
                if env.seq == seq {
                    found = Some(env);
                    return;
                }
            }
        })?;
        found.ok_or_else(|| SessionError::InvalidInput(format!("no event at seq {seq}")))
    }

    /// Iterate committed envelopes in seq order starting at `from_seq`,
    /// calling `f` with each resolved envelope (promoted `$object` markers
    /// dereferenced). One pass over the log — the transcript replay seam for
    /// UIs that render the whole history (R-19: the transcript is a pure
    /// projection of committed envelopes; launch is always resume). Returns
    /// the last seq seen (`None` when no envelope reached `from_seq`).
    pub fn replay_envelopes(
        &self,
        from_seq: u64,
        mut f: impl FnMut(&Envelope),
    ) -> Result<Option<u64>, SessionError> {
        let log_path = self.log_path.clone();
        let mut last: Option<u64> = None;
        kanbei_log::for_each_frame(&log_path, |info| {
            for line in &info.events {
                let Ok(env) = Envelope::from_line(line) else {
                    continue;
                };
                let seq = env.seq;
                if seq < from_seq {
                    continue;
                }
                let mut resolved = env;
                if resolved.payload.get("$object").is_some() {
                    resolved.payload = self.resolved_payload(&resolved);
                }
                f(&resolved);
                last = Some(seq);
            }
        })?;
        Ok(last)
    }

    /// Resolve an event payload that may be object-promoted (`{"$object":
    /// "blake3:..."}` markers; §7 — large intents/outcomes live in the
    /// store). Recovery scans must read the resolved payload or promoted
    /// records are invisible: a promoted `tool_intent` would be dropped
    /// from B-05 classification entirely.
    pub(crate) fn resolved_payload(&self, env: &Envelope) -> serde_json::Value {
        resolve_payload(&self.store, env)
    }
}

/// Resolve a possibly object-promoted payload against `store` (see
/// [`Session::resolved_payload`]). Free so callers can hold disjoint borrows of
/// the session (the transcript replay mutates the projection while reading the
/// store).
pub(crate) fn resolve_payload(
    store: &kanbei_objects::ObjectStore,
    env: &Envelope,
) -> serde_json::Value {
    let Some(marker) = env.payload.get("$object").and_then(|o| o.as_str()) else {
        return env.payload.clone();
    };
    match marker.parse::<Digest>() {
        Ok(digest) => store
            .get(&digest)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_else(|| env.payload.clone()),
        Err(_) => env.payload.clone(),
    }
}
