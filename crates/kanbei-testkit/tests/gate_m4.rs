#![allow(clippy::result_large_err)]

//! M4 milestone gate tests (docs/architecture.md "Memory" + "History and
//! context projection"): the memory substrate session integration — the
//! propose/approve/transition/backlink flow with crash recovery (R-11),
//! cross-session root CAS determinism, the memory tools, bounded child runs
//! with attenuated scope (R-09), projection cache/continuity records
//! (R-08/E-13, R-18/E-07), manifest memory-root pins, the compaction FSM
//! (R-18/E-06), and the consistency-15 scope invariance.

use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use kanbei_capabilities::{
    Broker, Capability, Grant, GrantScope, PolicyTemplate, Principal, TrustClass,
};
use kanbei_core::digest::Digest;
use kanbei_core::id::Id128;
use kanbei_core::queue::DurabilityQueue;
use kanbei_memory::{
    Claim, ClaimProvenance, EdgeKind, IdempotencyKey, MEMORY_CLAIM_SCHEMA, MEMORY_ROOT_SCHEMA,
    MEMORY_TRANSITION_SCHEMA, MemoryRootActor, MemoryScope, MemoryTransition, RootManifest,
    TransitionKind, TransitionOutcome,
};
use kanbei_objects::ObjectStore;
use kanbei_provider::{
    CompletionResponse, FakeEngine, FinishReason, KeySource, ProviderConfig, Usage,
};
use kanbei_scheduler::{
    CognitionProvider, ModelCallSpec, StepCommand, StepContext, StepError, StepResult,
    TerminalOutcome, Trigger, TriggerKind,
};
use kanbei_session::{FaultPoint, NewEvent, Session, SessionConfig, SessionError};
use kanbei_snapshot::ExecutionManifest;
use kanbei_testkit::{
    CrashPoint, child_acked, collect_envelopes, spawn_m4_crash_child, verify_m4_recovery,
};
use serde_json::{Value, json};

fn fresh_session_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("kb-gate-m4-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

struct DirGuard(PathBuf);
impl Drop for DirGuard {
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

fn response(content: &str, input: u64, output: u64) -> CompletionResponse {
    CompletionResponse {
        content: Some(content.into()),
        tool_calls: vec![],
        finish_reason: FinishReason::Stop,
        usage: Usage {
            input_tokens: input,
            output_tokens: output,
        },
    discontinuity: None,
    opaque_artifacts: None,
    }
}

/// A broker granting memory.propose (+ optionally memory.query) to the
/// session principal, with approval required when `require_approval` is set.
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
            purpose: Some("gate".into()),
            policy_version: 1,
        };
        grant.grant_digest = grant.derive_digest();
        broker.add_grant(grant).unwrap();
    }
    broker
}

