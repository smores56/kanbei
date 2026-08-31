//! Driver-layer tests: the wake-to-quiescence loop over real sessions
//! (fake provider engine, temp session dirs). The canonical-record
//! assertions use kanbei-testkit's envelope collector.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use kanbei_driver::Driver;
use kanbei_provider::{
    CompletionRequest, CompletionResponse, FinishReason, FakeEngine, KeySource, ProviderConfig,
    ProviderError, ProviderEngine, ToolCall, Usage,
};
use kanbei_scheduler::TerminalOutcome;
use kanbei_session::{Session, SessionConfig};
use serde_json::json;

static COUNTER: AtomicU64 = AtomicU64::new(0);

struct DirGuard(PathBuf);

impl DirGuard {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "kanbei-driver-test-{}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::Relaxed),
            name
        ));
        std::fs::create_dir_all(&dir).unwrap();
        DirGuard(dir)
    }
}

impl Drop for DirGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn fresh(name: &str) -> (PathBuf, DirGuard) {
    let g = DirGuard::new(name);
    (g.0.clone(), g)
}

fn resp(content: Option<&str>, calls: Vec<ToolCall>, finish: FinishReason) -> CompletionResponse {
    CompletionResponse {
        content: content.map(str::to_string),
        tool_calls: calls,
        finish_reason: finish,
        usage: Usage { input_tokens: 10, output_tokens: 5 },
        discontinuity: None,
        opaque_artifacts: None,
    }
}

fn call(id: &str, name: &str, args: serde_json::Value) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: args,
    }
}

fn fake_cfg() -> ProviderConfig {
    ProviderConfig {
        provider: "fake".into(),
        model: "driver-test".into(),
        base_url: "http://localhost:0/v1".into(),
        key: KeySource::Env("KANBEI_DRIVER_TEST_KEY".into()),
        temperature: None,
        max_tokens: None,
        timeout: Duration::from_secs(5),
    }
}

/// Trait-object wrapper so the test keeps a handle for `push`.
struct SharedFake(Arc<FakeEngine>);

impl ProviderEngine for SharedFake {
    fn complete(
        &self,
        req: &CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        self.0.complete(req)
    }
    fn identity(&self) -> &str {
        self.0.identity()
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

fn driver_in(
    dir: &Path,
    responses: Vec<CompletionResponse>,
) -> (Driver, Arc<FakeEngine>) {
    let fake = Arc::new(FakeEngine::new(fake_cfg(), responses));
    let session = Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        stream: "driver".into(),
        provider_engine: Some(Box::new(SharedFake(Arc::clone(&fake)))),
        fs_root: dir.to_path_buf(),
        // Unattended battery stand-in: auto-approve gated intents.
        approval_resolver: Some(Arc::new(|_p| true)),
        ..Default::default()
    })
    .unwrap();
    (Driver::new(session), fake)
}

#[test]
fn plain_turn_answers_in_one_run() {
    let (dir, _g) = fresh("plain");
    let (mut driver, _fake) =
        driver_in(&dir, vec![resp(Some("hello"), vec![], FinishReason::Stop)]);
    let turn = driver.user_turn("hi there").unwrap();
    assert_eq!(turn.runs, 1);
    assert_eq!(turn.answer.as_deref(), Some("hello"));
    assert_eq!(turn.last_outcome, Some(TerminalOutcome::Progress));

    let envelopes = kanbei_testkit::collect_envelopes(&dir).unwrap();
    let kind_count = |k: &str| envelopes.iter().filter(|e| e.kind == k).count();
    assert_eq!(kind_count("user_message"), 1);
    assert_eq!(kind_count("wake_acceptance"), 1);
    assert_eq!(kind_count("model_outcome"), 1);
    assert_eq!(kind_count("run_outcome"), 1);
    driver.into_session().close().unwrap();
}

