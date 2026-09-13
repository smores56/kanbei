//! Provider streaming + stream-boundary cancellation (decision 13): a user
//! cancel lands inside an in-flight model call, not only between host
//! commands, and the run takes the canonical `Failed(UserCancelled)` path.

#![allow(clippy::result_large_err)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use kanbei_core::digest::Digest;
use kanbei_core::id::Id128;
use kanbei_provider::{
    CompletionResponse, FinishReason, KeySource, ProviderConfig, ProviderEngine, ProviderError,
    Usage,
};
use kanbei_scheduler::{
    Budgets, CognitionProvider, FailureKind, StepCommand, StepContext, StepError, StepResult,
    TerminalOutcome, Trigger, TriggerKind,
};
use kanbei_session::{Session, SessionConfig};

/// An engine that streams scripted fragments and honours the cancel token
/// between fragments — the stream boundary a user cancel lands on.
struct StreamEngine {
    fragments: Vec<String>,
}

impl ProviderEngine for StreamEngine {
    fn complete(
        &self,
        _req: &kanbei_provider::CompletionRequest,
    ) -> Result<CompletionResponse, ProviderError> {
        Ok(self.response())
    }

    fn complete_stream(
        &self,
        _req: &kanbei_provider::CompletionRequest,
        cancel: &AtomicBool,
        on_delta: &mut dyn FnMut(&str),
    ) -> Result<CompletionResponse, ProviderError> {
        for fragment in &self.fragments {
            if cancel.load(Ordering::SeqCst) {
                return Err(ProviderError::Cancelled {
                    provider: self.identity().to_string(),
                });
            }
            on_delta(fragment);
        }
        Ok(self.response())
    }

