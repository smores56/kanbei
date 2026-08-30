//! Integration tests for kanbei-session M6 wave 2: manifest config
//! population (tool-registry/provider-config/scheduler-policy pins) + the
//! full closure walk, `continue_from`'s memory-follow policy and
//! config-choice record, PinnedAt projection wiring (folds, projection
//! roots, memory.query), `memory_follow_changed`, and pinned-root
//! validation. Guest-wasm tests skip when the guest is not built (see m2.rs).
//!
//! Run `cargo build -p kanbei-guest --target wasm32-wasip1 --release` from
//! the workspace root first; the module-dependent test prints `skip:` and
//! passes without it.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use kanbei_capabilities::{
    Broker, Capability, Grant, GrantScope, PolicyTemplate, Principal, TrustClass,
};
use kanbei_core::digest::Digest;
use kanbei_core::envelope::Envelope;
use kanbei_core::id::Id128;
use kanbei_core::queue::DurabilityQueue;
use kanbei_log::for_each_frame;
use kanbei_memory::{
    Claim, ClaimProvenance, IdempotencyKey, MEMORY_CLAIM_SCHEMA, MEMORY_ROOT_SCHEMA,
    MEMORY_TRANSITION_SCHEMA, MemoryFollowPolicy, MemoryRootActor, MemoryScope, MemoryTransition,
    RootManifest, TransitionKind, TransitionOutcome,
};
use kanbei_modules::{ModuleOrigin, PackageManifest};
use kanbei_objects::ObjectStore;
use kanbei_provider::{KeySource, ProviderConfig};
use kanbei_services::ScopePath;
use kanbei_session::{NewEvent, Session, SessionConfig, SessionError};
use kanbei_snapshot::{ExecutionManifest, manifest_closure, verify_closure};
use kanbei_vm::{GuestError, Vm, VmConfig};
use serde_json::{Value, json};

// --- helpers ---------------------------------------------------------------

struct TempDir(PathBuf);

impl TempDir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kb-session-m6-{tag}-{}-{}",
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

fn fake_config(provider: &str) -> ProviderConfig {
    ProviderConfig {
        provider: provider.into(),
        model: "test".into(),
        base_url: "http://localhost:0/v1".into(),
        key: KeySource::Env("KANBEI_TEST_KEY".into()),
        temperature: None,
        max_tokens: Some(10),
        timeout: std::time::Duration::from_secs(5),
    }
}

/// Module tests need the guest wasm; without it they skip with a note.
fn require_guest() -> bool {
    match Vm::load(no_epoch()) {
        Ok(_) => true,
        Err(GuestError::NotBuilt) => {
            eprintln!(
                "skip: guest wasm not built (run `cargo build -p kanbei-guest \
                 --target wasm32-wasip1 --release`)"
            );
            false
        }
        Err(e) => panic!("Vm::load failed: {e}"),
    }
}

/// Non-fuel, non-epoch-bounded engine config for tests whose modules must not
/// trap.
fn no_epoch() -> VmConfig {
    VmConfig {
        fuel_per_call: u64::MAX,
        epoch_deadline: u64::MAX,
        ..Default::default()
    }
}

fn open_session(dir: &Path) -> Session {
    Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        ..Default::default()
    })
    .unwrap()
}

/// All envelopes currently in the log, in seq order.
fn envelopes(log_path: &Path) -> Vec<Envelope> {
    let mut out = Vec::new();
    for_each_frame(log_path, |frame| {
        for line in &frame.events {
            out.push(Envelope::from_line(line).unwrap());
        }
    })
    .unwrap();
    out
}

