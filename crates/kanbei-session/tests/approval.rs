//! Approval-gate integration (R-16/D-12, R-17/H-05): approval-gated tools
//! park their committed intent instead of self-approving, the user resolves
//! the parked digest (approve dispatches + commits the outcome, deny
//! resolves `Interrupted`), and the bounded queue evicts oldest-first with
//! eviction resolving to "no parked approval" (re-approval is a new intent).

use std::path::{Path, PathBuf};

use kanbei_capabilities::{Broker, Capability, Grant, GrantScope, PolicyTemplate, Principal, TrustClass};
use kanbei_session::{Session, SessionConfig};
use serde_json::json;

use kanbei_tools::OutcomeClassification;

struct DirGuard(PathBuf);

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fresh(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kb-approval-{tag}-{}-{}",
        std::process::id(),
        kanbei_core::id::Id128::generate()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A session whose broker gates `fs.write` behind an approval (allow-read
/// template + a grant that requires approval for writes).
fn gated_session(dir: &Path, bound: usize) -> (Session, Principal) {
    let session_id = kanbei_core::id::Id128::generate();
    let mut broker = Broker::new();
    broker
        .add_template(PolicyTemplate {
            trust_class: TrustClass::Builtin,
            allow: vec![
                Capability::new("fs.read".into(), vec!["call".into()]),
                Capability::new("fs.write".into(), vec!["call".into()]),
            ],
            deny: vec![],
            require_approval: vec![Capability::new("fs.write".into(), vec!["call".into()])],
            version: 1,
            monotonic: true,
        })
        .unwrap();
    let mut grant = Grant {
        grant_digest: kanbei_core::digest::Digest::new(b"placeholder"),
        principal: Principal {
            session: session_id,
            generation: 0,
            run: None,
        },
        module_generation: 0,
        capability: Capability::new("fs.write".into(), vec!["call".into()]),
        scope: GrantScope::Session,
        expiry: None,
        budget: None,
        purpose: Some("approval gate".into()),
        policy_version: 1,
    };
    grant.grant_digest = grant.derive_digest();
    broker.add_grant(grant).unwrap();
    let session = Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        fs_root: dir.to_path_buf(),
        broker,
        session_id: Some(session_id),
        approval_bound: bound,
        ..Default::default()
    })
    .unwrap();
    let principal = Principal {
        session: session_id,
        generation: 0,
        run: Some(0),
    };
    (session, principal)
}

fn start_run(session: &mut Session) -> kanbei_scheduler::RunId {
    session.observe_trigger(kanbei_scheduler::Trigger {
        kind: kanbei_scheduler::TriggerKind::NewCausalEvent,
        referent: None,
    });
    let run = session.accept_wake().unwrap().unwrap();
    session.run_start(run.run_id).unwrap();
    run.run_id
}

#[test]
fn gated_tool_parks_and_never_self_approves() {
    let dir = fresh("park");
    let _guard = DirGuard(dir.clone());
    let (mut session, principal) = gated_session(&dir, 8);
    let run_id = start_run(&mut session);
    let outcome = session
        .tool_call(run_id, principal, "fs.write", json!({"path": "secret.txt", "content": "s"}))
        .unwrap();
    match &outcome.classification {
        OutcomeClassification::Interrupted(reason) => assert!(reason.starts_with("awaiting approval")),
        other => panic!("gated tool must park, got {other:?}"),
    }
    assert!(
        !dir.join("secret.txt").exists(),
        "the gated tool must not execute before approval"
    );
    assert_eq!(session.pending_approvals().len(), 1);
    session.close().unwrap();
}

#[test]
fn approve_dispatches_and_commits_the_outcome() {
    let dir = fresh("approve");
    let _guard = DirGuard(dir.clone());
    let (mut session, principal) = gated_session(&dir, 8);
    let run_id = start_run(&mut session);
    let outcome = session
        .tool_call(run_id, principal, "fs.write", json!({"path": "secret.txt", "content": "approved bytes"}))
        .unwrap();
    assert!(matches!(
        outcome.classification,
        OutcomeClassification::Interrupted(_)
    ));
    let digest = session.pending_approvals()[0];
    let resolved = session.resolve_approval(&digest, true).unwrap().unwrap();
    assert_eq!(resolved.classification, OutcomeClassification::Normal);
    assert_eq!(resolved.call_id, outcome.call_id);
    assert!(dir.join("secret.txt").exists());
    assert!(
        session.pending_approvals().is_empty(),
        "resolution is one-shot"
    );
    // the appended event replays the outcome the user approved
    session.close().unwrap();
}

#[test]
fn deny_resolves_interrupted() {
    let dir = fresh("deny");
    let _guard = DirGuard(dir.clone());
    let (mut session, principal) = gated_session(&dir, 8);
    let run_id = start_run(&mut session);
    session
        .tool_call(run_id, principal, "fs.write", json!({"path": "x.txt", "content": "x"}))
        .unwrap();
    let digest = session.pending_approvals()[0];
    let resolved = session
        .resolve_approval(&digest, false)
        .unwrap()
        .expect("denied approvals still resolve");
    match resolved.classification {
        OutcomeClassification::Interrupted(reason) => assert_eq!(reason, "approval denied by user"),
        other => panic!("denied approval must interrupt, got {other:?}"),
    }
    assert!(!dir.join("x.txt").exists());
    session.close().unwrap();
}

#[test]
fn eviction_resolves_nothing() {
    let dir = fresh("evict");
    let _guard = DirGuard(dir.clone());
    let (mut session, principal) = gated_session(&dir, 1);
    let run_id = start_run(&mut session);
    session
        .tool_call(run_id, principal.clone(), "fs.write", json!({"path": "a.txt", "content": "a"}))
        .unwrap();
    let evicted = session.pending_approvals()[0];
    // the bound is 1: the second gated intent evicts the first
    session
        .tool_call(run_id, principal, "fs.write", json!({"path": "b.txt", "content": "b"}))
        .unwrap();
    assert_eq!(session.pending_approvals().len(), 1);
    assert!(
        session.pending_approvals()[0] != evicted,
        "oldest was evicted"
    );
    let resolved = session
        .resolve_approval(&evicted, true)
        .unwrap();
    assert!(
        resolved.is_none(),
        "an evicted approval resolves to nothing (re-approval is a new intent)"
    );
    session.close().unwrap();
}
