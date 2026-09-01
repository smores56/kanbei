//! M7 memory-usefulness probes (docs/memory-probes.md): the dogfooding
//! instrument applied to the M4 memory substrate. Every probe runs against
//! the fake engine and the `MemoryRootActor` — no live provider. Thresholds
//! are the M7 tuning values; `probes_verdict` folds them into one bool.
//! Each probe logs its raw numbers to stdout for the milestone report.

#![allow(clippy::result_large_err)]

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kanbei_capabilities::{
    Broker, Capability, Grant, GrantScope, PolicyTemplate, Principal, TrustClass,
};
use kanbei_core::digest::Digest;
use kanbei_core::id::Id128;
use kanbei_core::queue::DurabilityQueue;
use kanbei_memory::{
    Claim, ClaimEdge, ClaimProvenance, EdgeKind, IdempotencyKey, MEMORY_CLAIM_SCHEMA,
    MEMORY_ROOT_SCHEMA, MEMORY_TRANSITION_SCHEMA, MemoryRootActor, MemoryScope,
    MemoryTransition, RootManifest, TransitionKind, TransitionOutcome,
};
use kanbei_objects::ObjectStore;
use kanbei_provider::{
    CompletionResponse, FakeEngine, FinishReason, KeySource, ProviderConfig, Usage,
};
use kanbei_scheduler::{
    Budgets, CognitionProvider, ModelCallSpec, StepCommand, StepContext, StepError, StepResult,
    TerminalOutcome, Trigger, TriggerKind,
};
use kanbei_session::{NewEvent, Session, SessionConfig};
use serde_json::{Value, json};

use crate::fixture::{python_path, tool_env};

// ---------- shared helpers ----------