/// Commits one lifetime-scope root via a standalone actor (the gate_m4
/// seeding pattern): the session's actor picks the root up at open. Returns
/// the root digest.
fn seed_lifetime_claim(memory_root: &Path, session_id: Id128, text: &str) -> Digest {
    let claim = Claim {
        schema: MEMORY_CLAIM_SCHEMA,
        claim_id: Id128::generate(),
        kind: "decision".into(),
        content: text.into(),
        owner: Principal {
            session: session_id,
            generation: 0,
            run: None,
        },
        visibility_scope: MemoryScope::Lifetime,
        provenance: ClaimProvenance::new_ordinary(session_id, 1),
        observed_at: Some(1_700_000_000),
        valid_from: None,
        sensitivity: "public".into(),
    };
    let mut actor = MemoryRootActor::open(memory_root, MemoryScope::Lifetime).unwrap();
    let queue = Arc::new(DurabilityQueue::start("kb-m6-seed"));
    let mut store =
        ObjectStore::open(&memory_root.join("lifetime/objects"), Arc::clone(&queue)).unwrap();
    let d = store.install(&claim.to_canonical_bytes()).unwrap();
    store.flush().unwrap();
    drop(store);
    if let Ok(q) = Arc::try_unwrap(queue) {
        let _ = q.shutdown();
    }
    let manifest = RootManifest {
        schema: MEMORY_ROOT_SCHEMA,
        parent: None,
        scope: MemoryScope::Lifetime,
        added_claims: vec![d],
        added_edges: vec![],
        retracted: vec![],
        transition_id: Id128::generate(),
    };
    let transition = MemoryTransition {
        schema: MEMORY_TRANSITION_SCHEMA,
        transition_id: manifest.transition_id,
        scope: MemoryScope::Lifetime,
        kind: TransitionKind::RootApproval,
        expected_old_root: None,
        accepted_new_root: manifest.digest(),
        origin_session: session_id,
        origin_event: 1,
        origin_kind: "memory_root_approved".into(),
        decision_principal: Principal {
            session: session_id,
            generation: 0,
            run: None,
        },
        decision_digest: Digest::new(b"m6-seed-decision"),
        idempotency_key: IdempotencyKey {
            session: session_id,
            event: 1,
            decision: Digest::new(b"m6-seed-decision"),
        },
    };
    match actor.propose(transition, &[d], &[]).unwrap() {
        TransitionOutcome::Committed { .. } => {}
        other => panic!("seed propose: expected Committed, got {other:?}"),
    }
    actor.flush().unwrap();
    manifest.digest()
}

/// A broker granting memory.propose (+ memory.query) to the session
/// principal, with approval required when `require_approval` is set (the
/// gate_m4 pattern: with approval the propose flow transitions the root).
fn memory_broker(session_id: Id128, require_approval: bool) -> Broker {
    let mut broker = Broker::new();
    broker
        .add_template(PolicyTemplate {
            trust_class: TrustClass::Builtin,
            allow: vec![
                Capability::new("memory.propose".into(), vec!["call".into()]),
                Capability::new("memory.query".into(), vec!["call".into()]),
            ],
            deny: vec![],
            require_approval: if require_approval {
                vec![Capability::new(
                    "memory.propose".into(),
                    vec!["call".into()],
                )]
            } else {
                vec![]
            },
            version: 1,
            monotonic: true,
        })
        .unwrap();
    for resource in ["memory.propose", "memory.query"] {
        let mut grant = Grant {
            grant_digest: Digest::new(b"placeholder"),
            principal: Principal {
                session: session_id,
                generation: 0,
                run: None,
            },
            module_generation: 0,
            capability: Capability::new(resource.into(), vec!["call".into()]),
            scope: GrantScope::Session,
            expiry: None,
            budget: None,
            purpose: Some("m6".into()),
            policy_version: 1,
        };
        grant.grant_digest = grant.derive_digest();
        broker.add_grant(grant).unwrap();
    }
    broker
}

/// Accept a wake + run start; returns (run_id, trigger).
fn setup_run(session: &mut Session) -> (kanbei_scheduler::RunId, kanbei_scheduler::Trigger) {
    session.observe_trigger(kanbei_scheduler::Trigger {
        kind: kanbei_scheduler::TriggerKind::NewCausalEvent,
        referent: None,
    });
    let run = session.accept_wake().unwrap().expect("wake accepted");
    session.run_start(run.run_id).unwrap();
    (run.run_id, run.trigger)
}

/// One memory.propose tool round trip (intent + outcome committed).
fn propose_claim(
    session: &mut Session,
    run_id: kanbei_scheduler::RunId,
    session_id: Id128,
    claim: Value,
) -> kanbei_tools::ToolOutcome {
    let principal = Principal {
        session: session_id,
        generation: 0,
        run: Some(0),
    };
    let outcome = session
        .tool_call(
            run_id,
            principal,
            "memory.propose",
            json!({ "claim": claim }),
        )
        .unwrap();
    session.commit_tool_outcome(&outcome).unwrap();
    outcome
}

// --- 1. manifest config population + full closure --------------------------