#[test]
fn tool_roundtrip_spans_two_wakes_and_commits_facts() {
    let (dir, _g) = fresh("tool");
    std::fs::write(dir.join("notes.txt"), "the file content").unwrap();
    let (mut driver, _fake) = driver_in(
        &dir,
        vec![
            resp(
                None,
                vec![call("c1", "fs.read", json!({ "path": "notes.txt" }))],
                FinishReason::ToolCalls,
            ),
            resp(
                Some("the notes said: the file content"),
                vec![],
                FinishReason::Stop,
            ),
        ],
    );
    let turn = driver.user_turn("read notes.txt").unwrap();
    assert_eq!(turn.runs, 2, "one tool round-trip = one extra wake");
    assert_eq!(
        turn.answer.as_deref(),
        Some("the notes said: the file content")
    );

    let envelopes = kanbei_testkit::collect_envelopes(&dir).unwrap();
    let kind_count = |k: &str| envelopes.iter().filter(|e| e.kind == k).count();
    assert_eq!(kind_count("wake_acceptance"), 2);
    assert_eq!(kind_count("model_outcome"), 2);
    assert_eq!(kind_count("tool_intent"), 1);
    let outcomes: Vec<_> = envelopes
        .iter()
        .filter(|e| e.kind == "tool_outcome")
        .collect();
    assert_eq!(outcomes.len(), 1);
    assert!(
        outcomes[0].payload.get("result").is_some(),
        "tool outcome must carry the read result: {:?}",
        outcomes[0].payload
    );
    assert!(
        outcomes[0].payload.get("error").and_then(|e| e.as_str()).is_none(),
        "fs.read of an existing file must not error: {:?}",
        outcomes[0].payload
    );
    driver.into_session().close().unwrap();
}

#[test]
fn failing_tool_result_continues_turn() {
    let (dir, _g) = fresh("failtool");
    let (mut driver, _fake) = driver_in(
        &dir,
        vec![
            resp(
                None,
                vec![call("c1", "fs.read", json!({ "path": "missing.txt" }))],
                FinishReason::ToolCalls,
            ),
            resp(Some("it was not there"), vec![], FinishReason::Stop),
        ],
    );
    let turn = driver.user_turn("read missing.txt").unwrap();
    assert_eq!(turn.runs, 2);
    assert_eq!(turn.answer.as_deref(), Some("it was not there"));
    driver.into_session().close().unwrap();
}

#[test]
fn consecutive_turns_keep_canonical_history() {
    let (dir, _g) = fresh("consec");
    let (mut driver, fake) = driver_in(&dir, vec![]);
    fake.push(resp(Some("one"), vec![], FinishReason::Stop));
    let t1 = driver.user_turn("first").unwrap();
    assert_eq!(t1.answer.as_deref(), Some("one"));

    fake.push(resp(Some("two"), vec![], FinishReason::Stop));
    let t2 = driver.user_turn("second").unwrap();
    assert_eq!(t2.answer.as_deref(), Some("two"));
    assert_eq!(t2.runs, 1);

    let envelopes = kanbei_testkit::collect_envelopes(&dir).unwrap();
    assert_eq!(
        envelopes.iter().filter(|e| e.kind == "user_message").count(),
        2
    );
    driver.into_session().close().unwrap();
}

#[test]
fn provider_error_releases_the_run_slot() {
    let (dir, _g) = fresh("provfail");
    let (mut driver, fake) = driver_in(&dir, vec![]); // empty queue: rejected
    let err = driver.user_turn("boom").unwrap_err();
    assert!(
        err.to_string().contains("provider error"),
        "expected a surfaced provider error, got: {err}"
    );
    // The run slot must be released: the next turn runs to completion.
    fake.push(resp(Some("recovered"), vec![], FinishReason::Stop));
    let turn = driver.user_turn("again").unwrap();
    assert_eq!(turn.answer.as_deref(), Some("recovered"));
    driver.into_session().close().unwrap();
}