    fn identity(&self) -> &str {
        "stream-fake"
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

impl StreamEngine {
    fn response(&self) -> CompletionResponse {
        CompletionResponse {
            content: Some(self.fragments.concat()),
            tool_calls: Vec::new(),
            finish_reason: FinishReason::Stop,
            usage: Usage {
                input_tokens: 1,
                output_tokens: 1,
            },
            discontinuity: None,
            opaque_artifacts: None,
        }
    }
}

struct ScriptedProvider {
    commands: std::collections::VecDeque<StepCommand>,
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

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!(
        "kanbei-stream-{tag}-{}-{}",
        std::process::id(),
        Id128::generate()
    ));
    std::fs::create_dir_all(&d).unwrap();
    d
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

fn model_call_plan() -> Vec<StepCommand> {
    vec![
        StepCommand::ModelCall(kanbei_scheduler::ModelCallSpec {
            rendered_hash: Digest::new(b"ctx"),
            max_tokens: None,
        }),
        StepCommand::Finish(TerminalOutcome::CompletedGoal),
    ]
}

fn render(_s: &mut Session) -> Result<StepContext, kanbei_session::SessionError> {
    Ok(StepContext {
        rendered: "hi".into(),
        rendered_hash: Digest::new(b"ctx"),
        selected_events: vec![],
        budget: Budgets::default(),
        projection_digest: None,
        memory_roots: vec![],
    })
}

fn open(
    dir: &std::path::Path,
    fragments: Vec<&str>,
    cancel: Arc<AtomicBool>,
    delta_listener: kanbei_session::DeltaListener,
) -> Session {
    Session::open(SessionConfig {
        dir: dir.to_path_buf(),
        provider: Some(fake_config()),
        provider_engine: Some(Box::new(StreamEngine {
            fragments: fragments.into_iter().map(str::to_owned).collect(),
        })),
        cancel_flag: Some(cancel),
        delta_listener: Some(delta_listener),
        session_id: Some(Id128::generate()),
        budgets: Budgets {
            deadline_secs: Some(60),
            ..Default::default()
        },
        ..Default::default()
    })
    .unwrap()
}

/// A cancel observed mid-stream ends the run with `Failed(UserCancelled)` and
/// frees the run slot; the fragments before the cancel reached the listener.
#[test]
fn stream_cancel_ends_run_as_user_cancelled() {
    let dir = dir("cancel");
    let cancel = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen_writer = seen.clone();
    let cancel_writer = cancel.clone();
    let listener: kanbei_session::DeltaListener = Arc::new(move |fragment: &str| {
        let mut seen = seen_writer.lock().unwrap();
        seen.push(fragment.to_owned());
        // Cancel once the second fragment has been observed: the engine's
        // next per-fragment check trips and the call aborts mid-stream.
        if seen.len() == 2 {
            cancel_writer.store(true, Ordering::SeqCst);
        }
    });
    let mut session = open(&dir, vec!["a", "b", "c", "d"], cancel, listener);
    session.observe_trigger(Trigger {
        kind: TriggerKind::UserMessage,
        referent: None,
    });
    let run = session.accept_wake().unwrap().unwrap();
    session.run_start(run.run_id).unwrap();
    let mut provider = ScriptedProvider {
        commands: model_call_plan().into(),
    };
    let outcome = session
        .cognition_loop(run.run_id, Trigger {
            kind: TriggerKind::UserMessage,
            referent: None,
        }, &mut provider, render)
        .unwrap();
    assert_eq!(outcome, TerminalOutcome::Failed(FailureKind::UserCancelled));
    assert_eq!(*seen.lock().unwrap(), vec!["a".to_string(), "b".to_string()]);
    // The run slot is released: no in-flight run remains to cancel.
    assert!(session.cancel_active_run().unwrap().is_none());
    session.close().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// A cancel is one-shot: the flag the session consumed is cleared, so the
/// following turn runs normally instead of being cancelled too.
#[test]
fn cancel_flag_is_consumed_so_next_turn_runs() {
    let dir = dir("resume");
    let cancel = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen_writer = seen.clone();
    let cancel_writer = cancel.clone();
    let listener: kanbei_session::DeltaListener = Arc::new(move |fragment: &str| {
        let mut seen = seen_writer.lock().unwrap();
        seen.push(fragment.to_owned());
        if seen.len() == 1 {
            cancel_writer.store(true, Ordering::SeqCst);
        }
    });
    let cancel_reader = cancel.clone();
    let mut session = open(&dir, vec!["x", "y"], cancel, listener);
    session.observe_trigger(Trigger {
        kind: TriggerKind::UserMessage,
        referent: None,
    });
    let run = session.accept_wake().unwrap().unwrap();
    session.run_start(run.run_id).unwrap();
    let mut provider = ScriptedProvider {
        commands: model_call_plan().into(),
    };
    let outcome = session
        .cognition_loop(run.run_id, Trigger {
            kind: TriggerKind::UserMessage,
            referent: None,
        }, &mut provider, render)
        .unwrap();
    assert_eq!(outcome, TerminalOutcome::Failed(FailureKind::UserCancelled));
    assert!(
        !cancel_reader.load(Ordering::SeqCst),
        "the consumed cancel flag must be cleared"
    );
    // A fresh turn runs to completion.
    session.observe_trigger(Trigger {
        kind: TriggerKind::UserMessage,
        referent: None,
    });
    let run = session.accept_wake().unwrap().unwrap();
    session.run_start(run.run_id).unwrap();
    let mut provider = ScriptedProvider {
        commands: model_call_plan().into(),
    };
    let outcome = session
        .cognition_loop(run.run_id, Trigger {
            kind: TriggerKind::UserMessage,
            referent: None,
        }, &mut provider, render)
        .unwrap();
    assert_eq!(outcome, TerminalOutcome::CompletedGoal);
    session.close().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}

/// Without a cancel, the streamed response is used whole and the run
/// completes.
#[test]
fn stream_without_cancel_completes() {
    let dir = dir("ok");
    let cancel = Arc::new(AtomicBool::new(false));
    let seen = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));
    let seen_writer = seen.clone();
    let listener: kanbei_session::DeltaListener = Arc::new(move |fragment: &str| {
        seen_writer.lock().unwrap().push(fragment.to_owned());
    });
    let mut session = open(&dir, vec!["he", "llo"], cancel, listener);
    session.observe_trigger(Trigger {
        kind: TriggerKind::UserMessage,
        referent: None,
    });
    let run = session.accept_wake().unwrap().unwrap();
    session.run_start(run.run_id).unwrap();
    let mut provider = ScriptedProvider {
        commands: model_call_plan().into(),
    };
    let outcome = session
        .cognition_loop(run.run_id, Trigger {
            kind: TriggerKind::UserMessage,
            referent: None,
        }, &mut provider, render)
        .unwrap();
    assert_eq!(outcome, TerminalOutcome::CompletedGoal);
    assert_eq!(*seen.lock().unwrap(), vec!["he".to_string(), "llo".to_string()]);
    session.close().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
}