#[test]
fn manifest_config_population_and_closure() {
    let dir = TempDir::new("config");
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        provider: Some(fake_config("fake")),
        ..Default::default()
    })
    .unwrap();
    let head = session.composition().digest;
    let receipt = session
        .commit(
            vec![NewEvent {
                kind: "append_user_message".into(),
                payload_schema: 1,
                payload: json!({"text": "hello"}),
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            Some(head),
        )
        .unwrap();
    let post = receipt
        .post_snapshot
        .expect("state-changing commit pins a manifest");
    let bytes = session.store().get(&post).unwrap();
    let manifest: ExecutionManifest = serde_json::from_slice(&bytes).unwrap();
    // the new pins: tool registry, provider config (digests over the
    // canonical bytes), and the scheduler policy name
    assert_eq!(
        manifest.tool_registry,
        Some(Digest::new(&kanbei_tools::ToolRegistry::builtin().to_canonical_bytes()))
    );
    assert_eq!(
        manifest.provider_config,
        Some(Digest::new(&fake_config("fake").to_canonical_bytes()))
    );
    assert_eq!(manifest.scheduler_policy.as_deref(), Some("builtin-default"));
    assert_eq!(manifest.state_head, Some(head));
    // the full closure walk resolves entirely in the session store — the
    // kernel-embedded engine artifact excepted (its bytes never enter the
    // object store; continue_from applies the same exception)
    let mut refs = manifest_closure(&manifest);
    if let Some(d) = manifest.engine_digest {
        refs.remove(&d);
    }
    if let Some(d) = manifest.toolchain_digest {
        refs.remove(&d);
    }
    assert_eq!(
        verify_closure(session.store(), &refs).unwrap(),
        refs.len() as u64
    );
    session.close().unwrap();
}

// --- 2. continue_from default follow + config choice record ----------------

#[test]
fn continue_from_records_pinned_follow_and_config_choice() {
    let dir = TempDir::new("follow");
    let memory_root = dir.path().join("memory");
    let session_id = Id128::generate();
    let root = seed_lifetime_claim(&memory_root, session_id, "pinned seed");
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        memory_root: Some(memory_root),
        provider: Some(fake_config("fake")),
        session_id: Some(session_id),
        ..Default::default()
    })
    .unwrap();
    let cp = session.create_checkpoint(Some("cp-1".into())).unwrap();
    let record = session.continue_from(&cp).unwrap();
    match record.follow {
        MemoryFollowPolicy::PinnedAt {
            lifetime_root,
            project_root,
        } => {
            assert_eq!(lifetime_root, root, "follow pins the checkpoint's lifetime root");
            assert_eq!(project_root, None);
        }
        other => panic!("expected PinnedAt, got {other:?}"),
    }
    // the transition event payload carries the same typed policy
    let envs = envelopes(session.log_path());
    let transition = envs
        .iter()
        .find(|e| e.kind == "branch_transition")
        .expect("one branch_transition");
    assert_eq!(
        transition.payload["follow"],
        serde_json::to_value(&record.follow).unwrap()
    );
    assert_eq!(
        transition.payload["config_choice"],
        serde_json::to_value(&record.config_choice).unwrap()
    );
    // storage-only session: no live config, no modules — current is None, the
    // historical pin is the checkpoint manifest's provider config digest
    assert_eq!(record.config_choice.mode, "Current");
    assert_eq!(record.config_choice.current, None);
    assert_eq!(
        record.config_choice.historical,
        Some(Digest::new(&fake_config("fake").to_canonical_bytes()))
    );
    assert_eq!(record.config_choice.composition, Some(session.composition().digest));
    // no lifetime root pinned → the branch cannot pin → FollowHead
    let dir2 = TempDir::new("follow-noroot");
    let mut session2 = open_session(dir2.path());
    let cp2 = session2.create_checkpoint(Some("cp-empty".into())).unwrap();
    let record2 = session2.continue_from(&cp2).unwrap();
    assert_eq!(record2.follow, MemoryFollowPolicy::FollowHead);
    session.close().unwrap();
    session2.close().unwrap();
}

// --- 3. PinnedAt projection: checkpoint-era roots, not the live head -------

