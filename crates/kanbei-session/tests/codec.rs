//! Codec-drift tests (decision 15): malformed load-bearing records must fail
//! loud on recovery instead of silently defaulting.

use std::path::PathBuf;

use kanbei_core::id::{BranchId, Id128};
use kanbei_session::{NewEvent, Session, SessionConfig, SessionError};
use serde_json::json;

fn tempdir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "kanbei-codec-{tag}-{}-{}",
        std::process::id(),
        Id128::generate()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn config(dir: &PathBuf) -> SessionConfig {
    SessionConfig {
        dir: dir.clone(),
        ..Default::default()
    }
}

/// Commit one `branch_transition` record with `payload`, then reopen the
/// session and return the open result.
fn commit_transition_then_reopen(tag: &str, payload: serde_json::Value) -> Result<Session, SessionError> {
    let dir = tempdir(tag);
    {
        let mut session = Session::open(config(&dir)).unwrap();
        session
            .commit(
                vec![NewEvent {
                    kind: "branch_transition".into(),
                    payload_schema: 1,
                    payload,
                    objects: Vec::new(),
                    refs: Vec::new(),
                }],
                None,
            )
            .unwrap();
        session.close().unwrap();
    }
    Session::open(config(&dir))
}

fn well_formed_payload() -> serde_json::Value {
    json!({
        "branch": BranchId::generate().to_string(),
        "frontier_seq": 1,
        "follow": "FollowHead",
        "config_choice": {
            "mode": "inherit",
            "current": null,
            "historical": null,
            "composition": null,
        },
        "quiesce": { "cancelled": [], "ambiguous": [] },
    })
}

#[test]
fn well_formed_branch_transition_reopens() {
    let session = commit_transition_then_reopen("ok", well_formed_payload())
        .expect("well-formed branch_transition must reopen");
    session.close().unwrap();
}

#[test]
fn malformed_follow_fails_open() {
    let mut payload = well_formed_payload();
    payload["follow"] = json!({ "NotAPolicy": true });
    let err = match commit_transition_then_reopen("follow", payload) {
        Ok(_) => panic!("open must fail on a malformed follow policy"),
        Err(e) => e,
    };
    assert!(
        matches!(err, SessionError::CorruptRecord(_)),
        "expected CorruptRecord, got {err:?}"
    );
}

#[test]
fn missing_frontier_seq_fails_open() {
    let mut payload = well_formed_payload();
    payload.as_object_mut().unwrap().remove("frontier_seq");
    let err = match commit_transition_then_reopen("frontier", payload) {
        Ok(_) => panic!("open must fail on a missing frontier_seq"),
        Err(e) => e,
    };
    assert!(
        matches!(err, SessionError::CorruptRecord(_)),
        "expected CorruptRecord, got {err:?}"
    );
}

#[test]
fn malformed_config_choice_fails_open() {
    let mut payload = well_formed_payload();
    payload["config_choice"] = json!({ "mode": 7 });
    let err = match commit_transition_then_reopen("choice", payload) {
        Ok(_) => panic!("open must fail on a malformed config_choice"),
        Err(e) => e,
    };
    assert!(
        matches!(err, SessionError::CorruptRecord(_)),
        "expected CorruptRecord, got {err:?}"
    );
}

#[test]
fn malformed_branch_fails_open() {
    let mut payload = well_formed_payload();
    payload.as_object_mut().unwrap().remove("branch");
    let err = match commit_transition_then_reopen("branch", payload) {
        Ok(_) => panic!("open must fail on a missing branch"),
        Err(e) => e,
    };
    assert!(
        matches!(err, SessionError::CorruptRecord(_)),
        "expected CorruptRecord, got {err:?}"
    );
}

#[test]
fn malformed_from_branch_fails_open() {
    let mut payload = well_formed_payload();
    payload["from_branch"] = json!(123);
    let err = match commit_transition_then_reopen("from", payload) {
        Ok(_) => panic!("open must fail on a malformed from_branch"),
        Err(e) => e,
    };
    assert!(
        matches!(err, SessionError::CorruptRecord(_)),
        "expected CorruptRecord, got {err:?}"
    );
}

#[test]
fn malformed_quiesce_fails_open() {
    let mut payload = well_formed_payload();
    payload["quiesce"] = json!({ "cancelled": 5 });
    let err = match commit_transition_then_reopen("quiesce", payload) {
        Ok(_) => panic!("open must fail on a malformed quiesce"),
        Err(e) => e,
    };
    assert!(
        matches!(err, SessionError::CorruptRecord(_)),
        "expected CorruptRecord, got {err:?}"
    );
}

#[test]
fn wave1_null_fields_reopen() {
    // Wave-1 transitions recorded follow/config_choice/quiesce as null and
    // omitted from_branch; that shape must keep reopening (documented
    // defaults), only *malformed* values fail loud.
    let payload = json!({
        "branch": BranchId::generate().to_string(),
        "frontier_seq": 1,
        "follow": null,
        "config_choice": null,
        "quiesce": null,
    });
    let session = commit_transition_then_reopen("nulls", payload)
        .expect("wave-1 null fields must reopen");
    session.close().unwrap();
}

#[test]
fn malformed_breaker_trip_fails_open() {
    let dir = tempdir("trip");
    {
        let mut session = Session::open(config(&dir)).unwrap();
        session
            .commit(
                vec![NewEvent {
                    kind: "breaker_tripped".into(),
                    payload_schema: 1,
                    payload: json!({ "value": "not-a-number" }),
                    objects: Vec::new(),
                    refs: Vec::new(),
                }],
                None,
            )
            .unwrap();
        session.close().unwrap();
    }
    let err = match Session::open(config(&dir)) {
        Ok(_) => panic!("open must fail on a malformed breaker_tripped"),
        Err(e) => e,
    };
    assert!(
        matches!(err, SessionError::CorruptRecord(_)),
        "expected CorruptRecord, got {err:?}"
    );
}