fn fresh_dir(root: &Path, tag: &str) -> PathBuf {
    let d = root.join(format!(
        "probe-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn fake_config(provider: &str) -> ProviderConfig {
    ProviderConfig {
        provider: provider.into(),
        model: "probe".into(),
        base_url: "http://localhost:0/v1".into(),
        key: KeySource::Env("KANBEI_TEST_KEY".into()),
        temperature: None,
        max_tokens: Some(10),
        timeout: Duration::from_secs(5),
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

/// A broker granting the listed tools to the session principal; when
/// `require_propose_approval`, `memory.propose` transitions the project root
/// only under a parked approval (the gate_m4/dogfood FSM contract).
fn probe_broker(session_id: Id128, tools: &[&str], require_propose_approval: bool) -> Broker {
    let mut broker = Broker::new();
    broker
        .add_template(PolicyTemplate {
            trust_class: TrustClass::Builtin,
            allow: tools
                .iter()
                .map(|r| Capability::new((*r).into(), vec!["call".into()]))
                .collect(),
            deny: vec![],
            require_approval: if require_propose_approval {
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
    for resource in tools {
        let mut grant = Grant {
            grant_digest: Digest::new(b"placeholder"),
            principal: Principal {
                session: session_id,
                generation: 0,
                run: None,
            },
            module_generation: 0,
            capability: Capability::new((*resource).into(), vec!["call".into()]),
            scope: GrantScope::Session,
            expiry: None,
            budget: None,
            purpose: Some("m7-probes".into()),
            policy_version: 1,
        };
        grant.grant_digest = grant.derive_digest();
        broker.add_grant(grant).unwrap();
    }
    broker
}

fn open_session(
    dir: &Path,
    memory_root: Option<&Path>,
    project: Option<Id128>,
    session_id: Id128,
    broker: Broker,
    responses: Vec<CompletionResponse>,
    child_provider: Option<Box<dyn FnMut() -> Box<dyn CognitionProvider> + Send>>,
) -> Session {
    Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        provider: Some(fake_config("probe")),
        provider_engine: if responses.is_empty() {
            None
        } else {
            Some(Box::new(FakeEngine::new(fake_config("probe"), responses)))
        },
        broker,
        // the probe plays the user: parked approvals resolve so the
        // scripted loop proceeds (writing fidelity, not approval UX)
        approval_resolver: Some(std::sync::Arc::new(|_| true)),
        session_id: Some(session_id),
        project,
        memory_root: memory_root.map(|p| p.to_path_buf()),
        fs_root: dir.to_path_buf(),
        budgets: Budgets {
            deadline_secs: Some(120),
            tokens: Some(100_000),
            tools: Some(200),
            children: Some(8),
        },
        child_provider,
        ..Default::default()
    })
    .unwrap()
}

fn principal(session_id: Id128) -> Principal {
    Principal {
        session: session_id,
        generation: 0,
        run: Some(0),
    }
}

/// One memory.propose tool round trip (intent + outcome committed).
fn propose_claim(
    session: &mut Session,
    run_id: kanbei_scheduler::RunId,
    session_id: Id128,
    claim: Value,
) -> kanbei_tools::ToolOutcome {
    let outcome = session
        .tool_call(run_id, principal(session_id), "memory.propose", json!({ "claim": claim }))
        .unwrap();
    if outcome.awaiting_approval() {
        // the probe plays the user on the direct path too
        let digest = *session.pending_approvals().last().expect("parked");
        return session
            .resolve_approval(&digest, true)
            .unwrap()
            .expect("approval resolves");
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
    let outcome = session
        .tool_call(run_id, principal(session_id), "memory.query", json!({ "query": query }))
        .unwrap();
    session.commit_tool_outcome(&outcome).unwrap();
    outcome
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

struct ScriptedProvider {
    commands: VecDeque<StepCommand>,
}

impl ScriptedProvider {
    fn new(commands: Vec<StepCommand>) -> Self {
        Self {
            commands: commands.into(),
        }
    }
}

impl CognitionProvider for ScriptedProvider {
    fn step(
        &mut self,
        _context: &StepContext,
        _trigger: &Trigger,
        _last: Option<&StepResult>,
    ) -> Result<StepCommand, StepError> {
        self.commands
            .pop_front()
            .ok_or(StepError::Invalid("no more commands".into()))
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// The gate_m4 recording provider: pushes the previous step result at every
/// step, so results[i] is the outcome of command i.
struct RecordingProvider {
    commands: VecDeque<StepCommand>,
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
        if let Some(last) = last {
            self.results.lock().unwrap().push(last.clone());
        }
        self.commands
            .pop_front()
            .ok_or(StepError::Invalid("no more commands".into()))
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// One accepted wake through the cognition loop, returning the rendered
/// projection text alongside the terminal outcome.
fn run_wake_capture(session: &mut Session, plan: Vec<StepCommand>) -> (TerminalOutcome, String) {
    session.observe_trigger(Trigger {
        kind: TriggerKind::NewCausalEvent,
        referent: None,
    });
    let run = session.accept_wake().unwrap().expect("wake accepted");
    let trigger = run.trigger.clone();
    let run_id = run.run_id;
    session.run_start(run_id).unwrap();
    let rendered = std::cell::RefCell::new(String::new());
    let mut provider = ScriptedProvider::new(plan);
    let outcome = session
        .cognition_loop(run_id, trigger.clone(), &mut provider, |s| {
            let ctx = s.project_context(run_id, &trigger)?;
            *rendered.borrow_mut() = ctx.rendered.clone();
            Ok(ctx)
        })
        .unwrap();
    (outcome, rendered.into_inner())
}

// ---------- actor seeding (the m6/gate_m4 pattern, project scope) ----------

fn make_claim(
    session_id: Id128,
    event: u64,
    kind: &str,
    content: &str,
    scope: MemoryScope,
) -> Claim {
    Claim {
        schema: MEMORY_CLAIM_SCHEMA,
        claim_id: Id128::generate(),
        kind: kind.into(),
        content: content.into(),
        owner: Principal {
            session: session_id,
            generation: 0,
            run: None,
        },
        visibility_scope: scope,
        provenance: ClaimProvenance::new_ordinary(session_id, event),
        observed_at: Some(1_700_000_000),
        valid_from: None,
        sensitivity: "public".into(),
    }
}

fn make_edge(
    from: Id128,
    to: Option<Id128>,
    kind: EdgeKind,
    session_id: Id128,
    event: u64,
) -> ClaimEdge {
    ClaimEdge::new(from, to, kind, Vec::new(), ClaimProvenance::new_ordinary(session_id, event))
        .unwrap()
}

static SEED_QUEUE: AtomicUsize = AtomicUsize::new(0);

/// Commits one project-scope root transition through a standalone actor (the
/// gate_m4 seeding pattern): installs the claim/edge objects, then proposes
/// the manifest the actor derives. Supersedes edges retract their `from`
/// claims exactly like the session's phase-2 transition.
fn seed_transition(
    memory_root: &Path,
    project_id: Id128,
    session_id: Id128,
    event: u64,
    claims: &[Claim],
    edges: &[ClaimEdge],
    tag: &[u8],
) -> Digest {
    let scope = MemoryScope::Project(project_id);
    let mut actor = MemoryRootActor::open(memory_root, scope.clone()).unwrap();
    let queue = Arc::new(DurabilityQueue::start(&format!(
        "kb-probe-seed-{}",
        SEED_QUEUE.fetch_add(1, Ordering::Relaxed)
    )));
    let objects_dir = memory_root.join(scope.dir_name()).join("objects");
    let mut store = ObjectStore::open(&objects_dir, Arc::clone(&queue)).unwrap();
    let claim_digests: Vec<Digest> = claims
        .iter()
        .map(|c| store.install(&c.to_canonical_bytes()).unwrap())
        .collect();
    let edge_digests: Vec<Digest> = edges
        .iter()
        .map(|e| store.install(&e.to_canonical_bytes()).unwrap())
        .collect();
    store.flush().unwrap();
    drop(store);
    if let Ok(q) = Arc::try_unwrap(queue) {
        let _ = q.shutdown();
    }
    let fold = actor.fold(actor.head()).unwrap();
    let mut committed: HashMap<Id128, Digest> = HashMap::new();
    for (d, c) in fold.claims.iter().chain(fold.retracted.iter()) {
        committed.insert(c.claim_id, *d);
    }
    for c in claims {
        committed.insert(c.claim_id, c.digest());
    }
    let mut retracted: Vec<Digest> = edges
        .iter()
        .filter(|e| e.kind == EdgeKind::Supersedes)
        .filter_map(|e| committed.get(&e.from).copied())
        .collect();
    retracted.sort_unstable();
    retracted.dedup();
    let manifest = RootManifest {
        schema: MEMORY_ROOT_SCHEMA,
        parent: actor.head(),
        scope: scope.clone(),
        added_claims: claim_digests.clone(),
        added_edges: edge_digests.clone(),
        retracted,
        transition_id: Id128::generate(),
    };
    let transition = MemoryTransition {
        schema: MEMORY_TRANSITION_SCHEMA,
        transition_id: manifest.transition_id,
        scope: scope.clone(),
        kind: TransitionKind::RootApproval,
        expected_old_root: actor.head(),
        accepted_new_root: manifest.digest(),
        origin_session: session_id,
        origin_event: event,
        origin_kind: "memory_root_approved".into(),
        decision_principal: Principal {
            session: session_id,
            generation: 0,
            run: None,
        },
        decision_digest: Digest::new(tag),
        idempotency_key: IdempotencyKey {
            session: session_id,
            event,
            decision: Digest::new(tag),
        },
    };
    match actor.propose(transition, &claim_digests, &edge_digests).unwrap() {
        TransitionOutcome::Committed { new_root, .. } => new_root,
        other => panic!("seed propose: expected Committed, got {other:?}"),
    }
}

fn percentile(ms: &mut [f64], p: f64) -> f64 {
    ms.sort_by(|a, b| a.total_cmp(b));
    let idx = ((ms.len() as f64 * p).ceil() as usize).saturating_sub(1);
    ms[idx.min(ms.len() - 1)]
}

// ---------- the R1 seed fold (50 claims, 3 contradictions, 2 supersessions) ----------

const R1_CLAIMS: [&str; 50] = [
    "the order service stores orders in postgres",
    "the order service listens on port 8080",
    "the order service logs to stdout in json",
    "the order service logs to stdout in plain text",
    "the order service validates email addresses with a regex",
    "the checkout flow requires a shipping address",
    "the checkout flow applies coupon discounts before taxes",
    "the checkout flow emails a receipt after payment",
    "the checkout flow reserves inventory for 15 minutes",
    "the checkout flow reserves inventory for 30 minutes",
    "the payments gateway retries failed charges twice",
    "the payments gateway stores the provider token in vault",
    "the payments gateway refunds only completed charges",
    "the inventory service decrements stock on order placement",
    "the inventory service holds a per-sku lock during decrement",
    "the shipping label printer renders a4 pdfs",
    "the shipping label printer falls back to thermal if the pdf fails",
    "the shipping label printer caches label templates for a day",
    "the auth service issues jwt tokens with a 1 hour expiry",
    "the auth service rotates signing keys every 30 days",
    "the auth service locks an account after 5 failed attempts",
    "the auth service stores password hashes with argon2id",
    "the auth service requires mfa for admin roles",
    "the notification worker sends emails through mailgun",
    "the notification worker dedupes alerts for an hour",
    "the notification worker batches push notifications every minute",
    "the notification worker drops alerts for muted projects",
    "the search index rebuilds every night at 3am",
    "the search index ranks results by recency and relevance",
    "the search index stores documents in a separate shard",
    "the search index reindexes documents on a content change",
    "the feature flags are cached for 60 seconds",
    "the feature flags fall back to defaults when the cache misses",
    "the feature flags allow gradual rollout by user id hash",
    "the api gateway rate limits per api key at 100 requests a minute",
    "the api gateway retries idempotent requests on 502s",
    "the api gateway strips the internal auth header before upstream",
    "the api gateway logs each request with a trace id",
    "the telemetry exporter samples traces at 10 percent",
    "the telemetry exporter batches spans every 5 seconds",
    "the telemetry exporter batches spans every 15 seconds",
    "the telemetry exporter tags every span with the deployment id",
    "the telemetry exporter drops spans without a parent",
    "the backup job runs every 6 hours and keeps 14 snapshots",
    "the backup job encrypts snapshots with a kms key",
    "the backup job verifies the restore path monthly",
    "the deployment pipeline builds on every push to main",
    "the backup job verifies the restore path weekly",
    "the deployment pipeline requires a signed approval for production",
    "the deployment pipeline promotes builds through staging",
];

/// (query, expected claim index) — hand-labeled relevance; every token of
/// every query appears verbatim in its target claim (the FTS step ANDs
/// tokens), and at least one token is unique to the target.
const R1_QUERIES: [(&str, usize); 20] = [
    ("postgres order stores", 0),
    ("checkout coupon discounts taxes", 6),
    ("inventory lock per sku", 14),
    ("label pdf thermal", 16),
    ("jwt expiry hour tokens", 18),
    ("argon2id password hashes", 21),
    ("mailgun emails", 23),
    ("feature flags cached 60 seconds", 31),
    ("api key rate limits 100 requests", 34),
    ("telemetry samples traces 10 percent", 38),
    ("backup kms snapshots", 44),
    ("signed production approval", 48),
    ("account failed attempts locks", 20),
    ("muted alerts notification drops", 26),
    ("search index shard documents", 29),
    ("internal auth header upstream", 36),
    ("retries failed charges twice", 10),
    ("push notifications every minute", 25),
    ("promotes staging pipeline", 49),
    ("backup restore path weekly", 47),
];

/// The superseded pair R2 targets: 45 (superseded) -> 47 (survivor). Every
/// token also matches the survivor, so the validity filter leaves it as the
/// best candidate carrying the supersedes annotation.
const R2_QUERY: &str = "backup restore path";

/// Seeds the R1 fold: claims 0..46 in one transition, the three survivors
/// (47/48/49) next, then the 3 contradiction + 2 supersede edges. Returns
/// the claims in seed order.
fn seed_r1_fold(
    memory_root: &Path,
    project_id: Id128,
    session_id: Id128,
) -> Vec<Claim> {
    let claims: Vec<Claim> = R1_CLAIMS
        .iter()
        .map(|c| {
            make_claim(
                session_id,
                1,
                "decision",
                c,
                MemoryScope::Project(project_id),
            )
        })
        .collect();
    seed_transition(
        memory_root,
        project_id,
        session_id,
        1,
        &claims[..47],
        &[],
        b"r1-t1",
    );
    seed_transition(
        memory_root,
        project_id,
        session_id,
        2,
        &claims[47..],
        &[],
        b"r1-t2",
    );
    let edges = vec![
        make_edge(
            claims[2].claim_id,
            Some(claims[3].claim_id),
            EdgeKind::Contradicts,
            session_id,
            3,
        ),
        make_edge(
            claims[8].claim_id,
            Some(claims[9].claim_id),
            EdgeKind::Contradicts,
            session_id,
            3,
        ),
        make_edge(
            claims[39].claim_id,
            Some(claims[40].claim_id),
            EdgeKind::Contradicts,
            session_id,
            3,
        ),
        make_edge(
            claims[45].claim_id,
            Some(claims[47].claim_id),
            EdgeKind::Supersedes,
            session_id,
            3,
        ),
        make_edge(
            claims[46].claim_id,
            Some(claims[49].claim_id),
            EdgeKind::Supersedes,
            session_id,
            3,
        ),
    ];
    seed_transition(memory_root, project_id, session_id, 3, &[], &edges, b"r1-t3");
    claims
}

// ---------- probe implementations ----------

/// W1: 10 scripted fix sessions; after each the scripted agent proposes 3
/// claims (gold root cause + gold fix + one decoy). Precision = fraction of
/// proposed claims whose content matches a gold string.
fn probe_w1(root: &Path) -> (usize, usize) {
    const DECOY: &str = "the test suite could cover more edge cases";
    let cases = w1_cases();
    let mut proposed = 0usize;
    let mut matched = 0usize;
    for (i, case) in cases.iter().enumerate() {
        let dir = fresh_dir(root, &format!("w1-{i}"));
        std::fs::write(dir.join("fixlib.py"), case.buggy).unwrap();
        std::fs::write(dir.join("test_fixlib.py"), case.test.as_str()).unwrap();
        let session_id = Id128::generate();
        let project = Id128::generate();
        let mut session = open_session(
            &dir,
            None,
            Some(project),
            session_id,
            probe_broker(
                session_id,
                &[
                    "fs.read",
                    "fs.patch",
                    "process.exec",
                    "memory.query",
                    "memory.propose",
                ],
                true,
            ),
            vec![],
            None,
        );
        let plan = vec![
            StepCommand::ToolIntent {
                tool: "fs.read".into(),
                arguments: json!({"path": "test_fixlib.py"}),
            },
            StepCommand::ToolIntent {
                tool: "fs.read".into(),
                arguments: json!({"path": "fixlib.py"}),
            },
            StepCommand::ToolIntent {
                tool: "fs.patch".into(),
                arguments: json!({"path": "fixlib.py", "replacements": [
                    {"old": case.old, "new": case.new}
                ]}),
            },
            StepCommand::ToolIntent {
                tool: "process.exec".into(),
                arguments: json!({
                    "argv": [python_path(), "-m", "unittest", "test_fixlib"],
                    "cwd": ".", "env": tool_env(),
                }),
            },
            StepCommand::MemoryPropose {
                claim: json!({"kind": "lesson", "content": case.root_cause}),
            },
            StepCommand::MemoryPropose {
                claim: json!({"kind": "decision", "content": case.fix}),
            },
            StepCommand::MemoryPropose {
                claim: json!({"kind": "preference", "content": DECOY}),
            },
            StepCommand::Finish(TerminalOutcome::CompletedGoal),
        ];
        let outcome = run_wake_capture(&mut session, plan).0;
        assert_eq!(outcome, TerminalOutcome::CompletedGoal, "w1 session {i}");
        session.close().unwrap();
        let gold = [case.root_cause, case.fix];
        for content in [case.root_cause, case.fix, DECOY] {
            proposed += 1;
            if gold.contains(&content) {
                matched += 1;
            }
        }
    }
    println!(
        "[probe W1] sessions=10 proposed={proposed} matched={matched} precision={:.3} (threshold >= 0.5)",
        matched as f64 / proposed as f64
    );
    (proposed, matched)
}

/// W2: 10 `memory.propose` calls through the session tool FSM (broker
/// require_approval + approval_bound > 0); every 2nd proposal supersedes the
/// previous claim (the two-transition path). Fraction reaching `approved`.
fn probe_w2(root: &Path) -> (usize, usize) {
    let dir = fresh_dir(root, "w2");
    let session_id = Id128::generate();
    let project = Id128::generate();
    let mut session = open_session(
        &dir,
        None,
        Some(project),
        session_id,
        probe_broker(session_id, &["memory.query", "memory.propose"], true),
        vec![],
        None,
    );
    let (run_id, trigger) = setup_run(&mut session);
    let mut agent = ProposeAgent::new(10, 2, Arc::new(Mutex::new(Vec::new())));
    let outcome = session
        .cognition_loop(run_id, trigger.clone(), &mut agent, |s| {
            s.project_context(run_id, &trigger)
        })
        .unwrap();
    assert_eq!(outcome, TerminalOutcome::CompletedGoal);
    let results = agent.results.lock().unwrap();
    let approved = results
        .iter()
        .filter(|v| v["result"]["status"] == "approved")
        .count();
    let total = results.len();
    drop(results);
    session.close().unwrap();
    println!(
        "[probe W2] proposes={total} approved={approved} rate={:.3} (threshold >= 0.9)",
        approved as f64 / total as f64
    );
    (total, approved)
}

/// A scripted cognition provider issuing memory.propose commands; every
/// `supersede_every`-th proposal supersedes the previous claim (the claim id
/// arrives in the previous step result, so the FSM is the same one dogfood
/// drives).
struct ProposeAgent {
    items: VecDeque<(String, String, bool)>,
    last_claim_id: Option<String>,
    results: Arc<Mutex<Vec<Value>>>,
}

impl ProposeAgent {
    fn new(count: usize, supersede_every: usize, results: Arc<Mutex<Vec<Value>>>) -> Self {
        let mut items = VecDeque::new();
        for i in 0..count {
            let supersede = supersede_every > 0 && i > 0 && i % supersede_every == 0;
            items.push_back((
                "decision".into(),
                format!("the w2 proposal number {i}"),
                supersede,
            ));
        }
        Self {
            items,
            last_claim_id: None,
            results,
        }
    }
}

impl CognitionProvider for ProposeAgent {
    fn step(
        &mut self,
        _context: &StepContext,
        _trigger: &Trigger,
        last: Option<&StepResult>,
    ) -> Result<StepCommand, StepError> {
        if let Some(StepResult::Memory(v)) = last {
            self.results.lock().unwrap().push(v.clone());
            if let Some(id) = v["result"]["claim_id"].as_str() {
                self.last_claim_id = Some(id.to_string());
            }
        }
        let Some((kind, content, supersede)) = self.items.pop_front() else {
            return Ok(StepCommand::Finish(TerminalOutcome::CompletedGoal));
        };
        let mut claim = json!({"kind": kind, "content": content});
        if supersede {
            let id = self
                .last_claim_id
                .clone()
                .expect("supersede target from the previous propose");
            claim["supersedes"] = json!(id);
        }
        Ok(StepCommand::MemoryPropose { claim })
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

/// R1 + R2 + T2: 20 scripted queries over the seeded 50-claim fold
/// (recall@5), the superseded-best-match survivor check, and the returned-
/// claim age distribution.
fn probe_r1_r2_t2(root: &Path) -> (usize, usize, bool, usize, usize) {
    let dir = fresh_dir(root, "r1");
    let memory_root = dir.join("memory");
    let project = Id128::generate();
    let session_id = Id128::generate();
    seed_r1_fold(&memory_root, project, session_id);
    let mut session = open_session(
        &dir,
        Some(&memory_root),
        Some(project),
        session_id,
        probe_broker(session_id, &["memory.query"], false),
        vec![],
        None,
    );
    let (run_id, trigger) = setup_run(&mut session);
    let results: Arc<Mutex<Vec<StepResult>>> = Arc::new(Mutex::new(Vec::new()));
    let mut commands: Vec<StepCommand> = R1_QUERIES
        .iter()
        .map(|(q, _)| StepCommand::MemoryQuery { query: (*q).into() })
        .collect();
    commands.push(StepCommand::MemoryQuery {
        query: R2_QUERY.into(),
    });
    commands.push(StepCommand::Finish(TerminalOutcome::CompletedGoal));
    let mut provider = RecordingProvider::new(commands, Arc::clone(&results));
    let outcome = session
        .cognition_loop(run_id, trigger.clone(), &mut provider, |s| {
            s.project_context(run_id, &trigger)
        })
        .unwrap();
    assert_eq!(outcome, TerminalOutcome::CompletedGoal);
    session.close().unwrap();

    let results = results.lock().unwrap();
    let text_to_index: HashMap<&str, usize> = R1_CLAIMS
        .iter()
        .enumerate()
        .map(|(i, c)| (*c, i))
        .collect();
    let mut hits = 0usize;
    let mut recent_returned: Vec<usize> = Vec::new();
    let mut oldest = usize::MAX;
    for (q, (_, expected)) in R1_QUERIES.iter().enumerate() {
        let claims_out = memory_claims(&results[q]);
        if claims_out
            .iter()
            .take(5)
            .any(|c| c["text"].as_str() == Some(R1_CLAIMS[*expected]))
        {
            hits += 1;
        }
        for c in claims_out {
            if let Some(text) = c["text"].as_str()
                && let Some(idx) = text_to_index.get(text)
            {
                oldest = oldest.min(*idx);
                if *idx >= 40 && !recent_returned.contains(idx) {
                    recent_returned.push(*idx);
                }
            }
        }
    }
    let recall = hits as f64 / R1_QUERIES.len() as f64;
    // R2: the query whose best lexical match is the superseded claim 45 must
    // surface the survivor 47 with the supersedes annotation.
    let r2 = memory_claims(&results[R1_QUERIES.len()]);
    let survivor = r2.iter().find(|c| c["text"] == R1_CLAIMS[47]);
    let superseded_absent = !r2.iter().any(|c| c["text"] == R1_CLAIMS[45]);
    let r2_ok = superseded_absent
        && survivor.is_some_and(|s| {
            s["contradictions"]
                .as_array()
                .is_some_and(|arr| {
                    arr.iter().any(|x| {
                        x["text"] == R1_CLAIMS[45] && x["supersedes"] == true
                    })
                })
        });
    println!(
        "[probe R1] queries=20 recall@5={recall:.3} (threshold >= 0.8); \
         [probe R2] survivor_supersedes_annotation={r2_ok}; \
         [probe T2] oldest_returned_index={} recent_from_last_10={}",
        if oldest == usize::MAX { 0 } else { oldest },
        recent_returned.len(),
    );
    (hits, R1_QUERIES.len(), r2_ok, recent_returned.len(), oldest)
}

/// The claims array of a Memory step result (panics on other variants).
fn memory_claims(r: &StepResult) -> Vec<Value> {
    match r {
        StepResult::Memory(v) => v["result"]["claims"]
            .as_array()
            .cloned()
            .unwrap_or_default(),
        other => panic!("expected Memory step result, got {other:?}"),
    }
}

/// T1: re-propose a contradicting claim with `supersedes` in a LATER
/// session; the older claim must vanish from queries and the annotation
/// text must survive.
fn probe_t1(root: &Path) -> (bool, bool) {
    let dir = fresh_dir(root, "t1");
    let memory_root = dir.join("memory");
    let project = Id128::generate();
    let session_id_a = Id128::generate();
    let session_id_b = Id128::generate();
    let old_text = "the parser handles crlf line endings";

    let dir_a = dir.join("a");
    std::fs::create_dir_all(&dir_a).unwrap();
    let mut a = open_session(
        &dir_a,
        Some(&memory_root),
        Some(project),
        session_id_a,
        probe_broker(session_id_a, &["memory.query", "memory.propose"], true),
        vec![],
        None,
    );
    let (run_a, _) = setup_run(&mut a);
    let out1 = propose_claim(
        &mut a,
        run_a,
        session_id_a,
        json!({"kind": "decision", "content": old_text}),
    );
    assert_eq!(out1.result["status"], "approved");
    let old_id = out1.result["claim_id"].as_str().unwrap().to_string();
    a.close().unwrap();

    let dir_b = dir.join("b");
    std::fs::create_dir_all(&dir_b).unwrap();
    let mut b = open_session(
        &dir_b,
        Some(&memory_root),
        Some(project),
        session_id_b,
        probe_broker(session_id_b, &["memory.query", "memory.propose"], true),
        vec![],
        None,
    );
    let (run_b, _) = setup_run(&mut b);
    let out2 = propose_claim(
        &mut b,
        run_b,
        session_id_b,
        json!({
            "kind": "decision",
            "content": "the parser handles lf line endings only",
            "supersedes": old_id,
        }),
    );
    assert_eq!(out2.result["status"], "approved");
    let mut absent = true;
    let mut annotated = false;
    for q in ["parser crlf line endings", "parser line endings"] {
        let res = query_memory(&mut b, run_b, session_id_b, q);
        let claims = res.result["claims"].as_array().cloned().unwrap_or_default();
        absent &= !claims.iter().any(|c| c["text"] == old_text);
        annotated |= claims.iter().any(|c| {
            c["contradictions"].as_array().is_some_and(|arr| {
                arr.iter().any(|x| x["text"] == old_text && x["supersedes"] == true)
            })
        });
    }
    b.close().unwrap();
    println!(
        "[probe T1] old_claim_absent={absent} annotation_text_preserved={annotated}"
    );
    (absent, annotated)
}

/// A1: 10 recurring questions with memory on (seeded fold) vs off (fresh
/// session, same scripted prompts); records whether the projection carried
/// the memory fragment.
fn probe_a1(root: &Path) -> (usize, usize, usize) {
    const QUESTIONS: [(&str, &str); 10] = [
        ("what port does the dev server use", "the dev server listens on port 8787"),
        ("which database does the api use", "the api connects to a postgres database"),
        ("how are secrets stored", "secrets are stored in the vault kv store"),
        ("what is the test command", "tests run with cargo test in the workspace root"),
        ("where do the logs go", "logs stream to stdout in json format"),
        ("what is the deploy cadence", "production deploys happen on friday evenings"),
        ("which http framework is used", "the api is built on axum"),
        ("how is the cache invalidated", "the cache is invalidated by a content hash"),
        ("what lints does ci enforce", "ci enforces clippy with deny warnings"),
        ("how are migrations applied", "migrations run automatically at boot"),
    ];

    let dir_seeded = fresh_dir(root, "a1-seeded");
    let memory_root = dir_seeded.join("memory");
    let project = Id128::generate();
    let session_id = Id128::generate();
    let claims: Vec<Claim> = QUESTIONS
        .iter()
        .map(|(_, content)| {
            make_claim(
                session_id,
                1,
                "decision",
                content,
                MemoryScope::Project(project),
            )
        })
        .collect();
    seed_transition(&memory_root, project, session_id, 1, &claims, &[], b"a1-seed");
    let responses: Vec<CompletionResponse> = QUESTIONS
        .iter()
        .map(|(_, content)| response(content, 5, 5))
        .collect();
    let mut seeded = open_session(
        &dir_seeded,
        Some(&memory_root),
        Some(project),
        session_id,
        probe_broker(session_id, &["memory.query", "memory.propose"], true),
        responses,
        None,
    );
    let mut seeded_with_fragment = 0usize;
    for (q, content) in QUESTIONS.iter() {
        let (_, rendered) = run_wake_capture(
            &mut seeded,
            vec![
                StepCommand::ModelCall(ModelCallSpec {
                    rendered_hash: Digest::new(q.as_bytes()),
                    max_tokens: None,
                }),
                StepCommand::Finish(TerminalOutcome::CompletedGoal),
            ],
        );
        if rendered.contains(content) {
            seeded_with_fragment += 1;
        }
    }
    seeded.close().unwrap();

    let dir_fresh = fresh_dir(root, "a1-fresh");
    let fresh_id = Id128::generate();
    let responses: Vec<CompletionResponse> = QUESTIONS
        .iter()
        .map(|(_, content)| response(content, 5, 5))
        .collect();
    let mut fresh = open_session(
        &dir_fresh,
        None,
        Some(project),
        fresh_id,
        probe_broker(fresh_id, &["memory.query", "memory.propose"], true),
        responses,
        None,
    );
    let mut fresh_with_fragment = 0usize;
    for (q, content) in QUESTIONS.iter() {
        let (_, rendered) = run_wake_capture(
            &mut fresh,
            vec![
                StepCommand::ModelCall(ModelCallSpec {
                    rendered_hash: Digest::new(q.as_bytes()),
                    max_tokens: None,
                }),
                StepCommand::Finish(TerminalOutcome::CompletedGoal),
            ],
        );
        if rendered.contains(content) {
            fresh_with_fragment += 1;
        }
    }
    fresh.close().unwrap();
    println!(
        "[probe A1] questions=10 seeded_with_fragment={seeded_with_fragment} \
         fresh_with_fragment={fresh_with_fragment} (threshold seeded >= 8/10)"
    );
    (seeded_with_fragment, fresh_with_fragment, QUESTIONS.len())
}

/// A2: cache_outcome distribution — a stale projection after a root change
/// invalidates, then a fresh projection hits (the gate_m4 pattern).
fn probe_a2(root: &Path) -> (usize, usize, usize) {
    let dir = fresh_dir(root, "a2");
    let memory_root = dir.join("memory");
    let project = Id128::generate();
    let session_id = Id128::generate();
    let mut session = open_session(
        &dir,
        Some(&memory_root),
        Some(project),
        session_id,
        probe_broker(session_id, &["memory.query", "memory.propose"], true),
        vec![
            response("a", 3, 2),
            response("b", 3, 2),
            response("c", 3, 2),
            response("d", 3, 2),
        ],
        None,
    );
    let (run_id, trigger) = setup_run(&mut session);
    let out0 = propose_claim(
        &mut session,
        run_id,
        session_id,
        json!({"kind": "decision", "content": "the a2 baseline claim"}),
    );
    assert_eq!(out0.result["status"], "approved");
    let ctx = session.project_context(run_id, &trigger).unwrap();
    let p0 = session.projection_state().cloned().unwrap();
    let messages = p0.lowered.clone();
    let selected = ctx.selected_events.clone();
    let rendered = ctx.rendered.clone();
    // Call 1: miss; call 2 (same projection): hit.
    session
        .model_call(run_id, messages.clone(), selected.clone(), &rendered)
        .unwrap();
    session
        .model_call(run_id, messages.clone(), selected.clone(), &rendered)
        .unwrap();
    // A root change makes the stale projection invalid on call 3.
    let out = propose_claim(
        &mut session,
        run_id,
        session_id,
        json!({"kind": "decision", "content": "the a2 cache-busting claim"}),
    );
    assert_eq!(out.result["status"], "approved");
    session
        .model_call(run_id, messages, selected, &rendered)
        .unwrap();
    // A fresh projection against the unchanged session hits again.
    let ctx2 = session.project_context(run_id, &trigger).unwrap();
    let p1 = session.projection_state().cloned().unwrap();
    session
        .model_call(
            run_id,
            p1.lowered.clone(),
            ctx2.selected_events.clone(),
            &ctx2.rendered,
        )
        .unwrap();
    session.close().unwrap();

    let mut hits = 0usize;
    let mut invalidated = 0usize;
    let mut misses = 0usize;
    for env in crate::collect_envelopes(&dir).unwrap() {
        if env.kind == "model_outcome" {
            match env.payload.get("cache_outcome") {
                Some(Value::String(s)) if s == "hit" => hits += 1,
                Some(Value::String(s)) if s == "miss" => misses += 1,
                Some(Value::Object(_)) => invalidated += 1,
                other => panic!("unexpected cache_outcome {other:?}"),
            }
        }
    }
    println!(
        "[probe A2] calls=4 hits={hits} invalidated={invalidated} misses={misses} \
         (threshold hits >= 1 and invalidated >= 1)"
    );
    (hits, invalidated, misses)
}

/// C1: a memory claim short-circuits a re-discovery read. With the claim the
/// scripted agent reads the test file once; without, twice. Counts fs.read
/// tool intents committed after the memory.query intent.
fn probe_c1(root: &Path) -> (usize, usize, bool) {
    const CLAIM_TEXT: &str = "the test file is test_mathlib.py";
    let buggy = "def clamp(x, lo, hi):\n    if x < lo:\n        return lo\n    if x > hi:\n        return hi\n    return lo\n";
    let test = "import unittest\nfrom fixlib import clamp\n\nclass ClampTests(unittest.TestCase):\n    def test_in_range(self):\n        self.assertEqual(clamp(3, 0, 5), 3)\n\nif __name__ == \"__main__\":\n    unittest.main()\n";
    let run_case = |tag: &str, seeded: bool| -> (usize, bool) {
        let dir = fresh_dir(root, tag);
        let repo = dir.join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        std::fs::write(repo.join("fixlib.py"), buggy).unwrap();
        std::fs::write(repo.join("test_mathlib.py"), test).unwrap();
        let memory_root = dir.join("memory");
        let project = Id128::generate();
        let session_id = Id128::generate();
        if seeded {
            let claim = make_claim(
                session_id,
                1,
                "lesson",
                CLAIM_TEXT,
                MemoryScope::Project(project),
            );
            seed_transition(&memory_root, project, session_id, 1, &[claim], &[], b"c1-seed");
        }
        let mut session = Session::open(SessionConfig {
            dir: dir.clone(),
            provider: Some(fake_config("probe")),
            broker: probe_broker(
                session_id,
                &[
                    "fs.read",
                    "fs.patch",
                    "process.exec",
                    "memory.query",
                    "memory.propose",
                ],
                true,
            ),
            session_id: Some(session_id),
            project: Some(project),
            memory_root: Some(memory_root.clone()),
            fs_root: repo.clone(),
            budgets: Budgets {
                deadline_secs: Some(120),
                tokens: Some(100_000),
                tools: Some(200),
                children: Some(8),
            },
            ..Default::default()
        })
        .unwrap();
        let (run_id, trigger) = setup_run(&mut session);
        let results: Arc<Mutex<Vec<StepResult>>> = Arc::new(Mutex::new(Vec::new()));
        let mut commands = vec![
            StepCommand::MemoryQuery {
                query: "test file".into(),
            },
            StepCommand::ToolIntent {
                tool: "fs.read".into(),
                arguments: json!({"path": "test_mathlib.py"}),
            },
        ];
        if !seeded {
            commands.push(StepCommand::ToolIntent {
                tool: "fs.read".into(),
                arguments: json!({"path": "test_mathlib.py"}),
            });
        }
        commands.extend([
            StepCommand::ToolIntent {
                tool: "fs.read".into(),
                arguments: json!({"path": "fixlib.py"}),
            },
            StepCommand::ToolIntent {
                tool: "fs.patch".into(),
                arguments: json!({"path": "fixlib.py", "replacements": [
                    {"old": "        return hi\n    return lo\n", "new": "        return hi\n    return x\n"}
                ]}),
            },
            StepCommand::ToolIntent {
                tool: "process.exec".into(),
                arguments: json!({
                    "argv": [python_path(), "-m", "unittest", "test_mathlib"],
                    "cwd": ".", "env": tool_env(),
                }),
            },
            StepCommand::Finish(TerminalOutcome::CompletedGoal),
        ]);
        let mut provider = RecordingProvider::new(commands, Arc::clone(&results));
        let outcome = session
            .cognition_loop(run_id, trigger.clone(), &mut provider, |s| {
                s.project_context(run_id, &trigger)
            })
            .unwrap();
        assert_eq!(outcome, TerminalOutcome::CompletedGoal);
        session.close().unwrap();
        let results = results.lock().unwrap();
        let claim_returned = results
            .iter()
            .filter_map(|r| match r {
                StepResult::Memory(v) => Some(v.clone()),
                _ => None,
            })
            .any(|v| {
                v["result"]["claims"]
                    .as_array()
                    .is_some_and(|arr| arr.iter().any(|c| c["text"] == CLAIM_TEXT))
            });
        drop(results);
        let mut query_seq = 0u64;
        for env in crate::collect_envelopes(&dir).unwrap() {
            if env.kind == "tool_intent" && env.payload["tool"] == "memory.query" {
                query_seq = env.seq;
            }
        }
        let reads_after = crate::collect_envelopes(&dir)
            .unwrap()
            .iter()
            .filter(|env| {
                env.kind == "tool_intent" && env.payload["tool"] == "fs.read" && env.seq > query_seq
            })
            .count();
        (reads_after, claim_returned)
    };
    let (with_claim, claim_returned) = run_case("c1-with", true);
    let (without_claim, _) = run_case("c1-without", false);
    println!(
        "[probe C1] fs.read_after_query with_claim={with_claim} without_claim={without_claim} \
         claim_returned={claim_returned} (threshold with < without)"
    );
    (with_claim, without_claim, claim_returned)
}

/// C2: a child's attenuated memory.query returns the project claim and the
/// ChildDone wake triggers a scripted parent follow-up.
fn probe_c2(root: &Path) -> (bool, String) {
    let dir = fresh_dir(root, "c2");
    let memory_root = dir.join("memory");
    let project = Id128::generate();
    let session_id = Id128::generate();
    let claim_text = "the widget integration uses the staging api";
    let claim = make_claim(
        session_id,
        1,
        "decision",
        claim_text,
        MemoryScope::Project(project),
    );
    seed_transition(&memory_root, project, session_id, 1, &[claim], &[], b"c2-seed");
    // The follow-up reads a real file from the session dir (the fs_root).
    std::fs::write(dir.join("fixlib.py"), "x = 1\n").unwrap();

    let child_results: Arc<Mutex<Vec<StepResult>>> = Arc::new(Mutex::new(Vec::new()));
    let child_results_after = Arc::clone(&child_results);
    let factory: Box<dyn FnMut() -> Box<dyn CognitionProvider> + Send> =
        Box::new(move || {
            Box::new(RecordingProvider::new(
                vec![
                    StepCommand::MemoryQuery {
                        query: "widget staging api".into(),
                    },
                    StepCommand::Finish(TerminalOutcome::CompletedGoal),
                ],
                Arc::clone(&child_results_after),
            ))
        });
    let mut session = open_session(
        &dir,
        Some(&memory_root),
        Some(project),
        session_id,
        probe_broker(session_id, &["child.spawn", "memory.query", "fs.read"], false),
        vec![],
        Some(factory),
    );
    let (run_id, trigger) = setup_run(&mut session);
    let parent_results: Arc<Mutex<Vec<StepResult>>> = Arc::new(Mutex::new(Vec::new()));
    let mut provider = RecordingProvider::new(
        vec![
            StepCommand::ChildSpawn {
                spec: json!({"prompt": "check the widget api", "budgets": {"tokens": 5000}}),
            },
            StepCommand::Finish(TerminalOutcome::CompletedGoal),
        ],
        Arc::clone(&parent_results),
    );
    let outcome = session
        .cognition_loop(run_id, trigger.clone(), &mut provider, |s: &mut Session| {
            s.project_context(
                run_id,
                &Trigger {
                    kind: TriggerKind::ChildDone,
                    referent: None,
                },
            )
        })
        .unwrap();
    assert_eq!(outcome, TerminalOutcome::CompletedGoal);

    let cresults = child_results.lock().unwrap();
    let child_has_claim = match cresults.first() {
        Some(StepResult::Memory(v)) => v["result"]["claims"]
            .as_array()
            .is_some_and(|arr| arr.iter().any(|c| c["text"] == claim_text)),
        other => panic!("expected child Memory result, got {other:?}"),
    };
    drop(cresults);

    // The ChildDone wake is observable after the parent run ends; the parent
    // answers with a scripted follow-up.
    let wake = session.accept_wake().unwrap().expect("ChildDone wake accepted");
    assert_eq!(wake.trigger.kind, TriggerKind::ChildDone);
    let wake_trigger = wake.trigger.clone();
    let wake_run = wake.run_id;
    session.run_start(wake_run).unwrap();
    let mut follow = ScriptedProvider::new(vec![
        StepCommand::ToolIntent {
            tool: "fs.read".into(),
            arguments: json!({"path": "fixlib.py"}),
        },
        StepCommand::Finish(TerminalOutcome::CompletedGoal),
    ]);
    let follow_outcome = session
        .cognition_loop(wake_run, wake_trigger, &mut follow, |s| {
            s.project_context(wake_run, &wake.trigger)
        })
        .unwrap();
    session.close().unwrap();
    println!(
        "[probe C2] child_query_has_project_claim={child_has_claim} \
         followup_outcome={follow_outcome:?}"
    );
    (child_has_claim, format!("{follow_outcome:?}"))
}

/// L1 + L2: project_context wall clock on a 64-event ring + 50-claim fold
/// (20 runs, p50/p95 — the salience projector is the single configured path)
/// and the memory fragment sizes against the projection budgets.
fn probe_l1_l2(root: &Path) -> (f64, f64, usize, Vec<(String, u64)>) {
    let dir = fresh_dir(root, "latency");
    let memory_root = dir.join("memory");
    let project = Id128::generate();
    let session_id = Id128::generate();
    let claims = seed_r1_fold(&memory_root, project, session_id);
    let mut session = open_session(
        &dir,
        Some(&memory_root),
        Some(project),
        session_id,
        probe_broker(session_id, &["memory.query"], false),
        vec![],
        None,
    );
    for i in 0..64u64 {
        session
            .commit(
                vec![NewEvent {
                    kind: "append_user_message".into(),
                    payload_schema: 1,
                    payload: json!({"text": format!("ring event {i}")}),
                    objects: Vec::new(),
                    refs: Vec::new(),
                }],
                None,
            )
            .unwrap();
    }
    let (run_id, trigger) = setup_run(&mut session);
    let mut timings = Vec::new();
    for _ in 0..20 {
        let t = Instant::now();
        session.project_context(run_id, &trigger).unwrap();
        timings.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    let p50 = percentile(&mut timings.clone(), 0.50);
    let p95 = percentile(&mut timings, 0.95);
    let lowered = session.projection_state().cloned().unwrap().lowered;
    session.close().unwrap();

    // mem.project: the exact fold render the kernel produces (active claims
    // sorted by claim_id text; the two superseded seeds never enter the
    // fragment). mem.lifetime: no lifetime claims seeded → absent.
    let mut sorted: Vec<&Claim> = claims
        .iter()
        .filter(|c| c.content != R1_CLAIMS[45] && c.content != R1_CLAIMS[46])
        .collect();
    sorted.sort_by_key(|c| c.claim_id.to_string());
    let mem_project_text = sorted
        .iter()
        .map(|c| format!("{} | {}\n", c.kind, c.content))
        .collect::<String>();
    let tokens = |text: &str| -> u64 { (text.len() as u64 / 4).max(1) };
    let mut fragments = vec![("mem.lifetime".to_string(), 0u64)];
    let mem_project = lowered
        .iter()
        .find(|m| m.content == mem_project_text)
        .unwrap_or_else(|| panic!("mem.project fragment missing from the projection"));
    fragments.push(("mem.project".to_string(), tokens(&mem_project.content)));
    // ev.memory: the evidence fragment is the only message carrying "score="
    // (tool schemas and event payloads never do).
    let ev_memory = lowered
        .iter()
        .find(|m| m.content.contains("score="))
        .unwrap_or_else(|| panic!("ev.memory fragment missing from the projection"));
    fragments.push(("ev.memory".to_string(), tokens(&ev_memory.content)));
    println!(
        "[probe L1] runs=20 p50={p50:.2}ms p95={p95:.2}ms (threshold p95 < 100ms); \
         [probe L2] fragments={fragments:?} (budgets: stable <= 4096, volatile <= 2048)"
    );
    (p50, p95, 20, fragments)
}

/// G1: 50 sessions over one project (scripted propose + supersede cycles);
/// records claims/edges/retracted and flags retraction growth.
fn probe_g1(root: &Path) -> (usize, usize, usize, bool) {
    let dir = fresh_dir(root, "g1");
    let memory_root = dir.join("memory");
    let project = Id128::generate();
    let mut prev_claim_id: Option<String> = None;
    for i in 0..50usize {
        let session_dir = dir.join(format!("s{i}"));
        std::fs::create_dir_all(&session_dir).unwrap();
        let session_id = Id128::generate();
        let mut session = open_session(
            &session_dir,
            Some(&memory_root),
            Some(project),
            session_id,
            probe_broker(session_id, &["memory.query", "memory.propose"], true),
            vec![],
            None,
        );
        let (run_id, _) = setup_run(&mut session);
        let mut claim = json!({"kind": "decision", "content": format!("the g1 decision number {i}")});
        if i % 5 == 4
            && let Some(target) = &prev_claim_id
        {
            claim["supersedes"] = json!(target);
        }
        let out = propose_claim(&mut session, run_id, session_id, claim);
        assert_eq!(out.result["status"], "approved", "g1 session {i}");
        prev_claim_id = Some(out.result["claim_id"].as_str().unwrap().to_string());
        session.close().unwrap();
    }
    let actor = MemoryRootActor::open(&memory_root, MemoryScope::Project(project)).unwrap();
    let fold = actor.fold(actor.head()).unwrap();
    let (active, edges, retracted) =
        (fold.claims.len(), fold.edges.len(), fold.retracted.len());
    let flag = retracted > 2 * active;
    println!(
        "[probe G1] sessions=50 active={active} edges={edges} retracted={retracted} \
         retraction_flag={flag} (threshold retracted <= 2x active)"
    );
    (active, edges, retracted, flag)
}

/// G2: retrieval reconcile time via the query path at 200/500/1000 claims.
fn probe_g2(root: &Path) -> (f64, f64, f64) {
    let mut out = (0.0, 0.0, 0.0);
    for (slot, size) in [200usize, 500, 1000].iter().enumerate() {
        let dir = fresh_dir(root, &format!("g2-{size}"));
        let memory_root = dir.join("memory");
        let project = Id128::generate();
        let session_id = Id128::generate();
        let claims: Vec<Claim> = (0..*size)
            .map(|i| {
                make_claim(
                    session_id,
                    1,
                    "decision",
                    &format!("the metric sink {i} flushes every {i} seconds"),
                    MemoryScope::Project(project),
                )
            })
            .collect();
        seed_transition(&memory_root, project, session_id, 1, &claims, &[], b"g2-seed");
        let mut session = open_session(
            &dir,
            Some(&memory_root),
            Some(project),
            session_id,
            probe_broker(session_id, &["memory.query"], false),
            vec![],
            None,
        );
        let (run_id, _) = setup_run(&mut session);
        let t = Instant::now();
        let out_come = query_memory(&mut session, run_id, session_id, "metric sink flushes seconds");
        assert!(out_come.error.is_none());
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        session.close().unwrap();
        match slot {
            0 => out.0 = ms,
            1 => out.1 = ms,
            _ => out.2 = ms,
        }
    }
    println!(
        "[probe G2] reconcile_ms 200={:.1} 500={:.1} 1000={:.1} (threshold 1000 < 5000ms)",
        out.0, out.1, out.2
    );
    out
}

// ---------- the W1 mini-fixture cases ----------

struct W1Case {
    buggy: &'static str,
    test: String,
    old: &'static str,
    new: &'static str,
    root_cause: &'static str,
    fix: &'static str,
}

fn w1_cases() -> Vec<W1Case> {
    let test_for = |import: &str, call: &str, expect: &str| {
        format!(
            "import unittest\nfrom fixlib import {import}\n\nclass {import}Tests(unittest.TestCase):\n    def test_case(self):\n        self.assertEqual({call}, {expect})\n\nif __name__ == \"__main__\":\n    unittest.main()\n"
        )
    };
    vec![
        W1Case {
            buggy: "def clamp(x, lo, hi):\n    if x < lo:\n        return lo\n    if x > hi:\n        return hi\n    return lo\n",
            test: test_for("clamp", "clamp(3, 0, 5)", "3"),
            old: "        return hi\n    return lo\n",
            new: "        return hi\n    return x\n",
            root_cause: "clamp returns the lower bound for in-range inputs",
            fix: "clamp returns the input for in-range values",
        },
        W1Case {
            buggy: "def fib(n):\n    if n <= 2:\n        return 1\n    return fib(n - 1) + fib(n - 2)\n",
            test: test_for("fib", "fib(3)", "2"),
            old: "    if n <= 2:\n        return 1\n",
            new: "    if n < 2:\n        return n\n",
            root_cause: "fib treats n=2 as a base case",
            fix: "fib returns n for n below 2 and recurses otherwise",
        },
        W1Case {
            buggy: "def max3(a, b, c):\n    return a\n",
            test: test_for("max3", "max3(1, 5, 3)", "5"),
            old: "    return a\n",
            new: "    return max(a, b, c)\n",
            root_cause: "max3 returns the first argument unconditionally",
            fix: "max3 returns the greatest of the three arguments",
        },
        W1Case {
            buggy: "def sum_list(items):\n    total = 1\n    for x in items:\n        total += x\n    return total\n",
            test: test_for("sum_list", "sum_list([1, 2, 3])", "6"),
            old: "    total = 1\n",
            new: "    total = 0\n",
            root_cause: "sum_list seeds the running total at 1",
            fix: "sum_list seeds the running total at 0",
        },
        W1Case {
            buggy: "def is_palindrome(s):\n    return True\n",
            test: test_for("is_palindrome", "is_palindrome(\"ab\")", "False"),
            old: "    return True\n",
            new: "    return s == s[::-1]\n",
            root_cause: "is_palindrome always returns true",
            fix: "is_palindrome compares the string with its reverse",
        },
        W1Case {
            buggy: "def count_vowels(s):\n    return s.count(\"a\")\n",
            test: test_for("count_vowels", "count_vowels(\"aeiou\")", "5"),
            old: "    return s.count(\"a\")\n",
            new: "    return sum(1 for ch in s.lower() if ch in \"aeiou\")\n",
            root_cause: "count_vowels counts only the letter a",
            fix: "count_vowels counts all five vowels case-insensitively",
        },
        W1Case {
            buggy: "def reverse_str(s):\n    return s\n",
            test: test_for("reverse_str", "reverse_str(\"ab\")", "\"ba\""),
            old: "    return s\n",
            new: "    return s[::-1]\n",
            root_cause: "reverse_str returns the input unchanged",
            fix: "reverse_str returns the reversed input",
        },
        W1Case {
            buggy: "def divisible_by_3(n):\n    return n % 2 == 0\n",
            test: test_for("divisible_by_3", "divisible_by_3(9)", "True"),
            old: "    return n % 2 == 0\n",
            new: "    return n % 3 == 0\n",
            root_cause: "divisible_by_3 tests divisibility by two",
            fix: "divisible_by_3 tests divisibility by three",
        },
        W1Case {
            buggy: "def to_upper(s):\n    return s.lower()\n",
            test: test_for("to_upper", "to_upper(\"ab\")", "\"AB\""),
            old: "    return s.lower()\n",
            new: "    return s.upper()\n",
            root_cause: "to_upper lowercases its input",
            fix: "to_upper uppercases its input",
        },
        W1Case {
            buggy: "def first_char(s):\n    return \"\"\n",
            test: test_for("first_char", "first_char(\"ab\")", "\"a\""),
            old: "    return \"\"\n",
            new: "    return s[0] if s else None\n",
            root_cause: "first_char returns the empty string for every input",
            fix: "first_char returns the first character or none for empty input",
        },
    ]
}

// ---------- the report ----------

#[derive(Debug, Clone)]
pub struct ProbesReport {
    pub w1_proposed: usize,
    pub w1_matched: usize,
    pub w1_precision: f64,
    pub w2_proposed: usize,
    pub w2_approved: usize,
    pub w2_rate: f64,
    pub r1_queries: usize,
    pub r1_hits: usize,
    pub r1_recall_at_5: f64,
    pub r2_survivor_supersedes_annotation: bool,
    pub t1_old_claim_absent: bool,
    pub t1_annotation_preserved: bool,
    pub t2_recent_returned: usize,
    pub t2_oldest_returned_index: usize,
    pub a1_seeded_with_fragment: usize,
    pub a1_fresh_with_fragment: usize,
    pub a1_total: usize,
    pub a2_hits: usize,
    pub a2_invalidated: usize,
    pub a2_misses: usize,
    pub c1_reads_with_claim: usize,
    pub c1_reads_without_claim: usize,
    pub c1_claim_returned: bool,
    pub c2_child_query_has_project_claim: bool,
    pub c2_followup_outcome: String,
    pub l1_runs: usize,
    pub l1_p50_ms: f64,
    pub l1_p95_ms: f64,
    pub l2_fragments: Vec<(String, u64)>,
    pub g1_active_claims: usize,
    pub g1_edges: usize,
    pub g1_retracted: usize,
    pub g1_retraction_flag: bool,
    pub g2_reconcile_200_ms: f64,
    pub g2_reconcile_500_ms: f64,
    pub g2_reconcile_1000_ms: f64,
}

/// The M7-tuned verdict: every probe threshold, folded.
pub fn probes_verdict(r: &ProbesReport) -> bool {
    r.w1_precision >= 0.5
        && r.w2_rate >= 0.9
        && r.r1_recall_at_5 >= 0.8
        && r.r2_survivor_supersedes_annotation
        && r.t1_old_claim_absent
        && r.t1_annotation_preserved
        && r.t2_recent_returned >= 1
        && r.a1_seeded_with_fragment >= 8
        && r.a2_hits >= 1
        && r.a2_invalidated >= 1
        && r.c1_reads_with_claim < r.c1_reads_without_claim
        && r.c2_child_query_has_project_claim
        && r.l1_p95_ms < 100.0
        && r
            .l2_fragments
            .iter()
            .all(|(id, tokens)| *tokens <= if id == "ev.memory" { 2048 } else { 4096 })
        && !r.g1_retraction_flag
        && r.g2_reconcile_1000_ms < 5000.0
}

/// Runs the full probe battery under `root` (all probe dirs are created
/// inside it) and returns the raw numbers.
pub fn run_probes(root: &Path) -> ProbesReport {
    let (w1_proposed, w1_matched) = probe_w1(root);
    let (w2_proposed, w2_approved) = probe_w2(root);
    let (r1_hits, r1_queries, r2_ok, t2_recent, t2_oldest) = probe_r1_r2_t2(root);
    let (t1_absent, t1_annotated) = probe_t1(root);
    let (a1_seeded, a1_fresh, a1_total) = probe_a1(root);
    let (a2_hits, a2_invalidated, a2_misses) = probe_a2(root);
    let (c1_with, c1_without, c1_returned) = probe_c1(root);
    let (c2_ok, c2_followup) = probe_c2(root);
    let (l1_p50, l1_p95, l1_runs, l2_fragments) = probe_l1_l2(root);
    let (g1_active, g1_edges, g1_retracted, g1_flag) = probe_g1(root);
    let (g2_200, g2_500, g2_1000) = probe_g2(root);
    ProbesReport {
        w1_proposed,
        w1_matched,
        w1_precision: w1_matched as f64 / w1_proposed as f64,
        w2_proposed,
        w2_approved,
        w2_rate: w2_approved as f64 / w2_proposed as f64,
        r1_queries,
        r1_hits,
        r1_recall_at_5: r1_hits as f64 / r1_queries as f64,
        r2_survivor_supersedes_annotation: r2_ok,
        t1_old_claim_absent: t1_absent,
        t1_annotation_preserved: t1_annotated,
        t2_recent_returned: t2_recent,
        t2_oldest_returned_index: t2_oldest,
        a1_seeded_with_fragment: a1_seeded,
        a1_fresh_with_fragment: a1_fresh,
        a1_total,
        a2_hits,
        a2_invalidated,
        a2_misses,
        c1_reads_with_claim: c1_with,
        c1_reads_without_claim: c1_without,
        c1_claim_returned: c1_returned,
        c2_child_query_has_project_claim: c2_ok,
        c2_followup_outcome: c2_followup,
        l1_runs,
        l1_p50_ms: l1_p50,
        l1_p95_ms: l1_p95,
        l2_fragments,
        g1_active_claims: g1_active,
        g1_edges,
        g1_retracted,
        g1_retraction_flag: g1_flag,
        g2_reconcile_200_ms: g2_200,
        g2_reconcile_500_ms: g2_500,
        g2_reconcile_1000_ms: g2_1000,
    }
}