#[test]
fn pinned_at_projection_uses_checkpoint_roots() {
    let dir = TempDir::new("pinned");
    let memory_root = dir.path().join("memory");
    let session_id = Id128::generate();
    let lifetime_root = seed_lifetime_claim(&memory_root, session_id, "lifetime seed");
    let project_id = Id128::generate();
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        memory_root: Some(memory_root),
        project: Some(project_id),
        broker: memory_broker(session_id, true),
        session_id: Some(session_id),
        ..Default::default()
    })
    .unwrap();
    let (run1, _) = setup_run(&mut session);
    let out1 = propose_claim(
        &mut session,
        run1,
        session_id,
        json!({"kind": "decision", "content": "widget v1"}),
    );
    assert_eq!(out1.result["status"], "approved");
    let r1 = session.memory_project().unwrap().head().unwrap();
    let cp = session.create_checkpoint(Some("cp-pinned".into())).unwrap();
    let record = session.continue_from(&cp).unwrap();
    assert_eq!(
        record.follow,
        MemoryFollowPolicy::PinnedAt {
            lifetime_root,
            project_root: Some(r1),
        }
    );
    // a post-checkpoint transition (new run — continue_from quiesced run1)
    // advances the live project head
    let (run2, trigger2) = setup_run(&mut session);
    let out2 = propose_claim(
        &mut session,
        run2,
        session_id,
        json!({"kind": "decision", "content": "widget v2"}),
    );
    assert_eq!(out2.result["status"], "approved");
    let r2 = session.memory_project().unwrap().head().unwrap();
    assert_ne!(r1, r2, "the post-checkpoint transition must advance the head");
    // the projection resolves against the pinned roots — the checkpoint-era
    // claim set — not the live head
    let ctx = session.project_context(run2, &trigger2).unwrap();
    assert_eq!(
        ctx.memory_roots,
        vec![lifetime_root, r1],
        "projection must pin the checkpoint-era roots, not the live head {r2}"
    );
    // memory.query resolves against the pinned folds too: the checkpoint-era
    // claim set excludes the post-checkpoint claim
    let principal = Principal {
        session: session_id,
        generation: 0,
        run: Some(0),
    };
    let q = session
        .tool_call(run2, principal, "memory.query", json!({"query": "widget"}))
        .unwrap();
    let claims = q.result["claims"].as_array().unwrap();
    let texts: Vec<&str> = claims.iter().map(|c| c["text"].as_str().unwrap()).collect();
    assert_eq!(texts, vec!["widget v1"], "query must see the pinned claim set only");
    session.commit_tool_outcome(&q).unwrap();
    session.close().unwrap();
}

/// A PinnedAt branch's model_call intent carries the checkpoint-era roots
/// (the projection-state pins, R-08/E-13), not the live head.
#[test]
fn pinned_at_model_call_pins_checkpoint_roots() {
    let dir = TempDir::new("pinned-model");
    let memory_root = dir.path().join("memory");
    let session_id = Id128::generate();
    let lifetime_root = seed_lifetime_claim(&memory_root, session_id, "lifetime seed");
    let project_id = Id128::generate();
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        memory_root: Some(memory_root),
        project: Some(project_id),
        broker: memory_broker(session_id, true),
        provider: Some(fake_config("fake")),
        provider_engine: Some(Box::new(kanbei_provider::FakeEngine::new(
            fake_config("fake"),
            vec![kanbei_provider::CompletionResponse {
                content: Some("answer".into()),
                tool_calls: vec![],
                finish_reason: kanbei_provider::FinishReason::Stop,
                usage: kanbei_provider::Usage {
                    input_tokens: 5,
                    output_tokens: 5,
                },
                discontinuity: None,
                opaque_artifacts: None,
            }],
        ))),
        session_id: Some(session_id),
        ..Default::default()
    })
    .unwrap();
    let (run1, _) = setup_run(&mut session);
    let out1 = propose_claim(
        &mut session,
        run1,
        session_id,
        json!({"kind": "decision", "content": "widget v1"}),
    );
    assert_eq!(out1.result["status"], "approved");
    let r1 = session.memory_project().unwrap().head().unwrap();
    let cp = session.create_checkpoint(Some("cp-pinned-model".into())).unwrap();
    session.continue_from(&cp).unwrap();
    let (run2, trigger2) = setup_run(&mut session);
    let out2 = propose_claim(
        &mut session,
        run2,
        session_id,
        json!({"kind": "decision", "content": "widget v2"}),
    );
    assert_eq!(out2.result["status"], "approved");
    let r2 = session.memory_project().unwrap().head().unwrap();
    assert_ne!(r1, r2);
    // materialize the pinned projection, then a model call
    session.project_context(run2, &trigger2).unwrap();
    session
        .model_call(
            run2,
            vec![kanbei_provider::Message {
                role: kanbei_provider::Role::User,
                content: "probe".into(),
                tool_call_id: None,
            }],
            Vec::new(),
            "rendered probe",
        )
        .unwrap();
    // the committed model_call intent pins the checkpoint-era roots
    let envs = envelopes(session.log_path());
    let intent = envs
        .iter()
        .find(|e| e.kind == "model_call")
        .expect("one model_call intent");
    let roots: Vec<String> = intent.payload["memory_roots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        roots,
        vec![lifetime_root.to_string(), r1.to_string()],
        "the intent must pin the checkpoint-era roots, not the live head {r2}"
    );
    session.close().unwrap();
}

