#![allow(clippy::result_large_err)]

//! M6 gate: historical correction — checkpoints, `continue_from` branching,
//! the path filter, quiesce, config choice, the bundle export (wave 4), and
//! the crash matrix over the six M6 fault points.

use std::path::PathBuf;

use kanbei_capabilities::{Broker, Capability, Grant, GrantScope, PolicyTemplate, Principal, TrustClass};
use kanbei_core::digest::Digest;
use kanbei_core::id::Id128;
use kanbei_memory::MemoryFollowPolicy;
use kanbei_modules::package::{ModuleOrigin, PackageManifest};
use kanbei_provider::{FakeEngine, KeySource, ProviderConfig};
use kanbei_scheduler::{Budgets, Trigger, TriggerKind};
use kanbei_session::{
    CheckpointRef, ExportReport, FaultPoint, NewEvent, Session, SessionConfig, SessionError,
};
use kanbei_snapshot::{ExecutionManifest, manifest_closure};
use kanbei_testkit::{child_acked, collect_envelopes, spawn_m6_crash_child, verify_m6_recovery};
use serde_json::json;

fn fresh_session_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("kb-gate-m6-{name}-{}", std::process::id()));
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

/// A storage-only session (no config/modules — checkpoints and branches need
/// none).
fn open_session(tag: &str) -> (PathBuf, Session) {
    let dir = fresh_session_dir(tag);
    let session = Session::open(SessionConfig {
        dir: dir.clone(),
        stream: format!("m6-{tag}"),
        ..Default::default()
    })
    .unwrap();
    (dir, session)
}

fn user_message(text: &str) -> NewEvent {
    NewEvent {
        kind: "user_message".into(),
        payload_schema: 1,
        payload: json!({"text": text}),
        objects: vec![],
        refs: vec![],
    }
}

fn fake_config() -> ProviderConfig {
    ProviderConfig {
        provider: "fake".into(),
        model: "test".into(),
        base_url: "http://localhost:0/v1".into(),
        key: KeySource::Env("KANBEI_TEST_KEY".into()),
        temperature: None,
        max_tokens: Some(10),
        timeout: std::time::Duration::from_secs(5),
    }
}

