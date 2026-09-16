//! R-11/R-08: a memory transition write is a state-changing commit and must
//! pin its post-state manifest, so `current_snapshot` advances and a resume
//! re-derives the POST-write snapshot (the manifest pins the advanced memory
//! root) rather than a stale pre-write one.

use kanbei_capabilities::{
    Broker, Capability, Grant, GrantScope, PolicyTemplate, Principal, TrustClass,
};
use kanbei_core::digest::Digest;
use kanbei_core::id::Id128;
use kanbei_scheduler::{Trigger, TriggerKind};
use kanbei_session::{Session, SessionConfig};
use serde_json::json;

fn tempdir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kanbei-memory-pin-{tag}-{}-{}",
        std::process::id(),
        Id128::generate()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A broker granting the memory tools to the session principal, with an
/// approval gate (the kernel's own memory flow parks and the harness resolves).
fn memory_broker(session_id: Id128) -> Broker {
    let call = |t: &str| Capability::new(t.into(), vec!["call".into()]);
    let allow = ["memory.propose", "memory.query", "memory.promote"];
    let mut broker = Broker::new();
    broker
        .add_template(PolicyTemplate {
            trust_class: TrustClass::Builtin,
            allow: allow.iter().map(|t| call(t)).collect(),
            deny: vec![],
            require_approval: allow.iter().map(|t| call(t)).collect(),
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
            purpose: Some("memory pin".into()),
            policy_version: 1,
        };
        grant.grant_digest = grant.derive_digest();
        broker.add_grant(grant).unwrap();
    }
    broker
}

fn open(dir: &std::path::Path, session_id: Id128, project: Id128) -> Session {
    Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        stream: "memory-pin".into(),
        memory_root: Some(dir.join("memory")),
        project: Some(project),
        broker: memory_broker(session_id),
        session_id: Some(session_id),
        ..Default::default()
    })
    .unwrap()
}

fn setup_run(session: &mut Session) -> kanbei_scheduler::RunId {
    session.observe_trigger(Trigger {
        kind: TriggerKind::NewCausalEvent,
        referent: None,
    });
    session.accept_wake().unwrap().expect("wake accepted").run_id
}

/// One memory tool round trip, resolving a parked approval.
fn call_memory(
    session: &mut Session,
    run_id: kanbei_scheduler::RunId,
    session_id: Id128,
    tool: &str,
    args: serde_json::Value,
) -> kanbei_tools::ToolOutcome {
    let principal = Principal {
        session: session_id,
        generation: 0,
        run: Some(0),
    };
    let outcome = session.tool_call(run_id, principal, tool, args).unwrap();
    if outcome.awaiting_approval() {
        let digest = *session
            .pending_approvals()
            .last()
            .expect("a parked approval");
        return session
            .resolve_approval(&digest, true)
            .unwrap()
            .expect("approval resolves");
    }
    session.commit_tool_outcome(&outcome).unwrap();
    outcome
}

#[test]
fn memory_promotion_pins_post_state_manifest() {
    let dir = tempdir("promote");
    let session_id = Id128::generate();
    let project = Id128::generate();
    let mut session = open(&dir, session_id, project);
    let run = setup_run(&mut session);

    let proposed = call_memory(
        &mut session,
        run,
        session_id,
        "memory.propose",
        json!({ "claim": { "kind": "decision", "content": "the widget is canonical" } }),
    );
    assert_eq!(proposed.result["status"], "approved", "{proposed:?}");
    let source_id = proposed.result["claim_id"].as_str().unwrap().to_string();

    let before = session.current_snapshot();
    let promoted = call_memory(
        &mut session,
        run,
        session_id,
        "memory.promote",
        json!({ "claim_id": source_id, "evidence": "held in the project" }),
    );
    assert_eq!(promoted.result["status"], "approved", "{promoted:?}");

    // The backlink is a state-changing commit: it pins the post-write manifest.
    let after = session.current_snapshot();
    assert!(after.is_some(), "a memory transition pins a manifest");
    assert_ne!(after, before, "the post-write manifest advances");

    session.close().unwrap();
    // Resume re-derives the POST-write snapshot, not a stale pre-write one.
    let resumed = open(&dir, session_id, project);
    assert_eq!(
        resumed.current_snapshot(),
        after,
        "resume re-derives the post-write manifest"
    );
    resumed.close().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