// --- 4. memory_follow_changed: release / re-pin + validation ---------------

#[test]
fn memory_follow_changed_releases_and_validates() {
    let dir = TempDir::new("follow-changed");
    let memory_root = dir.path().join("memory");
    let session_id = Id128::generate();
    let lifetime_root = seed_lifetime_claim(&memory_root, session_id, "lifetime seed");
    let project_id = Id128::generate();
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        memory_root: Some(memory_root),
        project: Some(project_id),
        broker: memory_broker(session_id, true),
        session_id: Some(session_id),
        ..Default::default()
    })
    .unwrap();
    let (run1, _) = setup_run(&mut session);
    let out1 = propose_claim(
        &mut session,
        run1,
        session_id,
        json!({"kind": "decision", "content": "widget v1"}),
    );
    assert_eq!(out1.result["status"], "approved");
    let r1 = session.memory_project().unwrap().head().unwrap();
    let cp = session.create_checkpoint(Some("cp".into())).unwrap();
    session.continue_from(&cp).unwrap();
    let (run2, trigger2) = setup_run(&mut session);
    let out2 = propose_claim(
        &mut session,
        run2,
        session_id,
        json!({"kind": "decision", "content": "widget v2"}),
    );
    assert_eq!(out2.result["status"], "approved");
    let r2 = session.memory_project().unwrap().head().unwrap();
    let before = envelopes(session.log_path()).len();
    // FollowHead: releases the pins — the next projection uses the live heads
    session
        .memory_follow(MemoryFollowPolicy::FollowHead)
        .unwrap();
    let ctx = session.project_context(run2, &trigger2).unwrap();
    assert_eq!(
        ctx.memory_roots,
        vec![lifetime_root, r2],
        "FollowHead projection must use the live heads"
    );
    // the record is canonical: one event, schema 1, {policy, at: its seq}
    let envs = envelopes(session.log_path());
    assert_eq!(envs.len(), before + 1);
    let changed = envs.last().unwrap();
    assert_eq!(changed.kind, "memory_follow_changed");
    assert_eq!(changed.payload_schema, 1);
    assert_eq!(changed.payload["at"], changed.seq);
    assert_eq!(
        changed.payload["policy"],
        serde_json::to_value(MemoryFollowPolicy::FollowHead).unwrap()
    );
    // re-pin to the checkpoint-era roots works (they are committed roots)
    session
        .memory_follow(MemoryFollowPolicy::PinnedAt {
            lifetime_root,
            project_root: Some(r1),
        })
        .unwrap();
    let ctx = session.project_context(run2, &trigger2).unwrap();
    assert_eq!(ctx.memory_roots, vec![lifetime_root, r1]);
    // a fabricated root is rejected explicitly, with no event committed
    let before = envelopes(session.log_path()).len();
    let err = session
        .memory_follow(MemoryFollowPolicy::PinnedAt {
            lifetime_root: Digest::new(b"fabricated"),
            project_root: None,
        })
        .unwrap_err();
    assert!(
        matches!(err, SessionError::InvalidInput(_)),
        "expected InvalidInput, got {err:?}"
    );
    assert_eq!(
        envelopes(session.log_path()).len(),
        before,
        "an invalid pin must commit no event"
    );
    session.close().unwrap();
}

// --- 5. config choice with a live config manifest (guest wasm) -------------