/// Accept a wake + run start; returns (run_id, trigger).
fn setup_run(session: &mut Session) -> (kanbei_scheduler::RunId, Trigger) {
    session.observe_trigger(Trigger {
        kind: TriggerKind::NewCausalEvent,
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
    if outcome.awaiting_approval() {
        // the harness plays the user: resolve the parked approval so the
        // propose flow proceeds (the approval gate parks by design)
        let digest = *session
            .pending_approvals()
            .last()
            .expect("a parked approval");
        let resolved = session
            .resolve_approval(&digest, true)
            .unwrap()
            .expect("approval resolves while parked");
        return resolved;
    }
    session.commit_tool_outcome(&outcome).unwrap();
    outcome
}

/// One memory.query tool round trip.
fn query_memory(
    session: &mut Session,
    run_id: kanbei_scheduler::RunId,
    session_id: Id128,
    query: &str,
) -> kanbei_tools::ToolOutcome {
    let principal = Principal {
        session: session_id,
        generation: 0,
        run: Some(0),
    };
    let outcome = session
        .tool_call(run_id, principal, "memory.query", json!({ "query": query }))
        .unwrap();
    session.commit_tool_outcome(&outcome).unwrap();
    outcome
}

fn envelopes(dir: &Path) -> Vec<kanbei_core::envelope::Envelope> {
    collect_envelopes(dir).unwrap()
}

struct RecordingProvider {
    commands: std::collections::VecDeque<StepCommand>,
    results: Arc<Mutex<Vec<StepResult>>>,
}

impl RecordingProvider {
    fn new(commands: Vec<StepCommand>, results: Arc<Mutex<Vec<StepResult>>>) -> Self {
        Self {
            commands: commands.into(),
            results,
        }
    }
}

impl CognitionProvider for RecordingProvider {
    fn step(
        &mut self,
        _context: &StepContext,
        _trigger: &Trigger,
        last: Option<&StepResult>,
    ) -> Result<StepCommand, StepError> {
        if let Some(l) = last {
            self.results.lock().unwrap().push(l.clone());
        }
        self.commands
            .pop_front()
            .ok_or(StepError::Invalid("no more commands".into()))
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

// --- 1. crash matrix over the six M4 seams ---------------------------------

#[test]
fn crash_matrix_memory_root_transition() {
    const SESSION_POINTS: [FaultPoint; 2] = [
        FaultPoint::BeforeMemoryProposal,
        FaultPoint::AfterMemoryProposal,
    ];
    const MEMORY_POINTS: [kanbei_memory::MemoryFaultPoint; 4] = [
        kanbei_memory::MemoryFaultPoint::BeforeTransition,
        kanbei_memory::MemoryFaultPoint::AfterTransition,
        kanbei_memory::MemoryFaultPoint::BeforeHeadUpdate,
        kanbei_memory::MemoryFaultPoint::AfterHeadUpdate,
    ];
    for point in SESSION_POINTS {
        let dir = fresh_session_dir(&format!("m4-crash-{point:?}"));
        let _guard = DirGuard(dir.clone());
        let mut child = spawn_m4_crash_child(&dir, Some(CrashPoint::Session(point)), 3);
        let status = child.wait().unwrap();
        let acked = child_acked(&mut child);
        assert_eq!(
            status.signal(),
            Some(6),
            "{point:?}: child must abort (SIGABRT), exited {status:?}"
        );
        let checks = verify_m4_recovery(&dir, acked).unwrap_or_else(|e| panic!("{point:?}: {e}"));
        assert!(checks >= 5, "{point:?}: expected >=5 checks, got {checks}");
        println!("m4 {point:?}: acked={acked} crashed=true checks={checks}");
    }
    for point in MEMORY_POINTS {
        let dir = fresh_session_dir(&format!("m4-crash-{point:?}"));
        let _guard = DirGuard(dir.clone());
        let mut child = spawn_m4_crash_child(&dir, Some(CrashPoint::Memory(point)), 3);
        let status = child.wait().unwrap();
        let acked = child_acked(&mut child);
        assert_eq!(
            status.signal(),
            Some(6),
            "{point:?}: child must abort (SIGABRT), exited {status:?}"
        );
        let checks = verify_m4_recovery(&dir, acked).unwrap_or_else(|e| panic!("{point:?}: {e}"));
        assert!(checks >= 6, "{point:?}: expected >=6 checks, got {checks}");
        println!("m4 {point:?}: acked={acked} crashed=true checks={checks}");
    }
}

// --- 2. cross-session root CAS determinism ---------------------------------

#[test]
fn concurrent_session_root_cas_deterministic() {
    let shared = fresh_session_dir("cas-shared");
    let _guard = DirGuard(shared.clone());
    let project_id = Id128::generate();

    // Pair 1: A commits first (genesis), B's stale proposal CAS-fails and
    // rebases onto A's root.
    let dir_a = fresh_session_dir("cas-a");
    let dir_b = fresh_session_dir("cas-b");
    let _ga = DirGuard(dir_a.clone());
    let _gb = DirGuard(dir_b.clone());
    let session_a_id = Id128::generate();
    let session_b_id = Id128::generate();
    let mut a = Session::open(SessionConfig {
        dir: dir_a.clone(),
        memory_root: Some(shared.clone()),
        project: Some(project_id),
        broker: memory_broker(session_a_id, true),
        session_id: Some(session_a_id),
        ..Default::default()
    })
    .unwrap();
    let mut b = Session::open(SessionConfig {
        dir: dir_b.clone(),
        memory_root: Some(shared.clone()),
        project: Some(project_id),
        broker: memory_broker(session_b_id, true),
        session_id: Some(session_b_id),
        ..Default::default()
    })
    .unwrap();
    let (run_a, _) = setup_run(&mut a);
    let (run_b, _) = setup_run(&mut b);
    let out_a = propose_claim(
        &mut a,
        run_a,
        session_a_id,
        json!({"kind": "decision", "content": "claim from session A"}),
    );
    assert_eq!(out_a.result["status"], "approved");
    let out_b = propose_claim(
        &mut b,
        run_b,
        session_b_id,
        json!({"kind": "decision", "content": "claim from session B"}),
    );
    assert_eq!(out_b.result["status"], "approved");

    // B's rebased manifest chain: A genesis, then B with parent = A's root.
    let fold_b = b
        .memory_project()
        .unwrap()
        .fold(b.memory_project().unwrap().head())
        .unwrap();
    assert_eq!(fold_b.history.len(), 2);
    assert_eq!(fold_b.claims.len(), 2);
    let store_b = b.memory_project().unwrap().store();
    let a_manifest: RootManifest =
        serde_json::from_slice(&store_b.get(&fold_b.history[0]).unwrap()).unwrap();
    let b_manifest: RootManifest =
        serde_json::from_slice(&store_b.get(&fold_b.history[1]).unwrap()).unwrap();
    assert_eq!(a_manifest.parent, None, "A's transition is genesis");
    assert_eq!(
        b_manifest.parent,
        Some(fold_b.history[0]),
        "B's transition rebases onto A's root"
    );
    let mut contents_b: Vec<String> = fold_b
        .claims
        .iter()
        .map(|(_, c)| c.content.clone())
        .collect();
    contents_b.sort();
    a.close().unwrap();
    b.close().unwrap();

    // Pair 2: B commits first; the final fold is the same claim content
    // (determinism — order-independent outcome), on its own shared root.
    let shared2 = fresh_session_dir("cas-shared2");
    let _gs2 = DirGuard(shared2.clone());
    let dir_a2 = fresh_session_dir("cas-a2");
    let dir_b2 = fresh_session_dir("cas-b2");
    let _ga2 = DirGuard(dir_a2.clone());
    let _gb2 = DirGuard(dir_b2.clone());
    let s_a2 = Id128::generate();
    let s_b2 = Id128::generate();
    let mut a2 = Session::open(SessionConfig {
        dir: dir_a2.clone(),
        memory_root: Some(shared2.clone()),
        project: Some(project_id),
        broker: memory_broker(s_a2, true),
        session_id: Some(s_a2),
        ..Default::default()
    })
    .unwrap();
    let mut b2 = Session::open(SessionConfig {
        dir: dir_b2.clone(),
        memory_root: Some(shared2.clone()),
        project: Some(project_id),
        broker: memory_broker(s_b2, true),
        session_id: Some(s_b2),
        ..Default::default()
    })
    .unwrap();
    let (run_b2, _) = setup_run(&mut b2);
    let (run_a2, _) = setup_run(&mut a2);
    let out_b2 = propose_claim(
        &mut b2,
        run_b2,
        s_b2,
        json!({"kind": "decision", "content": "claim from session B"}),
    );
    assert_eq!(out_b2.result["status"], "approved");
    let out_a2 = propose_claim(
        &mut a2,
        run_a2,
        s_a2,
        json!({"kind": "decision", "content": "claim from session A"}),
    );
    assert_eq!(out_a2.result["status"], "approved");
    let fold_a2 = a2
        .memory_project()
        .unwrap()
        .fold(a2.memory_project().unwrap().head())
        .unwrap();
    assert_eq!(fold_a2.claims.len(), 2);
    let mut contents_a2: Vec<String> = fold_a2
        .claims
        .iter()
        .map(|(_, c)| c.content.clone())
        .collect();
    contents_a2.sort();
    assert_eq!(
        contents_a2, contents_b,
        "final folds are identical claim sets"
    );
    a2.close().unwrap();
    b2.close().unwrap();
}

// --- 3. memory tools end to end --------------------------------------------

#[test]
fn memory_tools_e2e() {
    let shared = fresh_session_dir("e2e-shared");
    let _guard = DirGuard(shared.clone());
    let project_id = Id128::generate();

    // Without an approval requirement the proposal is left unresolved.
    let dir1 = fresh_session_dir("e2e-1");
    let _g1 = DirGuard(dir1.clone());
    let s1 = Id128::generate();
    let mut session1 = Session::open(SessionConfig {
        dir: dir1.clone(),
        memory_root: Some(shared.clone()),
        project: Some(project_id),
        broker: memory_broker(s1, false),
        session_id: Some(s1),
        ..Default::default()
    })
    .unwrap();
    let (run1, _) = setup_run(&mut session1);
    let out1 = propose_claim(
        &mut session1,
        run1,
        s1,
        json!({"kind": "decision", "content": "the unapproved widget"}),
    );
    assert_eq!(out1.result["status"], "proposed");
    assert_eq!(out1.result.get("transition_id"), None);
    assert_eq!(session1.memory_project().unwrap().transition_count(), 0);
    session1.close().unwrap();

    // With approval: transition + backlink; the query returns the claim.
    let dir2 = fresh_session_dir("e2e-2");
    let _g2 = DirGuard(dir2.clone());
    let s2 = Id128::generate();
    let mut session2 = Session::open(SessionConfig {
        dir: dir2.clone(),
        memory_root: Some(shared.clone()),
        project: Some(project_id),
        broker: memory_broker(s2, true),
        session_id: Some(s2),
        ..Default::default()
    })
    .unwrap();
    let (run2, _) = setup_run(&mut session2);
    let out2 = propose_claim(
        &mut session2,
        run2,
        s2,
        json!({"kind": "decision", "content": "the widget is approved"}),
    );
    assert_eq!(out2.result["status"], "approved");
    let claim2_id = out2.result["claim_id"].as_str().unwrap().to_string();
    let claim2_digest = out2.result["claim_digest"].as_str().unwrap().to_string();
    assert!(out2.result["transition_id"].is_string());
    assert_eq!(session2.memory_project().unwrap().transition_count(), 1);
    let evs = envelopes(&dir2);
    assert_eq!(
        evs.iter()
            .filter(|e| e.kind == "memory_transition_backlink")
            .count(),
        1,
        "exactly one backlink for the approved transition"
    );

    let q1 = query_memory(&mut session2, run2, s2, "widget");
    let claims1 = q1.result["claims"].as_array().unwrap();
    assert_eq!(claims1.len(), 1);
    assert_eq!(claims1[0]["text"], "the widget is approved");
    assert_eq!(claims1[0]["status"], "Active");

    // A third claim superseding the second: the second is excluded and
    // annotates the survivor.
    let out3 = propose_claim(
        &mut session2,
        run2,
        s2,
        json!({
            "kind": "decision",
            "content": "the widget is superseding",
            "supersedes": claim2_id,
        }),
    );
    assert_eq!(out3.result["status"], "approved");
    // claim3 + the supersede edge are two transitions (the successor must be
    // committed before the edge pointing at it).
    assert_eq!(session2.memory_project().unwrap().transition_count(), 3);
    let q2 = query_memory(&mut session2, run2, s2, "widget");
    let claims2 = q2.result["claims"].as_array().unwrap();
    assert!(
        claims2
            .iter()
            .all(|c| c["text"] != "the widget is approved"),
        "the superseded claim must be excluded from results"
    );
    let survivor = claims2
        .iter()
        .find(|c| c["text"] == "the widget is superseding")
        .unwrap();
    assert_eq!(survivor["status"], "Active");
    let contradictions = survivor["contradictions"].as_array().unwrap();
    assert_eq!(contradictions.len(), 1);
    assert_eq!(contradictions[0]["digest"], claim2_digest);
    assert_eq!(contradictions[0]["text"], "the widget is approved");
    assert_eq!(contradictions[0]["supersedes"], true);
    session2.close().unwrap();
}

// --- 4. child runs: bounded, canonical, attenuated -------------------------

#[test]
fn child_runs_bounded_and_attenuated() {
    let dir = fresh_session_dir("children");
    let _guard = DirGuard(dir.clone());
    let memory_root = dir.join("memory");
    let session_id = Id128::generate();
    let seed_text = "the seed widget is canonical";

    // Seed the lifetime scope BEFORE the session opens (a standalone actor
    // commit; the session's actor + index pick it up at open).
    let seed_claim = Claim {
        schema: MEMORY_CLAIM_SCHEMA,
        claim_id: Id128::generate(),
        kind: "decision".into(),
        content: seed_text.into(),
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
    {
        let mut actor = MemoryRootActor::open(&memory_root, MemoryScope::Lifetime).unwrap();
        let queue = Arc::new(DurabilityQueue::start("kb-gate-m4-seed"));
        let mut store =
            ObjectStore::open(&memory_root.join("lifetime/objects"), Arc::clone(&queue)).unwrap();
        let d = store.install(&seed_claim.to_canonical_bytes()).unwrap();
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
            decision_digest: Digest::new(b"seed-decision"),
            idempotency_key: IdempotencyKey {
                session: session_id,
                event: 1,
                decision: Digest::new(b"seed-decision"),
            },
        };
        match actor.propose(transition, &[d], &[]).unwrap() {
            TransitionOutcome::Committed { .. } => {}
            other => panic!("seed propose: expected Committed, got {other:?}"),
        }
        actor.flush().unwrap();
    }

    // Children: 1 completes, 2 blows a tiny token budget (Blocked), 3 runs
    // an attenuated memory.query (project scope only — none bound → empty).
    let parent_results: Arc<Mutex<Vec<StepResult>>> = Arc::new(Mutex::new(Vec::new()));
    let child_results: Arc<Mutex<Vec<StepResult>>> = Arc::new(Mutex::new(Vec::new()));
    let child_results_after = Arc::clone(&child_results);
    let mut spawned = 0usize;
    let factory: Box<dyn FnMut() -> Box<dyn CognitionProvider> + Send> = Box::new(move || {
        spawned += 1;
        let commands = match spawned {
            1 => vec![StepCommand::Finish(TerminalOutcome::CompletedGoal)],
            2 => vec![
                StepCommand::ModelCall(ModelCallSpec {
                    rendered_hash: Digest::new(b"child"),
                    max_tokens: None,
                }),
                StepCommand::Finish(TerminalOutcome::CompletedGoal),
            ],
            _ => vec![
                StepCommand::MemoryQuery {
                    query: "seed".into(),
                },
                StepCommand::Finish(TerminalOutcome::CompletedGoal),
            ],
        };
        Box::new(RecordingProvider::new(commands, Arc::clone(&child_results)))
    });
    // child.spawn + memory.query are granted (no approval) to the session
    // principal.
    let mut broker = Broker::new();
    broker
        .add_template(PolicyTemplate {
            trust_class: TrustClass::Builtin,
            allow: vec![
                Capability::new("child.spawn".into(), vec!["call".into()]),
                Capability::new("memory.query".into(), vec!["call".into()]),
            ],
            deny: vec![],
            require_approval: vec![],
            version: 1,
            monotonic: true,
        })
        .unwrap();
    for resource in ["child.spawn", "memory.query"] {
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
            purpose: Some("gate".into()),
            policy_version: 1,
        };
        grant.grant_digest = grant.derive_digest();
        broker.add_grant(grant).unwrap();
    }
    let mut session = Session::open(SessionConfig {
        dir: dir.clone(),
        memory_root: Some(memory_root.clone()),
        provider: Some(fake_config("fake")),
        provider_engine: Some(Box::new(FakeEngine::new(
            fake_config("fake"),
            vec![response("child answer", 10, 10)],
        ))),
        broker,
        session_id: Some(session_id),
        child_provider: Some(factory),
        budgets: kanbei_scheduler::Budgets {
            deadline_secs: Some(60),
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap();
    session.observe_trigger(Trigger {
        kind: TriggerKind::NewCausalEvent,
        referent: None,
    });
    let run = session.accept_wake().unwrap().unwrap();
    session.run_start(run.run_id).unwrap();
    let parent_trigger = run.trigger.clone();
    let parent_run = run.run_id;
    let mut provider = RecordingProvider::new(
        vec![
            StepCommand::ChildSpawn {
                spec: json!({"prompt": "child one", "budgets": {"tokens": 5000}}),
            },
            StepCommand::ChildSpawn {
                spec: json!({"prompt": "child two", "budgets": {"tokens": 5}}),
            },
            StepCommand::ChildSpawn {
                spec: json!({"prompt": "child three"}),
            },
            StepCommand::ToolIntent {
                tool: "memory.query".into(),
                arguments: json!({"query": "seed"}),
            },
            StepCommand::Finish(TerminalOutcome::CompletedGoal),
        ],
        Arc::clone(&parent_results),
    );
    let outcome = session
        .cognition_loop(
            parent_run,
            parent_trigger,
            &mut provider,
            |s: &mut Session| {
                s.project_context(
                    parent_run,
                    &Trigger {
                        kind: TriggerKind::ChildDone,
                        referent: None,
                    },
                )
            },
        )
        .unwrap();
    assert_eq!(outcome, TerminalOutcome::CompletedGoal);

    let results = parent_results.lock().unwrap();
    // Child 1 completed with a canonical run record.
    let child1: Value = match &results[0] {
        StepResult::Child(v) => v.clone(),
        other => panic!("expected Child result, got {other:?}"),
    };
    let child1_id = child1["result"]["run_id"].as_str().unwrap().to_string();
    assert_eq!(child1["result"]["outcome"], "CompletedGoal");
    // Child 2 blew its token budget → Blocked.
    let child2: Value = match &results[1] {
        StepResult::Child(v) => v.clone(),
        other => panic!("expected Child result, got {other:?}"),
    };
    let child2_id = child2["result"]["run_id"].as_str().unwrap().to_string();
    assert_eq!(child2["result"]["outcome"], "Blocked");
    assert_eq!(child2["result"]["usage"]["tokens"], 20);
    // Child 3's query attenuated: no lifetime claims for children.
    let child3: Value = match &results[2] {
        StepResult::Child(v) => v.clone(),
        other => panic!("expected Child result, got {other:?}"),
    };
    let child3_id = child3["result"]["run_id"].as_str().unwrap().to_string();
    assert_eq!(child3["result"]["outcome"], "CompletedGoal");
    // The parent's memory.query sees the lifetime seed.
    let parent_query: Value = match &results[3] {
        StepResult::Tool(v) => v.clone(),
        other => panic!("expected Tool result, got {other:?}"),
    };
    let pclaims = parent_query["result"]["claims"].as_array().unwrap();
    assert!(
        pclaims.iter().any(|c| c["text"] == seed_text),
        "parent query must return the lifetime seed"
    );
    drop(results);

    // The child's Memory result carried no claims (child2 never reached its
    // Finish step — the budget boundary stopped it; its Model result was
    // never fed back).
    let cresults = child_results_after.lock().unwrap();
    assert_eq!(cresults.len(), 1, "child3's memory result only");
    let memory_result = match &cresults[0] {
        StepResult::Memory(v) => v.clone(),
        other => panic!("expected Memory result, got {other:?}"),
    };
    assert_eq!(
        memory_result["result"]["claims"].as_array().unwrap().len(),
        0
    );
    drop(cresults);

    // Every started child reached a terminal outcome; the tiny-budget child
    // recorded Blocked exactly once.
    let evs = envelopes(&dir);
    for id in [&child1_id, &child2_id, &child3_id] {
        let starts = evs
            .iter()
            .filter(|e| e.kind == "run_start" && e.payload["run_id"] == json!(id))
            .count();
        let ends = evs
            .iter()
            .filter(|e| e.kind == "run_outcome" && e.payload["run_id"] == json!(id))
            .count();
        assert_eq!(starts, 1, "child {id}: exactly one run_start");
        assert_eq!(ends, 1, "child {id}: exactly one run_outcome");
    }
    let blocked = evs
        .iter()
        .filter(|e| e.kind == "run_outcome" && e.payload["run_id"] == json!(child2_id))
        .count();
    assert_eq!(
        blocked, 1,
        "the tiny-budget child records exactly one Blocked outcome"
    );
    let blocked_event = evs
        .iter()
        .find(|e| e.kind == "run_outcome" && e.payload["run_id"] == json!(child2_id))
        .unwrap();
    assert_eq!(blocked_event.payload["outcome"], "Blocked");

    // The ChildDone wake is observable after the parent run ends.
    let wake = session
        .accept_wake()
        .unwrap()
        .expect("ChildDone wake accepted");
    assert_eq!(wake.trigger.kind, TriggerKind::ChildDone);
    session.close().unwrap();
}

// --- 5. projection cache plan/outcome + reasoning continuity ---------------

#[test]
fn projection_cache_and_continuity() {
    let dir = fresh_session_dir("cache");
    let _guard = DirGuard(dir.clone());
    let memory_root = dir.join("memory");
    let project_id = Id128::generate();
    let session_id = Id128::generate();
    // A lifetime claim so both roots are pinned (lifetime + project).
    let lifetime_claim = Claim {
        schema: MEMORY_CLAIM_SCHEMA,
        claim_id: Id128::generate(),
        kind: "decision".into(),
        content: "the lifetime baseline".into(),
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
    {
        let mut actor = MemoryRootActor::open(&memory_root, MemoryScope::Lifetime).unwrap();
        let queue = Arc::new(DurabilityQueue::start("kb-gate-m4-cache-seed"));
        let mut store =
            ObjectStore::open(&memory_root.join("lifetime/objects"), Arc::clone(&queue)).unwrap();
        let d = store.install(&lifetime_claim.to_canonical_bytes()).unwrap();
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
            decision_digest: Digest::new(b"cache-seed-decision"),
            idempotency_key: IdempotencyKey {
                session: session_id,
                event: 1,
                decision: Digest::new(b"cache-seed-decision"),
            },
        };
        match actor.propose(transition, &[d], &[]).unwrap() {
            TransitionOutcome::Committed { .. } => {}
            other => panic!("expected Committed, got {other:?}"),
        }
        actor.flush().unwrap();
    }
    let mut session = Session::open(SessionConfig {
        dir: dir.clone(),
        memory_root: Some(memory_root.clone()),
        project: Some(project_id),
        broker: memory_broker(session_id, true),
        provider: Some(fake_config("fake")),
        provider_engine: Some(Box::new(FakeEngine::new(
            fake_config("fake"),
            vec![
                response("one", 3, 2),
                response("two", 3, 2),
                response("three", 3, 2),
            ],
        ))),
        session_id: Some(session_id),
        ..Default::default()
    })
    .unwrap();
    let (run_id, trigger) = setup_run(&mut session);
    // A first approved claim so the project root exists before the calls.
    let out0 = propose_claim(
        &mut session,
        run_id,
        session_id,
        json!({"kind": "decision", "content": "the baseline project claim"}),
    );
    assert_eq!(out0.result["status"], "approved");

    // The staged projection: StablePrefix plan + pinned roots.
    let ctx = session.project_context(run_id, &trigger).unwrap();
    let p0 = session
        .projection_state()
        .cloned()
        .expect("projection materialized");
    assert!(matches!(
        p0.cache_plan,
        kanbei_provider::CachePlan::StablePrefix { .. }
    ));
    assert_eq!(p0.memory_roots.len(), 2, "lifetime + project roots pinned");
    let messages = p0.lowered.clone();
    let rendered = ctx.rendered.clone();

    // Call 1: Miss (nothing cached yet), Broken on the first call.
    session
        .model_call(
            run_id,
            messages.clone(),
            ctx.selected_events.clone(),
            &rendered,
        )
        .unwrap();
    // Call 2 with the same projection: Hit, Continuous.
    session
        .model_call(
            run_id,
            messages.clone(),
            ctx.selected_events.clone(),
            &rendered,
        )
        .unwrap();
    // A new approved claim moves the project root; the stale projection now
    // pins roots that no longer match the live actors → Invalidated.
    let out = propose_claim(
        &mut session,
        run_id,
        session_id,
        json!({"kind": "decision", "content": "the cache-busting claim"}),
    );
    assert_eq!(out.result["status"], "approved");
    session
        .model_call(run_id, messages, ctx.selected_events, &rendered)
        .unwrap();

    let evs = envelopes(&dir);
    let calls: Vec<&kanbei_core::envelope::Envelope> =
        evs.iter().filter(|e| e.kind == "model_call").collect();
    let outcomes: Vec<&kanbei_core::envelope::Envelope> =
        evs.iter().filter(|e| e.kind == "model_outcome").collect();
    assert_eq!(calls.len(), 3);
    assert_eq!(outcomes.len(), 3);
    let plan_digest = match p0.cache_plan {
        kanbei_provider::CachePlan::StablePrefix { digest } => digest.to_string(),
        kanbei_provider::CachePlan::None => panic!("expected StablePrefix"),
    };
    for call in &calls {
        let hashes = call.payload["projection_hashes"].as_array().unwrap();
        assert_eq!(hashes.len(), 1);
        assert_eq!(hashes[0], json!(p0.projection_digest.to_string()));
        assert_eq!(
            call.payload["cache_plan"]["stableprefix"]["digest"],
            json!(plan_digest)
        );
        assert_eq!(
            call.payload["memory_roots"].as_array().unwrap().len(),
            2,
            "lifetime + project roots pinned"
        );
    }
    assert_eq!(outcomes[0].payload["cache_outcome"], json!("miss"));
    assert!(
        outcomes[0].payload["reasoning_continuity"]["Broken"]["from_provider"] == json!("none")
    );
    assert_eq!(outcomes[1].payload["cache_outcome"], json!("hit"));
    assert_eq!(
        outcomes[1].payload["reasoning_continuity"],
        json!("Continuous")
    );
    assert_eq!(
        outcomes[2].payload["cache_outcome"],
        json!({"invalidated": {"reason": "memory root changed"}})
    );
    assert_eq!(
        outcomes[2].payload["reasoning_continuity"],
        json!("Continuous")
    );
    session.close().unwrap();

    // A fresh session with a different provider: the first call is Broken.
    let dir2 = fresh_session_dir("cache-2");
    let _g2 = DirGuard(dir2.clone());
    let session_id2 = Id128::generate();
    let mut session2 = Session::open(SessionConfig {
        dir: dir2.clone(),
        provider: Some(fake_config("fake2")),
        provider_engine: Some(Box::new(FakeEngine::new(
            fake_config("fake2"),
            vec![response("a", 1, 1), response("b", 1, 1)],
        ))),
        session_id: Some(session_id2),
        ..Default::default()
    })
    .unwrap();
    let (run2, trigger2) = setup_run(&mut session2);
    let ctx2 = session2.project_context(run2, &trigger2).unwrap();
    let msgs2 = session2.projection_state().cloned().unwrap().lowered;
    session2
        .model_call(
            run2,
            msgs2.clone(),
            ctx2.selected_events.clone(),
            &ctx2.rendered,
        )
        .unwrap();
    session2
        .model_call(run2, msgs2, ctx2.selected_events, &ctx2.rendered)
        .unwrap();
    let evs2 = envelopes(&dir2);
    let outs2: Vec<&kanbei_core::envelope::Envelope> =
        evs2.iter().filter(|e| e.kind == "model_outcome").collect();
    assert!(outs2[0].payload["reasoning_continuity"]["Broken"]["from_provider"] == json!("none"));
    assert_eq!(
        outs2[1].payload["reasoning_continuity"],
        json!("Continuous")
    );
    session2.close().unwrap();
}

// --- 6. execution manifests pin the memory roots ---------------------------

#[test]
fn manifest_pins_memory_roots() {
    let dir = fresh_session_dir("pins");
    let _guard = DirGuard(dir.clone());
    let memory_root = dir.join("memory");
    let project_id = Id128::generate();
    let session_id = Id128::generate();
    let mut session = Session::open(SessionConfig {
        dir: dir.clone(),
        memory_root: Some(memory_root),
        project: Some(project_id),
        broker: memory_broker(session_id, true),
        session_id: Some(session_id),
        ..Default::default()
    })
    .unwrap();
    let (run_id, _) = setup_run(&mut session);
    let out = propose_claim(
        &mut session,
        run_id,
        session_id,
        json!({"kind": "decision", "content": "the pinned claim"}),
    );
    assert_eq!(out.result["status"], "approved");
    // A state-changing commit materializes the post-snapshot manifest.
    let receipt = session
        .commit(
            vec![NewEvent {
                kind: "test_event".into(),
                payload_schema: 1,
                payload: json!({"pin": true}),
                objects: vec![],
                refs: vec![],
            }],
            Some(Digest::new(b"state-head")),
        )
        .unwrap();
    let post = receipt.post_snapshot.expect("state commit pins a manifest");
    let manifest: ExecutionManifest =
        serde_json::from_slice(&session.store().get(&post).unwrap()).unwrap();
    assert_eq!(manifest.memory_root, session.memory_lifetime().head());
    assert_eq!(
        manifest.project_memory_root,
        session.memory_project().unwrap().head()
    );
    session.close().unwrap();
}

// --- 7. compaction FSM rejects covered fragments ---------------------------

#[test]
fn compaction_fsm_rejects_covered_fragments() {
    let dir = fresh_session_dir("compaction");
    let _guard = DirGuard(dir.clone());
    let mut session = Session::open(SessionConfig {
        dir: dir.clone(),
        ..Default::default()
    })
    .unwrap();
    session
        .commit(
            vec![NewEvent {
                kind: "compaction_selected".into(),
                payload_schema: 1,
                payload: json!({
                    "range": [1, 3],
                    "summary_digest": Digest::new(b"summary").to_string(),
                    "covered_fragments": ["conv.prefix.1.3"],
                }),
                objects: vec![],
                refs: vec![],
            }],
            None,
        )
        .unwrap();
    let err = session
        .commit(
            vec![NewEvent {
                kind: "user_message".into(),
                payload_schema: 1,
                payload: json!({"fragment": "conv.prefix.1.3"}),
                objects: vec![],
                refs: vec![],
            }],
            None,
        )
        .unwrap_err();
    let violation = format!("{err}");
    assert!(
        matches!(&err, SessionError::CompactionViolation(f) if f == "conv.prefix.1.3"),
        "covered fragment must be rejected: {violation}"
    );
    // A different fragment commits fine.
    session
        .commit(
            vec![NewEvent {
                kind: "user_message".into(),
                payload_schema: 1,
                payload: json!({"fragment": "conv.prefix.2.4"}),
                objects: vec![],
                refs: vec![],
            }],
            None,
        )
        .unwrap();
    session.close().unwrap();
    // Recovery re-parses the compaction selection: still rejected.
    let mut session = Session::open(SessionConfig {
        dir: dir.clone(),
        ..Default::default()
    })
    .unwrap();
    let err = session
        .commit(
            vec![NewEvent {
                kind: "user_message".into(),
                payload_schema: 1,
                payload: json!({"fragment": "conv.prefix.1.3"}),
                objects: vec![],
                refs: vec![],
            }],
            None,
        )
        .unwrap_err();
    assert!(matches!(err, SessionError::CompactionViolation(_)));
    session.close().unwrap();
}

// --- 8. consistency 15: the memory substrate leaves scope state intact -----

#[test]
fn consistency_scope_15_unchanged() {
    let dir = fresh_session_dir("scope");
    let _guard = DirGuard(dir.clone());
    let memory_root = dir.join("memory");
    let project_id = Id128::generate();
    let session_id = Id128::generate();
    let mut session = Session::open(SessionConfig {
        dir: dir.clone(),
        memory_root: Some(memory_root),
        project: Some(project_id),
        broker: memory_broker(session_id, true),
        session_id: Some(session_id),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(session.scopes().scopes().len(), 1);
    let before_epoch = session.composition().epoch;
    let (run_id, _) = setup_run(&mut session);
    let out = propose_claim(
        &mut session,
        run_id,
        session_id,
        json!({"kind": "decision", "content": "the scope-safe claim"}),
    );
    assert_eq!(out.result["status"], "approved");
    let q = query_memory(&mut session, run_id, session_id, "scope-safe");
    assert_eq!(q.result["claims"].as_array().unwrap().len(), 1);
    // No scope drift, no composition change — memory is a pure consumer of
    // the composition, never a contributor (R-26).
    assert_eq!(session.scopes().scopes().len(), 1);
    assert_eq!(session.composition().epoch, before_epoch);
    session.close().unwrap();
}

// --- 9. backlink recovery is idempotent across reopens ---------------------

#[test]
fn backlink_idempotent_after_crash() {
    let dir = fresh_session_dir("backlink");
    let _guard = DirGuard(dir.clone());
    let memory_root = dir.join("memory");
    let project_id = Id128::generate();
    let session_id = Id128::generate();
    // Simulate the crash window directly: the transition commits through the
    // actor but no backlink event is ever committed to the session log.
    let claim = Claim {
        schema: MEMORY_CLAIM_SCHEMA,
        claim_id: Id128::generate(),
        kind: "decision".into(),
        content: "the orphaned transition claim".into(),
        owner: Principal {
            session: session_id,
            generation: 0,
            run: None,
        },
        visibility_scope: MemoryScope::Project(project_id),
        provenance: ClaimProvenance::new_ordinary(session_id, 1),
        observed_at: Some(1_700_000_000),
        valid_from: None,
        sensitivity: "public".into(),
    };
    {
        let mut actor =
            MemoryRootActor::open(&memory_root, MemoryScope::Project(project_id)).unwrap();
        let queue = Arc::new(DurabilityQueue::start("kb-gate-m4-backlink"));
        let mut store = ObjectStore::open(
            &memory_root.join(format!("projects/{project_id}/objects")),
            Arc::clone(&queue),
        )
        .unwrap();
        let d = store.install(&claim.to_canonical_bytes()).unwrap();
        store.flush().unwrap();
        drop(store);
        if let Ok(q) = Arc::try_unwrap(queue) {
            let _ = q.shutdown();
        }
        let manifest = RootManifest {
            schema: MEMORY_ROOT_SCHEMA,
            parent: None,
            scope: MemoryScope::Project(project_id),
            added_claims: vec![d],
            added_edges: vec![],
            retracted: vec![],
            transition_id: Id128::generate(),
        };
        let transition = MemoryTransition {
            schema: MEMORY_TRANSITION_SCHEMA,
            transition_id: manifest.transition_id,
            scope: MemoryScope::Project(project_id),
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
            decision_digest: Digest::new(b"backlink-decision"),
            idempotency_key: IdempotencyKey {
                session: session_id,
                event: 1,
                decision: Digest::new(b"backlink-decision"),
            },
        };
        match actor.propose(transition, &[d], &[]).unwrap() {
            TransitionOutcome::Committed { .. } => {}
            other => panic!("expected Committed, got {other:?}"),
        }
        actor.flush().unwrap();
    }
    // The session log has no backlink yet.
    let dir2 = fresh_session_dir("backlink-log");
    let _g2 = DirGuard(dir2.clone());
    let _session = Session::open(SessionConfig {
        dir: dir2.clone(),
        memory_root: Some(memory_root.clone()),
        project: Some(project_id),
        session_id: Some(session_id),
        ..Default::default()
    })
    .unwrap();
    let backlinks = |d: &Path| {
        envelopes(d)
            .iter()
            .filter(|e| e.kind == "memory_transition_backlink")
            .count()
    };
    // The first open's recovery committed exactly one backlink; reopens add
    // none (idempotent by TransitionId).
    assert_eq!(backlinks(&dir2), 1);
    for _ in 0..2 {
        let s = Session::open(SessionConfig {
            dir: dir2.clone(),
            memory_root: Some(memory_root.clone()),
            project: Some(project_id),
            session_id: Some(session_id),
            ..Default::default()
        })
        .unwrap();
        s.close().unwrap();
    }
    assert_eq!(backlinks(&dir2), 1);
}

// --- 10. T16: promotion write path, root review, layer-2 active view --------

/// A broker granting exactly `allow`, requiring approval for exactly
/// `require` — the T16 tools are not in the M4 baseline broker.
fn tool_broker(session_id: Id128, allow: &[&str], require: &[&str]) -> Broker {
    let call = |t: &str| Capability::new(t.into(), vec!["call".into()]);
    let mut broker = Broker::new();
    broker
        .add_template(PolicyTemplate {
            trust_class: TrustClass::Builtin,
            allow: allow.iter().map(|t| call(t)).collect(),
            deny: vec![],
            require_approval: require.iter().map(|t| call(t)).collect(),
            version: 1,
            monotonic: true,
        })
        .unwrap();
    for resource in allow {
        let mut grant = Grant {
            grant_digest: Digest::new(b"placeholder"),
            principal: Principal {
                session: session_id,
                generation: 0,
                run: None,
            },
            module_generation: 0,
            capability: call(resource),
            scope: GrantScope::Session,
            expiry: None,
            budget: None,
            purpose: Some("gate".into()),
            policy_version: 1,
        };
        grant.grant_digest = grant.derive_digest();
        broker.add_grant(grant).unwrap();
    }
    broker
}

/// One memory.review round trip; the harness resolves a park.
fn review_claim(
    session: &mut Session,
    run_id: kanbei_scheduler::RunId,
    session_id: Id128,
    claim_id: &str,
    decision: &str,
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
            "memory.review",
            json!({ "claim_id": claim_id, "decision": decision, "reason": "root review" }),
        )
        .unwrap();
    if outcome.awaiting_approval() {
        let digest = *session
            .pending_approvals()
            .last()
            .expect("a parked review approval");
        return session
            .resolve_approval(&digest, true)
            .unwrap()
            .expect("review approval resolves");
    }
    session.commit_tool_outcome(&outcome).unwrap();
    outcome
}

/// One memory.promote round trip; the harness resolves a park.
fn promote_claim(
    session: &mut Session,
    run_id: kanbei_scheduler::RunId,
    session_id: Id128,
    claim_id: &str,
    evidence: &str,
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
            "memory.promote",
            json!({ "claim_id": claim_id, "evidence": evidence }),
        )
        .unwrap();
    if outcome.awaiting_approval() {
        let digest = *session
            .pending_approvals()
            .last()
            .expect("a parked promotion approval");
        return session
            .resolve_approval(&digest, true)
            .unwrap()
            .expect("promotion approval resolves");
    }
    session.commit_tool_outcome(&outcome).unwrap();
    outcome
}

/// E-F4: promotion is a real write path — a `Promotion` transition on the
/// LIFETIME scope, a `promoted_from` edge whose target is the project source,
/// the destination claim's provenance carrying the source digest, and the
/// session backlink naming the lifetime scope.
#[test]
fn promotion_write_path_lifetime_root_and_promoted_from_edge() {
    let root = fresh_session_dir("promotion");
    let _guard = DirGuard(root.clone());
    let project_id = Id128::generate();
    let sid = Id128::generate();
    let mut session = Session::open(SessionConfig {
        dir: root.clone(),
        memory_root: Some(root.join("memory")),
        project: Some(project_id),
        broker: tool_broker(
            sid,
            &["memory.propose", "memory.query", "memory.promote"],
            &["memory.propose", "memory.promote"],
        ),
        session_id: Some(sid),
        ..Default::default()
    })
    .unwrap();
    let (run, _) = setup_run(&mut session);
    let proposed = propose_claim(
        &mut session,
        run,
        sid,
        json!({"kind": "decision", "content": "the widget is canonical"}),
    );
    assert_eq!(proposed.result["status"], "approved");
    let source_id = proposed.result["claim_id"].as_str().unwrap().to_string();
    let source_digest = proposed.result["claim_digest"].as_str().unwrap().to_string();

    let promoted = promote_claim(
        &mut session,
        run,
        sid,
        &source_id,
        "the project held the evidence",
    );
    assert_eq!(promoted.result["status"], "approved", "{promoted:?}");
    assert_eq!(promoted.result["source_claim_id"], source_id);
    assert_eq!(promoted.result["source_digest"], source_digest);
    assert!(
        promoted.result["transition_id"].is_string(),
        "a lifetime transition commits"
    );

    // Lifetime root updated: one Promotion transition, its fold carries the
    // destination claim and the promoted_from edge to the project source id.
    let actor = session.memory_lifetime();
    assert_eq!(actor.transition_count(), 1);
    let fold = actor.fold(actor.head()).unwrap();
    assert_eq!(fold.claims.len(), 1);
    let (_, dest) = &fold.claims[0];
    assert_eq!(dest.visibility_scope, MemoryScope::Lifetime);
    assert_eq!(dest.provenance.source_claims.len(), 1);
    assert_eq!(dest.provenance.source_claims[0].to_string(), source_digest);
    assert_eq!(dest.content, "the widget is canonical");
    assert_eq!(fold.edges.len(), 1);
    let (_, edge) = &fold.edges[0];
    assert_eq!(edge.kind, EdgeKind::PromotedFrom);
    assert_eq!(edge.from, dest.claim_id);
    assert_eq!(edge.to.unwrap().to_string(), source_id);

    let evs = envelopes(&root);
    assert!(
        evs.iter().any(|e| e.kind == "memory_promotion_approved"),
        "the promotion origin fact is canonical"
    );
    let backlink = evs
        .iter()
        .find(|e| {
            e.kind == "memory_transition_backlink"
                && e.payload.get("scope").and_then(|s| s.as_str()) == Some("Lifetime")
        })
        .expect("a lifetime backlink is recorded");
    assert!(backlink.payload["transition_id"].is_string());
    session.close().unwrap();
}

/// E-F4 root-agent review: reject and request_evidence are canonical decision
/// facts that leave the fold untouched; approve commits the root transition
/// under the user approval and the claim becomes active.
#[test]
fn root_agent_review_reject_request_evidence_approve() {
    let root = fresh_session_dir("review");
    let _guard = DirGuard(root.clone());
    let project_id = Id128::generate();
    let sid = Id128::generate();
    let mut session = Session::open(SessionConfig {
        dir: root.clone(),
        memory_root: Some(root.join("memory")),
        project: Some(project_id),
        broker: tool_broker(
            sid,
            &["memory.propose", "memory.review"],
            &["memory.review"],
        ),
        session_id: Some(sid),
        ..Default::default()
    })
    .unwrap();
    let (run, _) = setup_run(&mut session);

    // No approval requirement on propose (the broker requires it for review
    // only): the claim stays proposed.
    let first = propose_claim(
        &mut session,
        run,
        sid,
        json!({"kind": "decision", "content": "rejected widget"}),
    );
    assert_eq!(first.result["status"], "proposed");
    let rejected_id = first.result["claim_id"].as_str().unwrap().to_string();
    let rejected = review_claim(&mut session, run, sid, &rejected_id, "reject");
    assert_eq!(rejected.result["decision"], "reject");
    assert_eq!(session.memory_project().unwrap().transition_count(), 0);
    assert!(
        session
            .memory_project()
            .unwrap()
            .fold(None)
            .unwrap()
            .claims
            .is_empty(),
        "a rejected proposal never enters the fold"
    );

    let second = propose_claim(
        &mut session,
        run,
        sid,
        json!({"kind": "decision", "content": "needs evidence"}),
    );
    let needs_id = second.result["claim_id"].as_str().unwrap().to_string();
    let evidence = review_claim(&mut session, run, sid, &needs_id, "request_evidence");
    assert_eq!(evidence.result["decision"], "request_evidence");
    assert_eq!(session.memory_project().unwrap().transition_count(), 0);

    let third = propose_claim(
        &mut session,
        run,
        sid,
        json!({"kind": "decision", "content": "approved widget"}),
    );
    let approved_id = third.result["claim_id"].as_str().unwrap().to_string();
    let approved = review_claim(&mut session, run, sid, &approved_id, "approve");
    assert_eq!(approved.result["status"], "approved");
    assert!(approved.result["transition_id"].is_string());
    assert_eq!(session.memory_project().unwrap().transition_count(), 1);
    let fold = session
        .memory_project()
        .unwrap()
        .fold(session.memory_project().unwrap().head())
        .unwrap();
    assert_eq!(fold.claims.len(), 1, "only the approved claim is active");
    assert_eq!(fold.claims[0].1.content, "approved widget");

    let evs = envelopes(&root);
    assert!(evs.iter().any(|e| e.kind == "memory_proposal_rejected"));
    assert!(evs.iter().any(|e| e.kind == "memory_evidence_requested"));
    session.close().unwrap();
}

/// E-F8: the layer-2 active view is derived from canonical facts and reaches
/// the model-call projection — a queried claim pins, a user message opens a
/// loop, and both render in the active-memory fragment so the salience
/// goals/pins components are live rather than permanently zero.
#[test]
fn layer2_pins_and_open_loops_reach_the_model_call_boundary() {
    let root = fresh_session_dir("layer2");
    let _guard = DirGuard(root.clone());
    let project_id = Id128::generate();
    let sid = Id128::generate();
    let mut session = Session::open(SessionConfig {
        dir: root.clone(),
        memory_root: Some(root.join("memory")),
        project: Some(project_id),
        broker: tool_broker(
            sid,
            &["memory.propose", "memory.query"],
            &["memory.propose"],
        ),
        session_id: Some(sid),
        ..Default::default()
    })
    .unwrap();
    let (run, trigger) = setup_run(&mut session);
    let proposed = propose_claim(
        &mut session,
        run,
        sid,
        json!({"kind": "decision", "content": "the widget is canonical"}),
    );
    let digest = proposed.result["claim_digest"].as_str().unwrap().to_string();

    let q = query_memory(&mut session, run, sid, "widget");
    assert_eq!(q.result["claims"].as_array().unwrap().len(), 1);
    assert_eq!(session.active_pins().len(), 1, "the query pins its claims");
    assert_eq!(session.active_pins()[0].to_string(), digest);

    session
        .append_user_message("please fix the widget")
        .unwrap();
    assert_eq!(session.open_loops().len(), 1);
    assert_eq!(session.open_loops()[0].text, "please fix the widget");

    let ctx = session.project_context(run, &trigger).unwrap();
    assert!(
        ctx.rendered.contains(&format!("pin: {digest}")),
        "the pinned claim renders in the active fragment: {}",
        ctx.rendered
    );
    assert!(
        ctx.rendered.contains("open_loop:") && ctx.rendered.contains("please fix the widget"),
        "the open loop renders in the active fragment: {}",
        ctx.rendered
    );

    // A completed run resolves the loop; the pin survives (it is query state).
    session
        .run_outcome(
            run,
            TerminalOutcome::CompletedGoal,
            session.scheduler_usage(run),
            &[],
        )
        .unwrap();
    assert!(
        session.open_loops().is_empty(),
        "CompletedGoal resolves loops"
    );
    assert_eq!(session.active_pins().len(), 1);
    session.close().unwrap();

    // Layer-2 is per-run disposable (R-12/F-S5): a reopened session starts
    // with an empty active view and re-derives it from the new run's facts.
    let mut reopened = Session::open(SessionConfig {
        dir: root.clone(),
        memory_root: Some(root.join("memory")),
        project: Some(project_id),
        broker: tool_broker(
            sid,
            &["memory.propose", "memory.query"],
            &["memory.propose"],
        ),
        session_id: Some(sid),
        ..Default::default()
    })
    .unwrap();
    assert!(
        reopened.active_pins().is_empty(),
        "the previous run's pins are not carried into the new run"
    );
    assert!(reopened.open_loops().is_empty());
    // A later message opens a fresh loop after resume.
    reopened.append_user_message("and then verify it").unwrap();
    assert_eq!(reopened.open_loops().len(), 1);
    reopened.close().unwrap();
}