/// A session pre-loaded with a fake provider and a broker granting `fs.read`
/// to the session principal (the quiesce test's run setup).
fn spine_session(tag: &str) -> (PathBuf, Session, Id128) {
    let dir = fresh_session_dir(tag);
    let session_id = Id128::generate();
    let mut broker = Broker::new();
    broker
        .add_template(PolicyTemplate {
            trust_class: TrustClass::Builtin,
            allow: vec![Capability::new("fs.read".into(), vec!["call".into()])],
            deny: vec![],
            require_approval: vec![],
            version: 1,
            monotonic: true,
        })
        .unwrap();
    let mut grant = Grant {
        grant_digest: Digest::new(b"placeholder"),
        principal: Principal {
            session: session_id,
            generation: 0,
            run: None,
        },
        module_generation: 0,
        capability: Capability::new("fs.read".into(), vec!["call".into()]),
        scope: GrantScope::Session,
        expiry: None,
        budget: None,
        purpose: Some("m6".into()),
        policy_version: 1,
    };
    grant.grant_digest = grant.derive_digest();
    broker.add_grant(grant).unwrap();
    let session = Session::open(SessionConfig {
        dir: dir.clone(),
        stream: format!("m6-{tag}"),
        provider: Some(fake_config()),
        provider_engine: Some(Box::new(FakeEngine::new(fake_config(), vec![]))),
        broker,
        session_id: Some(session_id),
        budgets: Budgets {
            deadline_secs: Some(60),
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap();
    (dir, session, session_id)
}

fn setup_run(session: &mut Session) -> (kanbei_scheduler::RunId, Trigger) {
    session.observe_trigger(Trigger {
        kind: TriggerKind::NewCausalEvent,
        referent: None,
    });
    let run = session.accept_wake().unwrap().expect("wake accepted");
    session.run_start(run.run_id).unwrap();
    (run.run_id, run.trigger)
}

/// The expected follow policy of a checkpoint: the payload's pinned lifetime
/// root decides between PinnedAt and FollowHead.
fn expected_follow(cp: &kanbei_core::envelope::Envelope) -> MemoryFollowPolicy {
    match cp
        .payload
        .get("memory_root")
        .and_then(|m| m.as_str())
        .and_then(|m| m.parse().ok())
    {
        Some(lifetime_root) => MemoryFollowPolicy::PinnedAt {
            lifetime_root,
            project_root: cp
                .payload
                .get("project_memory_root")
                .and_then(|m| m.as_str())
                .and_then(|m| m.parse().ok()),
        },
        None => MemoryFollowPolicy::FollowHead,
    }
}

/// The closure of the checkpoint's snapshot manifest minus the identity pins
/// (mirror of continue_from) — the store objects that must resolve.
fn checkpoint_closure(session: &Session, cp: &kanbei_core::envelope::Envelope) -> Vec<Digest> {
    let snapshot: Digest = cp.payload["snapshot"].as_str().unwrap().parse().unwrap();
    let manifest: ExecutionManifest =
        serde_json::from_slice(&session.store().get(&snapshot).unwrap()).unwrap();
    let mut closure: Vec<Digest> = manifest_closure(&manifest).into_iter().collect();
    closure.retain(|d| {
        manifest.engine_digest != Some(*d) && manifest.toolchain_digest != Some(*d)
    });
    closure
}

/// 1 — create_checkpoint commits one canonical checkpoint_created event whose
/// payload names the label, the frontier, the pinned snapshot, and the
/// session state, and the receipt's post_snapshot equals the payload pin.
#[test]
fn checkpoint_is_canonical() {
    let (dir, mut session) = open_session("cp-canon");
    let _guard = DirGuard(dir.clone());
    session.commit(vec![user_message("a")], None).unwrap();
    session.commit(vec![user_message("b")], None).unwrap();
    let receipt = session.create_checkpoint(Some("m6-gate".into())).unwrap();
    let envs = collect_envelopes(&dir).unwrap();
    let cp = envs
        .iter()
        .find(|e| e.kind == "checkpoint_created")
        .expect("checkpoint_created committed");
    assert_eq!(cp.payload["label"], json!("m6-gate"));
    assert_eq!(cp.payload["frontier_seq"], json!(cp.seq));
    let snapshot: Digest = cp.payload["snapshot"].as_str().unwrap().parse().unwrap();
    assert!(
        session.store().exists(&snapshot),
        "the pinned snapshot manifest is a store object"
    );
    assert_eq!(receipt.seq, cp.seq);
    assert!(cp.payload.get("composition").and_then(|c| c.as_str()).is_some());
    assert!(cp.payload.get("branch").and_then(|b| b.as_str()).is_some());
    assert!(cp.payload.get("memory_root").is_some());
    assert!(session.on_path(cp.seq), "the checkpoint stays on-path at its frontier");
    session.close().unwrap();
}

/// 2 — continue_from never rewrites history: the envelope set gains exactly
/// the branch_transition; the record carries the new branch/from/frontier/
/// transition, follow, and Current config choice; the path filter and ranges
/// derive the new path; the trajectory excludes the abandoned tail; reopen
/// rebuilds the branch state.
#[test]
fn continue_from_preserves_history_and_derives_path() {
    let (dir, mut session) = open_session("branch");
    let _guard = DirGuard(dir.clone());
    session.commit(vec![user_message("a")], None).unwrap();
    session.commit(vec![user_message("b")], None).unwrap();
    let checkpoint = session.create_checkpoint(None).unwrap();
    // One committed event between the checkpoint and the branch: the
    // abandoned tail (off-path).
    session.commit(vec![user_message("abandoned-tail-msg")], None).unwrap();
    let before = collect_envelopes(&dir).unwrap();
    let root_branch = session.branch();
    let record = session.continue_from(&checkpoint).unwrap();
    let after = collect_envelopes(&dir).unwrap();
    let t = record.transition_seq;

    // (a) exactly the old set + one branch_transition.
    assert_eq!(after.len(), before.len() + 1, "exactly +1 envelope");
    for (b, a) in before.iter().zip(after.iter()) {
        assert_eq!(b, a, "history untouched");
    }
    assert_eq!(after.last().unwrap().kind, "branch_transition");

    // (b) record fields.
    assert_ne!(record.id, root_branch, "new branch id");
    assert_eq!(record.from, Some(root_branch));
    assert_eq!(record.frontier_seq, checkpoint.seq);
    assert_eq!(t, checkpoint.seq + 2, "transition right after the tail");
    let cp = before
        .iter()
        .find(|e| e.kind == "checkpoint_created")
        .unwrap();
    assert_eq!(record.follow, expected_follow(cp));
    assert_eq!(record.config_choice.mode, "Current");
    assert_eq!(session.branch(), record.id);

    // (c) on_path: frontier on, transition off, transition+1 on.
    assert!(session.on_path(checkpoint.seq));
    assert!(!session.on_path(t));
    assert!(session.on_path(t + 1));

    // (d) path_ranges.
    assert_eq!(session.path_ranges(), vec![(1, checkpoint.seq), (t + 1, u64::MAX)]);

    // (e) a new commit lands on-path and the trajectory excludes the tail.
    let receipt = session.commit(vec![user_message("post-branch-msg")], None).unwrap();
    assert_eq!(receipt.last_seq, t + 1);
    assert!(session.on_path(receipt.last_seq));
    let (run_id, trigger) = setup_run(&mut session);
    let ctx = session.project_context(run_id, &trigger).unwrap();
    assert!(
        ctx.rendered.contains("post-branch-msg"),
        "post-branch event projected"
    );
    assert!(
        !ctx.rendered.contains("abandoned-tail-msg"),
        "abandoned tail excluded from the trajectory"
    );

    // (f) reopen rebuilds the branch state from the log.
    session.close().unwrap();
    let reopened = Session::open(SessionConfig {
        dir: dir.clone(),
        stream: "m6-branch-reopen".into(),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(reopened.branch(), record.id);
    assert_eq!(reopened.branch_records(), &[record]);
    assert_eq!(reopened.path_ranges(), vec![(1, checkpoint.seq), (t + 1, u64::MAX)]);
    assert!(reopened.on_path(checkpoint.seq));
    assert!(!reopened.on_path(t));
    reopened.close().unwrap();
}

/// 3 — continue_from quiesces an active run: the run_outcome
/// Failed(Quiesced) is committed, the pending tool intent lands in
/// quiesce.cancelled, and continue_from itself commits no intent_classified
/// facts.
#[test]
fn continue_from_quiesces_active_run() {
    let (dir, mut session, session_id) = spine_session("quiesce");
    let _guard = DirGuard(dir.clone());
    session.commit(vec![user_message("a")], None).unwrap();
    session.commit(vec![user_message("b")], None).unwrap();
    let checkpoint = session.create_checkpoint(None).unwrap();
    let (run_id, _trigger) = setup_run(&mut session);
    let principal = Principal {
        session: session_id,
        generation: 0,
        run: Some(0),
    };
    let outcome = session
        .tool_call(run_id, principal, "fs.read", json!({"path": "x"}))
        .unwrap();
    assert_eq!(
        outcome.classification,
        kanbei_tools::OutcomeClassification::Normal
    );
    // The intent is committed; the outcome is NOT — pending at the branch.
    let record = session.continue_from(&checkpoint).unwrap();

    let envs = collect_envelopes(&dir).unwrap();
    let intent = envs
        .iter()
        .find(|e| e.kind == "tool_intent")
        .expect("tool intent committed");
    let quiesce = envs
        .iter()
        .find(|e| e.kind == "run_outcome")
        .expect("run quiesced");
    assert_eq!(
        quiesce.payload["outcome"]["Failed"],
        json!("Quiesced")
    );
    assert_eq!(quiesce.seq, record.transition_seq - 1, "quiesce before transition");
    assert_eq!(record.quiesce.cancelled.len(), 1);
    assert_eq!(record.quiesce.cancelled[0].id, intent.evt);
    assert_eq!(record.quiesce.cancelled[0].kind, "tool_intent");
    assert_eq!(record.quiesce.cancelled[0].seq, intent.seq);
    assert!(record.quiesce.ambiguous.is_empty());
    assert!(
        envs.iter().all(|e| e.kind != "intent_classified"),
        "continue_from commits no classification facts"
    );
    session.close().unwrap();
}

/// 4 — continue_from rejects invalid checkpoints explicitly: future seq,
/// wrong session id, non-checkpoint event, and a damaged snapshot closure —
/// never appending a branch_transition.
#[test]
fn continue_from_rejects_invalid() {
    let (dir, mut session) = open_session("reject");
    let _guard = DirGuard(dir.clone());
    session.commit(vec![user_message("a")], None).unwrap();
    session.commit(vec![user_message("b")], None).unwrap();
    let checkpoint = session.create_checkpoint(None).unwrap();
    let sid = session.session_id();

    // Future seq.
    let err = session
        .continue_from(&CheckpointRef {
            session_id: sid,
            seq: session.next_seq() + 5,
        })
        .unwrap_err();
    assert!(matches!(err, SessionError::InvalidInput(_)), "future seq: {err}");

    // Wrong session id.
    let err = session
        .continue_from(&CheckpointRef {
            session_id: Id128::generate(),
            seq: checkpoint.seq,
        })
        .unwrap_err();
    assert!(matches!(err, SessionError::InvalidInput(_)), "wrong session: {err}");

    // A committed non-checkpoint event.
    let err = session
        .continue_from(&CheckpointRef { session_id: sid, seq: 2 })
        .unwrap_err();
    assert!(matches!(err, SessionError::InvalidInput(_)), "non-checkpoint: {err}");

    // Damaged closure: delete one store object the checkpoint's snapshot pins.
    let envs = collect_envelopes(&dir).unwrap();
    let cp = envs
        .iter()
        .find(|e| e.kind == "checkpoint_created")
        .unwrap();
    let closure = checkpoint_closure(&session, cp);
    assert!(!closure.is_empty(), "checkpoint pins store objects");
    let victim = closure[0];
    std::fs::remove_file(session.store().path_for(&victim)).unwrap();
    let before = collect_envelopes(&dir).unwrap().len();
    let err = session.continue_from(&checkpoint).unwrap_err();
    assert!(matches!(err, SessionError::Snapshot(_)), "closure: {err}");
    assert_eq!(
        collect_envelopes(&dir).unwrap().len(),
        before,
        "failed continue_from appends nothing"
    );
    session.close().unwrap();
}

/// A config module publishing `svc.<name>` (the crash_child M2 shape).
fn config_source(service: &str) -> String {
    format!(
        r#"function kb_on_activate(ctx)
  ctx.service_publish('{{"scope":[],"name":"{service}"}}', 1, '[]')
end
function kb_hot(x) return {{ from = "{service}", got = x }} end
"#
    )
}

/// 5 — the branch record's config choice: `current` is the live config
/// manifest digest at the branch point, `historical` the checkpoint
/// manifest's provider_config pin. Skips when the guest wasm is not built.
#[test]
fn config_choice_records_current_vs_historical() {
    let dir = fresh_session_dir("config-choice");
    let _guard = DirGuard(dir.clone());
    let mut session = Session::open(SessionConfig {
        dir: dir.clone(),
        stream: "m6-config-choice".into(),
        provider: Some(fake_config()),
        engine: Some(kanbei_vm::VmConfig {
            fuel_per_call: u64::MAX,
            epoch_deadline: u64::MAX,
            ..Default::default()
        }),
        ..Default::default()
    })
    .unwrap();
    if session.modules().is_none() {
        eprintln!("skip: guest wasm not built (kanbei-vm NotBuilt)");
        return;
    }
    session
        .activate_config(PackageManifest {
            schema: 1,
            module_id: Id128::generate(),
            origin: ModuleOrigin::UserConfig,
            trust_class: TrustClass::Builtin,
            scope: kanbei_services::ScopePath(vec![]),
            deps: vec![],
            capabilities: vec![],
            source: config_source("svc_a"),
            state_schema: None,
        })
        .unwrap();
    session.commit(vec![user_message("a")], None).unwrap();
    let checkpoint = session.create_checkpoint(None).unwrap();
    // The config change between checkpoint and branch point.
    session
        .activate_config(PackageManifest {
            schema: 1,
            module_id: Id128::generate(),
            origin: ModuleOrigin::UserConfig,
            trust_class: TrustClass::Builtin,
            scope: kanbei_services::ScopePath(vec![]),
            deps: vec![],
            capabilities: vec![],
            source: config_source("svc_b"),
            state_schema: None,
        })
        .unwrap();
    let record = session.continue_from(&checkpoint).unwrap();
    assert_eq!(record.config_choice.mode, "Current");

    // current == the live config digest (the last composition_changed's
    // package — what activate_config retains as config_digest).
    let envs = collect_envelopes(&dir).unwrap();
    let last_comp = envs
        .iter()
        .rev()
        .find(|e| e.kind == "composition_changed")
        .expect("config activations committed");
    let live: Digest = last_comp.payload["delta"]["added"][0]["package"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();
    assert_eq!(record.config_choice.current, Some(live));

    // historical == the checkpoint manifest's provider_config pin.
    let cp = envs
        .iter()
        .find(|e| e.kind == "checkpoint_created")
        .unwrap();
    let snapshot: Digest = cp.payload["snapshot"].as_str().unwrap().parse().unwrap();
    let manifest: ExecutionManifest =
        serde_json::from_slice(&session.store().get(&snapshot).unwrap()).unwrap();
    assert!(manifest.provider_config.is_some(), "provider config pinned");
    assert_eq!(record.config_choice.historical, manifest.provider_config);
    session.close().unwrap();
}

/// 6 — export_bundle: verified report, populated manifests/ and objects/,
/// closure.json round-trip, and every manifest's closure re-verifies from the
/// exported objects (presence + hash — the export names objects <digest>.bin).
#[test]
fn export_bundle_closure_verifies() {
    let (dir, mut session) = open_session("export");
    let _guard = DirGuard(dir.clone());
    session.commit(vec![user_message("a")], None).unwrap();
    session.commit(vec![user_message("b")], None).unwrap();
    let checkpoint = session.create_checkpoint(None).unwrap();
    session.continue_from(&checkpoint).unwrap();
    session.commit(vec![user_message("post")], None).unwrap();

    let export_dir = fresh_session_dir("export-out");
    let _out_guard = DirGuard(export_dir.clone());
    let report = session.export_bundle(&export_dir).unwrap();
    assert!(report.verified, "missing: {:?}", report.missing);
    assert!(report.manifests >= 2, "genesis + checkpoint manifests");
    assert!(report.objects >= 1);
    assert_eq!(report.envelopes, 5);
    assert!(report.frames >= 1);

    // Plain JSONL log with one line per envelope; raw frame copy preserved.
    let jsonl = std::fs::read_to_string(export_dir.join("session.log.jsonl")).unwrap();
    assert_eq!(jsonl.lines().count() as u64, report.envelopes);
    assert!(export_dir.join("session.log.zst").exists());

    // closure.json round-trips the report.
    let on_disk: ExportReport =
        serde_json::from_slice(&std::fs::read(export_dir.join("closure.json")).unwrap()).unwrap();
    assert_eq!(on_disk, report);

    // Every exported manifest's closure (minus identity pins) resolves from
    // the exported objects by presence and hash.
    let mut manifest_files = 0;
    let mut object_files = 0;
    for entry in std::fs::read_dir(export_dir.join("manifests")).unwrap() {
        let path = entry.unwrap().path();
        let bytes = std::fs::read(&path).unwrap();
        let manifest: ExecutionManifest = serde_json::from_slice(&bytes).unwrap();
        for digest in manifest_closure(&manifest) {
            if manifest.engine_digest == Some(digest)
                || manifest.toolchain_digest == Some(digest)
            {
                continue;
            }
            let obj = std::fs::read(export_dir.join("objects").join(format!("{digest}.bin")))
                .unwrap_or_else(|e| panic!("closure object {digest} missing: {e}"));
            assert_eq!(Digest::new(&obj), digest, "closure object hash mismatch");
        }
        manifest_files += 1;
    }
    for entry in std::fs::read_dir(export_dir.join("objects")).unwrap() {
        let _ = entry.unwrap();
        object_files += 1;
    }
    assert_eq!(manifest_files, report.manifests);
    assert_eq!(object_files, report.objects);
    session.close().unwrap();
}

/// 7 — a deleted store object is reported honestly: verified=false, the
/// digest in missing, closure.json still written, no export object for it.
#[test]
fn export_reports_missing_honestly() {
    let (dir, mut session) = open_session("export-missing");
    let _guard = DirGuard(dir.clone());
    session.commit(vec![user_message("a")], None).unwrap();
    let checkpoint = session.create_checkpoint(None).unwrap();
    let _ = checkpoint;
    let envs = collect_envelopes(&dir).unwrap();
    let cp = envs
        .iter()
        .find(|e| e.kind == "checkpoint_created")
        .unwrap();
    let closure = checkpoint_closure(&session, cp);
    let victim = closure[0];
    std::fs::remove_file(session.store().path_for(&victim)).unwrap();

    let export_dir = fresh_session_dir("export-missing-out");
    let _out_guard = DirGuard(export_dir.clone());
    let report = session.export_bundle(&export_dir).unwrap();
    assert!(!report.verified, "missing object must fail verification");
    assert!(report.missing.contains(&victim), "missing lists {victim}");
    assert!(!export_dir.join("objects").join(format!("{victim}.bin")).exists());
    let on_disk: ExportReport =
        serde_json::from_slice(&std::fs::read(export_dir.join("closure.json")).unwrap()).unwrap();
    assert_eq!(on_disk, report, "partial report still written");
    session.close().unwrap();
}

/// 8 — the crash matrix: every M6 point × Before/After aborts the child and
/// recovery verifies (M1 invariants, torn-tail truncation, branch state
/// rebuild or clean no-branch reopen, idempotent reopens).
#[test]
fn crash_matrix_m6() {
    if std::env::var("KANBEI_SKIP_CRASH").is_ok() {
        eprintln!("skip: KANBEI_SKIP_CRASH set");
        return;
    }
    let points = [
        FaultPoint::BeforeCheckpointCommit,
        FaultPoint::AfterCheckpointCommit,
        FaultPoint::BeforeBranchTransition,
        FaultPoint::AfterBranchTransition,
        FaultPoint::BeforeSessionHeadAdvance,
        FaultPoint::AfterSessionHeadAdvance,
    ];
    for after_acks in [0u64, 2] {
        for point in points {
            let dir = fresh_session_dir(&format!("crash-{point:?}-{after_acks}"));
            let _guard = DirGuard(dir.clone());
            let mut child = spawn_m6_crash_child(&dir, Some(point), after_acks);
            let status = child.wait().unwrap();
            assert!(
                !status.success(),
                "{point:?}/{after_acks}: crash child must abort, got {status:?}"
            );
            let acked = child_acked(&mut child);
            let checks = verify_m6_recovery(&dir, acked, 0)
                .unwrap_or_else(|e| panic!("{point:?}/{after_acks}: {e}"));
            assert!(checks >= 4, "{point:?}/{after_acks}: {checks} checks");
        }
    }
}

/// The m6 driver with no fault point completes cleanly and its log verifies
/// like the crashed variants (full branch flow, no torn tail).
#[test]
fn m6_crash_child_completes_without_point() {
    let dir = fresh_session_dir("m6-nopoint");
    let _guard = DirGuard(dir.clone());
    let mut child = spawn_m6_crash_child(&dir, None, 2);
    let status = child.wait().unwrap();
    assert!(status.success(), "no-point child must complete: {status:?}");
    let acked = child_acked(&mut child);
    assert!(acked >= 5, "full flow acked (2 + checkpoint + transition + commit)");
    let checks = verify_m6_recovery(&dir, acked, 0).unwrap();
    assert!(checks >= 4, "{checks} checks");
}

/// 9 — reopen the branched session, commit a user_message: it lands
/// on-path after the transition, and the trajectory includes it but not the
/// abandoned tail.
#[test]
fn reopen_after_branch_commits_on_path() {
    let (dir, mut session) = open_session("reopen-onpath");
    let _guard = DirGuard(dir.clone());
    session.commit(vec![user_message("a")], None).unwrap();
    session.commit(vec![user_message("b")], None).unwrap();
    let checkpoint = session.create_checkpoint(None).unwrap();
    session.commit(vec![user_message("abandoned-tail-msg")], None).unwrap();
    let record = session.continue_from(&checkpoint).unwrap();
    let t = record.transition_seq;
    session.close().unwrap();

    let mut reopened = Session::open(SessionConfig {
        dir: dir.clone(),
        stream: "m6-reopen-onpath".into(),
        ..Default::default()
    })
    .unwrap();
    assert_eq!(reopened.branch(), record.id);
    assert_eq!(reopened.branch_records(), &[record]);
    let receipt = reopened
        .commit(vec![user_message("post-reopen-msg")], None)
        .unwrap();
    assert!(receipt.last_seq > t);
    assert!(reopened.on_path(receipt.last_seq));
    assert!(!reopened.on_path(t));

    let (run_id, trigger) = setup_run(&mut reopened);
    let ctx = reopened.project_context(run_id, &trigger).unwrap();
    assert!(
        ctx.rendered.contains("post-reopen-msg"),
        "post-reopen event projected"
    );
    assert!(
        !ctx.rendered.contains("abandoned-tail-msg"),
        "abandoned tail excluded from the trajectory"
    );
    reopened.close().unwrap();
}