#[test]
fn continue_from_records_live_config_digest() {
    if !require_guest() {
        return;
    }
    let dir = TempDir::new("choice");
    let memory_root = dir.path().join("memory");
    let session_id = Id128::generate();
    seed_lifetime_claim(&memory_root, session_id, "choice seed");
    let config = PackageManifest {
        schema: 1,
        module_id: Id128::generate(),
        origin: ModuleOrigin::UserConfig,
        trust_class: TrustClass::User,
        scope: ScopePath(vec![]),
        deps: vec![],
        capabilities: vec![],
        source: "function kb_on_activate(ctx) ctx.service_publish('{\"scope\":[],\"name\":\"m6-greeter\"}', 1, '[]') end\nfunction kb_hot(x) return x end".into(),
        state_schema: None,
    };
    // the package digest is the canonical content digest (install_package)
    let config_digest = Digest::new(&serde_json::to_vec(&config).unwrap());
    let provider_cfg = fake_config("fake");
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        memory_root: Some(memory_root),
        provider: Some(provider_cfg.clone()),
        config: Some(config),
        session_id: Some(session_id),
        engine: Some(no_epoch()),
        ..Default::default()
    })
    .unwrap();
    let cp = session.create_checkpoint(Some("cp".into())).unwrap();
    let record = session.continue_from(&cp).unwrap();
    let expected = kanbei_session::ConfigChoiceRecord {
        mode: "Current".into(),
        current: Some(config_digest),
        historical: Some(Digest::new(&provider_cfg.to_canonical_bytes())),
        composition: Some(session.composition().digest),
    };
    assert_eq!(record.config_choice, expected);
    session.close().unwrap();
}

// --- 6. contains_root ------------------------------------------------------

#[test]
fn contains_root_known_and_fabricated() {
    let dir = TempDir::new("contains");
    let memory_root = dir.path().join("memory");
    let session_id = Id128::generate();
    let root = seed_lifetime_claim(&memory_root, session_id, "known root");
    let actor = MemoryRootActor::open(&memory_root, MemoryScope::Lifetime).unwrap();
    assert!(actor.contains_root(&root), "the committed root is known");
    assert!(
        !actor.contains_root(&Digest::new(b"fabricated")),
        "a fabricated digest is unknown"
    );
    // an actor with no transitions knows no roots
    let empty =
        MemoryRootActor::open(&memory_root, MemoryScope::Project(Id128::generate())).unwrap();
    assert!(!empty.contains_root(&root));
}

// --- 7. corrupted checkpoint: explicit error, no branch --------------------

#[test]
fn continue_from_rejects_unknown_pinned_root() {
    let dir = TempDir::new("bogus");
    let memory_root = dir.path().join("memory");
    let session_id = Id128::generate();
    seed_lifetime_claim(&memory_root, session_id, "bogus seed");
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        memory_root: Some(memory_root),
        provider: Some(fake_config("fake")),
        session_id: Some(session_id),
        ..Default::default()
    })
    .unwrap();
    // a real checkpoint to steal a closure-valid snapshot digest from
    let real = session.create_checkpoint(Some("real".into())).unwrap();
    let real_env = envelopes(session.log_path())
        .into_iter()
        .find(|e| e.seq == real.seq)
        .unwrap();
    let snapshot = real_env.payload["snapshot"].as_str().unwrap().to_string();
    // hand-commit a corrupted checkpoint event: valid snapshot, bogus
    // memory_root (continue_from must reject it before any transition)
    let seq = session.next_seq();
    let before = envelopes(session.log_path()).len();
    session
        .commit(
            vec![NewEvent {
                kind: "checkpoint_created".into(),
                payload_schema: 1,
                payload: json!({
                    "label": null,
                    "frontier_seq": seq,
                    "snapshot": snapshot,
                    "memory_root": Digest::new(b"fabricated").to_string(),
                    "project_memory_root": null,
                    "composition": session.composition().digest.to_string(),
                }),
                objects: Vec::new(),
                refs: Vec::new(),
            }],
            None,
        )
        .unwrap();
    let err = session
        .continue_from(&kanbei_session::CheckpointRef {
            session_id,
            seq,
        })
        .unwrap_err();
    assert!(
        matches!(err, SessionError::InvalidInput(_)),
        "expected InvalidInput, got {err:?}"
    );
    let envs = envelopes(session.log_path());
    assert_eq!(envs.len(), before + 1, "only the corrupted event was committed");
    assert!(
        !envs.iter().any(|e| e.kind == "branch_transition"),
        "no branch_transition may follow a rejected checkpoint"
    );
    session.close().unwrap();
}

// --- 8. M6 wave 3: discontinuity flags + opaque artifact replay ------------

/// One scripted response; the new fields default to None.
fn scripted(
    content: &str,
    discontinuity: Option<&str>,
    opaque_artifacts: Option<&str>,
) -> kanbei_provider::CompletionResponse {
    kanbei_provider::CompletionResponse {
        content: Some(content.into()),
        tool_calls: vec![],
        finish_reason: kanbei_provider::FinishReason::Stop,
        usage: kanbei_provider::Usage {
            input_tokens: 1,
            output_tokens: 1,
        },
        discontinuity: discontinuity.map(|s| s.into()),
        opaque_artifacts: opaque_artifacts.map(|s| s.into()),
    }
}

fn probe_msg() -> Vec<kanbei_provider::Message> {
    vec![kanbei_provider::Message {
        role: kanbei_provider::Role::User,
        content: "probe".into(),
        tool_call_id: None,
    }]
}

/// A cloneable engine handle so two sessions share one FakeEngine request
/// log (asserting what each call actually received).
#[derive(Clone)]
struct SharedFake(Arc<kanbei_provider::FakeEngine>);

impl kanbei_provider::ProviderEngine for SharedFake {
    fn complete(
        &self,
        req: &kanbei_provider::CompletionRequest,
    ) -> Result<kanbei_provider::CompletionResponse, kanbei_provider::ProviderError> {
        self.0.complete(req)
    }

    fn identity(&self) -> &str {
        self.0.identity()
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self.0.as_any()
    }
}

#[test]
fn model_flagged_discontinuity_records_reason() {
    let dir = TempDir::new("w3-flag");
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        provider: Some(fake_config("fake-a")),
        provider_engine: Some(Box::new(kanbei_provider::FakeEngine::new(
            fake_config("fake-a"),
            vec![scripted("answer", Some("projection"), None)],
        ))),
        ..Default::default()
    })
    .unwrap();
    let (run, _) = setup_run(&mut session);
    session
        .model_call(run, probe_msg(), Vec::new(), "rendered probe")
        .unwrap();
    let envs = envelopes(session.log_path());
    let outcome = envs
        .iter()
        .find(|e| e.kind == "model_outcome")
        .expect("one model outcome");
    // the model's own flag wins over the provider-change heuristic: Broken
    // against the CURRENT provider, with the raw flag as the reason
    assert_eq!(
        outcome.payload["reasoning_continuity"],
        json!({"Broken": {"from_provider": "fake-a", "at_event": outcome.seq, "reason": "projection"}})
    );
    assert_eq!(outcome.payload["discontinuity"], json!("projection"));
    session.close().unwrap();
}

#[test]
fn same_provider_without_flag_stays_continuous() {
    let dir = TempDir::new("w3-cont");
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        provider: Some(fake_config("fake-a")),
        provider_engine: Some(Box::new(kanbei_provider::FakeEngine::new(
            fake_config("fake-a"),
            vec![scripted("a", None, None), scripted("b", None, None)],
        ))),
        ..Default::default()
    })
    .unwrap();
    let (run, _) = setup_run(&mut session);
    session
        .model_call(run, probe_msg(), Vec::new(), "rendered")
        .unwrap();
    session
        .model_call(run, probe_msg(), Vec::new(), "rendered")
        .unwrap();
    let envs = envelopes(session.log_path());
    let outs: Vec<_> = envs.iter().filter(|e| e.kind == "model_outcome").collect();
    assert_eq!(outs.len(), 2);
    // first call breaks from "none" (no reason recorded), the second —
    // same provider, no flag — is Continuous
    assert_eq!(
        outs[0].payload["reasoning_continuity"],
        json!({"Broken": {"from_provider": "none", "at_event": outs[0].seq, "reason": null}})
    );
    assert_eq!(outs[1].payload["reasoning_continuity"], json!("Continuous"));
    session.close().unwrap();
}

#[test]
fn opaque_artifacts_replay_same_provider_only() {
    let shared = SharedFake(Arc::new(kanbei_provider::FakeEngine::new(
        fake_config("fake-a"),
        vec![
            scripted("a", None, Some("Ymxvcg==")),
            scripted("b", None, None),
            scripted("c", None, None),
        ],
    )));
    let dir = TempDir::new("w3-art-a");
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        provider: Some(fake_config("fake-a")),
        provider_engine: Some(Box::new(shared.clone())),
        ..Default::default()
    })
    .unwrap();
    let (run, _) = setup_run(&mut session);
    session
        .model_call(run, probe_msg(), Vec::new(), "rendered")
        .unwrap();
    session
        .model_call(run, probe_msg(), Vec::new(), "rendered")
        .unwrap();
    // a different provider (fresh session with its own config, the gate_m4
    // pattern) must never receive the artifacts — cross-provider transfer
    // is prohibited (E-07, transferability default NONE)
    let dir2 = TempDir::new("w3-art-b");
    let mut session_b = Session::open(SessionConfig {
        dir: dir2.path().to_path_buf(),
        provider: Some(fake_config("fake-b")),
        provider_engine: Some(Box::new(shared.clone())),
        ..Default::default()
    })
    .unwrap();
    let (run_b, _) = setup_run(&mut session_b);
    session_b
        .model_call(run_b, probe_msg(), Vec::new(), "rendered")
        .unwrap();
    // the outcome record carries the emitted artifacts verbatim
    let envs = envelopes(session.log_path());
    let outs: Vec<_> = envs.iter().filter(|e| e.kind == "model_outcome").collect();
    assert_eq!(outs.len(), 2);
    assert_eq!(outs[0].payload["opaque_artifacts"], json!("Ymxvcg=="));
    assert!(outs[0].payload["discontinuity"].is_null());
    // the shared request log: emitted on call 1, replayed on call 2 (same
    // provider), never sent to the other provider on call 3
    let reqs = shared.0.requests.lock().unwrap();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[0].opaque_artifacts, None);
    assert_eq!(reqs[1].opaque_artifacts.as_deref(), Some("Ymxvcg=="));
    assert_eq!(reqs[2].opaque_artifacts, None);
    session.close().unwrap();
    session_b.close().unwrap();
}

#[test]
fn opaque_artifacts_round_trip_byte_exact() {
    // base64 of bytes 0..=255: any re-encoding or truncation corrupts it
    const ALL_BYTES: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8gISIjJCUmJygpKissLS4vMDEyMzQ1Njc4OTo7PD0+P0BBQkNERUZHSElKS0xNTk9QUVJTVFVWV1hZWltcXV5fYGFiY2RlZmdoaWprbG1ub3BxcnN0dXZ3eHl6e3x9fn+AgYKDhIWGh4iJiouMjY6PkJGSk5SVlpeYmZqbnJ2en6ChoqOkpaanqKmqq6ytrq+wsbKztLW2t7i5uru8vb6/wMHCw8TFxsfIycrLzM3Oz9DR0tPU1dbX2Nna29zd3t/g4eLj5OXm5+jp6uvs7e7v8PHy8/T19vf4+fr7/P3+/w==";
    let shared = SharedFake(Arc::new(kanbei_provider::FakeEngine::new(
        fake_config("fake-a"),
        vec![scripted("a", None, Some(ALL_BYTES)), scripted("b", None, None)],
    )));
    let dir = TempDir::new("w3-exact");
    let mut session = Session::open(SessionConfig {
        dir: dir.path().to_path_buf(),
        provider: Some(fake_config("fake-a")),
        provider_engine: Some(Box::new(shared.clone())),
        ..Default::default()
    })
    .unwrap();
    let (run, _) = setup_run(&mut session);
    session
        .model_call(run, probe_msg(), Vec::new(), "rendered")
        .unwrap();
    session
        .model_call(run, probe_msg(), Vec::new(), "rendered")
        .unwrap();
    let envs = envelopes(session.log_path());
    let outs: Vec<_> = envs.iter().filter(|e| e.kind == "model_outcome").collect();
    // byte-exact in the canonical record (S9 acceptance)
    assert_eq!(outs[0].payload["opaque_artifacts"], json!(ALL_BYTES));
    // and byte-exact in the replay the same-provider call received
    let reqs = shared.0.requests.lock().unwrap();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[1].opaque_artifacts.as_deref(), Some(ALL_BYTES));
    session.close().unwrap();
}
